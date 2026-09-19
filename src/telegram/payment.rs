use sqlx::SqlitePool;
use teloxide::prelude::*;
use teloxide::types::{ChatId, ParseMode, UserId};
use tokio::time::{Duration as TokioDuration, sleep};
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    db,
    models::PaymentChain,
    payment::{
        ChainPaymentVerifier, PaymentVerificationError, PaymentVerificationOutcome,
        PaymentVerificationRequest, PaymentVerifier,
    },
};

use super::helpers::{html_escape, payment_error_for_user};
use super::keyboards;
use super::types::{EXTERNAL_STEP_MAX_ATTEMPTS, TelegramUiConfig};

pub(crate) struct PaymentTxProcess<'a> {
    pub bot: &'a Bot,
    pub chat_id: ChatId,
    pub user_id: UserId,
    pub config: &'a TelegramUiConfig,
    pub pool: &'a SqlitePool,
    pub order_id: Uuid,
    pub chain: PaymentChain,
    pub amount_usdt: rust_decimal::Decimal,
    pub tx_hash: &'a str,
}

impl<'a> PaymentTxProcess<'a> {
    pub(crate) async fn process(self) -> ResponseResult<()> {
        let PaymentTxProcess {
            bot,
            chat_id,
            user_id,
            config,
            pool,
            order_id,
            chain,
            amount_usdt,
            tx_hash,
        } = self;
        let orders = db::OrderStore::new(pool.clone());
        info!(
            user_id = user_id.0 as i64,
            order_id = %order_id,
            chain = chain.as_str(),
            required_amount_usdt = %amount_usdt,
            "payment tx submitted; starting verification"
        );

        // Check if already paid
        if let Ok(Some(order)) = orders.get(order_id).await
            && (order.status == crate::models::OrderStatus::PaidAndGenerating
                || order.status == crate::models::OrderStatus::Done)
        {
            info!(
                user_id = user_id.0 as i64,
                order_id = %order_id,
                "payment tx ignored because order is already paid"
            );
            return bot
                .send_message(chat_id, "<b>Payment confirmed.</b>\nWebsite is queued.")
                .parse_mode(ParseMode::Html)
                .reply_markup(keyboards::status())
                .await
                .map(|_| ());
        }

        match orders.tx_hash_already_used(chain, tx_hash).await {
            Ok(true) => {
                warn!(
                    user_id = user_id.0 as i64,
                    order_id = %order_id,
                    chain = chain.as_str(),
                    "reused tx rejected"
                );
                return bot
                    .send_message(chat_id, "<b>TX already used.</b>\nSend a new TX.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }
            Ok(false) => {
                info!(
                    user_id = user_id.0 as i64,
                    order_id = %order_id,
                    chain = chain.as_str(),
                    "tx reuse check passed"
                );
            }
            Err(error) => warn!(?error, "failed to check tx reuse"),
        }

        let outcome = if config.skip_payment_verification {
            warn!(
                order_id = %order_id,
                chain = chain.as_str(),
                "SKIP_PAYMENT_VERIFICATION is enabled — accepting tx without on-chain check"
            );
            PaymentVerificationOutcome {
                observed_amount_usdt: amount_usdt,
                observed_to_address: config.payment_address_for_chain(chain).trim().to_string(),
                confirmations: Some(999),
                confirmed_at: chrono::Utc::now(),
            }
        } else {
            let expected_to_address = config.payment_address_for_chain(chain).trim().to_string();
            if expected_to_address.is_empty() {
                return bot
                    .send_message(chat_id, "<b>Payment address missing.</b>\nContact admin.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }

            let verifier = ChainPaymentVerifier::new(config.shared_config());
            let tx_submitted_at = chrono::Utc::now();

            // Reconstruct payment window from order
            let order = orders.get(order_id).await.ok().flatten();
            let payment_expires_at = order
                .as_ref()
                .and_then(|o| o.payment_expires_at)
                .unwrap_or_else(|| {
                    warn!(order_id = %order_id, "payment_expires_at is NULL, using now + window");
                    chrono::Utc::now() + chrono::Duration::minutes(db::PAYMENT_WINDOW_MINUTES)
                });
            let payment_started_at =
                payment_expires_at - chrono::Duration::minutes(db::PAYMENT_WINDOW_MINUTES);

            let verify_request = PaymentVerificationRequest {
                order_id,
                chain,
                tx_hash: tx_hash.to_string(),
                expected_to_address,
                required_amount_usdt: amount_usdt,
                payment_started_at,
                submitted_at: tx_submitted_at,
                expires_at: payment_expires_at,
            };
            let verification = verify_payment_with_retries(&verifier, verify_request).await;

            match verification {
                Ok(outcome) => {
                    info!(
                        user_id = user_id.0 as i64,
                        order_id = %order_id,
                        chain = chain.as_str(),
                        observed_amount_usdt = %outcome.observed_amount_usdt,
                        confirmations = outcome.confirmations.map(|value| value as i64),
                        confirmed_at = %outcome.confirmed_at,
                        "payment verified on chain"
                    );
                    outcome
                }
                Err(error) => {
                    warn!(
                        user_id = user_id.0 as i64,
                        order_id = %order_id,
                        chain = chain.as_str(),
                        error = %error,
                        "payment verification failed"
                    );
                    return bot
                        .send_message(
                            chat_id,
                            format!(
                                "❌ <b>Payment not confirmed.</b>\n{}",
                                html_escape(&payment_error_for_user(&error))
                            ),
                        )
                        .parse_mode(ParseMode::Html)
                        .await
                        .map(|_| ());
                }
            }
        };

        // Mark order as paid — this transitions to paid_and_generating.
        // The queue worker will pick it up automatically.
        if let Err(error) = orders.mark_paid(order_id, chain, tx_hash).await {
            warn!(?error, order_id = %order_id, "failed to mark order paid");
            return bot
                .send_message(chat_id, "⚠️ <b>Payment record failed.</b>\nContact admin.")
                .parse_mode(ParseMode::Html)
                .await
                .map(|_| ());
        }
        info!(
            user_id = user_id.0 as i64,
            order_id = %order_id,
            chain = chain.as_str(),
            observed_amount_usdt = %outcome.observed_amount_usdt,
            "payment recorded and order marked paid"
        );

        // Bind domain to Cloudflare: create zone + set nameservers.
        // This runs early so the zone can propagate while the site is being generated.
        let domain = orders
            .get(order_id)
            .await
            .ok()
            .flatten()
            .and_then(|o| o.requested_domain);
        if let (Some(domain), Some(base_url), Some(api_key)) = (
            &domain,
            config.api_service_base_url.as_deref(),
            config.api_service_api_key.as_deref(),
        ) {
            let deployer = crate::api_client::HttpClientPagesDeployer::new(
                base_url.to_string(),
                api_key.to_string(),
            );
            info!(domain = %domain, order_id = %order_id, "creating cloudflare zone for domain");
            match deployer.create_zone(domain).await {
                Ok(zone) => {
                    info!(
                        domain = %domain,
                        zone_id = %zone.zone_id,
                        nameservers = ?zone.nameservers,
                        "zone created/found, setting nameservers"
                    );
                    if let Err(e) = deployer.set_nameservers(domain, &zone.nameservers).await {
                        warn!(domain = %domain, error = %e, "failed to set nameservers");
                        let _ = bot.send_message(
                            chat_id,
                            "⚠️ <b>Domain nameserver setup failed.</b>\nYour site will still be generated. Contact admin to fix domain binding.",
                        )
                        .parse_mode(ParseMode::Html)
                        .await;
                    } else {
                        info!(domain = %domain, "nameservers set successfully");
                    }
                }
                Err(e) => {
                    warn!(domain = %domain, error = %e, "failed to create cloudflare zone");
                    let _ = bot.send_message(
                        chat_id,
                        "⚠️ <b>Domain zone setup failed.</b>\nYour site will still be generated. Contact admin to fix domain binding.",
                    )
                    .parse_mode(ParseMode::Html)
                    .await;
                }
            }
        }

        bot.send_message(
            chat_id,
            format!(
                "✅ <b>Payment confirmed.</b>\nPaid: {} USDT. Confirmations: {}.\nWebsite queued.",
                outcome.observed_amount_usdt,
                outcome
                    .confirmations
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "confirmed".to_string())
            ),
        )
        .parse_mode(ParseMode::Html)
        .reply_markup(keyboards::status())
        .await
        .map(|_| ())
    }
}

pub(crate) async fn verify_payment_with_retries(
    verifier: &dyn PaymentVerifier,
    request: PaymentVerificationRequest,
) -> Result<PaymentVerificationOutcome, PaymentVerificationError> {
    let mut last_error = None;
    for attempt in 1..=EXTERNAL_STEP_MAX_ATTEMPTS {
        info!(
            order_id = %request.order_id,
            chain = request.chain.as_str(),
            attempt,
            "checking payment transaction"
        );
        match verifier.verify_tx(&request).await {
            Ok(outcome) => {
                info!(
                    order_id = %request.order_id,
                    chain = request.chain.as_str(),
                    attempt,
                    "payment transaction check succeeded"
                );
                return Ok(outcome);
            }
            Err(error)
                if retryable_payment_error(&error) && attempt < EXTERNAL_STEP_MAX_ATTEMPTS =>
            {
                warn!(
                    order_id = %request.order_id,
                    chain = request.chain.as_str(),
                    attempt,
                    error = %error,
                    "payment transaction check failed; retrying"
                );
                last_error = Some(error);
                sleep(TokioDuration::from_secs(attempt as u64 * 2)).await;
            }
            Err(error) => {
                warn!(
                    order_id = %request.order_id,
                    chain = request.chain.as_str(),
                    attempt,
                    error = %error,
                    "payment transaction check failed"
                );
                return Err(error);
            }
        }
    }
    Err(last_error.unwrap_or(PaymentVerificationError::Provider(
        "payment verification failed after retries".to_string(),
    )))
}

fn retryable_payment_error(error: &PaymentVerificationError) -> bool {
    matches!(error, PaymentVerificationError::Provider(_))
}
