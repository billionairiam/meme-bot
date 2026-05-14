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
        ChainPaymentVerifier, PaymentService, PaymentVerificationError, PaymentVerificationOutcome,
        PaymentVerificationRequest, PaymentVerifier,
    },
};

use super::helpers::{
    get_session, html_escape, payment_address_for_chain, payment_error_for_user, set_session,
};
use super::keyboards::status_keyboard;
use super::types::{DraftStep, EXTERNAL_STEP_MAX_ATTEMPTS, Sessions, TelegramUiConfig};

pub(crate) struct PaymentTxProcess<'a> {
    pub bot: &'a Bot,
    pub chat_id: ChatId,
    pub user_id: UserId,
    pub config: &'a TelegramUiConfig,
    pub sessions: &'a Sessions,
    pub pool: &'a SqlitePool,
    pub order_id: Uuid,
    pub chain: PaymentChain,
    pub amount_usdt: rust_decimal::Decimal,
    pub tx_hash: &'a str,
}

pub(crate) async fn process_payment_tx_context(
    request: PaymentTxProcess<'_>,
) -> ResponseResult<()> {
    let PaymentTxProcess {
        bot,
        chat_id,
        user_id,
        config,
        sessions,
        pool,
        order_id,
        chain,
        amount_usdt,
        tx_hash,
    } = request;
    info!(
        user_id = user_id.0 as i64,
        order_id = %order_id,
        chain = chain.as_str(),
        required_amount_usdt = %amount_usdt,
        "payment tx submitted; starting verification"
    );

    // Check if already paid
    if let Ok(Some(order)) = db::get_order(pool, order_id).await
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
            .reply_markup(status_keyboard())
            .await
            .map(|_| ());
    }

    match db::tx_hash_already_used(pool, chain, tx_hash).await {
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
            observed_to_address: payment_address_for_chain(config, chain).trim().to_string(),
            confirmations: Some(999),
            confirmed_at: chrono::Utc::now(),
        }
    } else {
        let expected_to_address = payment_address_for_chain(config, chain).trim().to_string();
        if expected_to_address.is_empty() {
            return bot
                .send_message(chat_id, "<b>Payment address missing.</b>\nContact admin.")
                .parse_mode(ParseMode::Html)
                .await
                .map(|_| ());
        }

        let verifier = ChainPaymentVerifier::new(
            config.usdt_erc20_rpc_url.clone(),
            config.usdt_erc20_contract.clone(),
            config.usdt_erc20_min_confirmations,
            config.tron_grid_base_url.clone(),
            config.tron_grid_api_key.clone(),
            config.usdt_tron_contract.clone(),
            config.usdt_tron_min_confirmations,
            config.solana_rpc_url.clone(),
            config.usdt_sol_mint.clone(),
            config.usdt_sol_min_confirmations,
        );
        let service = PaymentService::new(verifier);
        let tx_submitted_at = chrono::Utc::now();

        // Reconstruct payment window from order
        let order = db::get_order(pool, order_id).await.ok().flatten();
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
        let verification = verify_payment_with_retries(&service, verify_request).await;

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
    if let Err(error) = db::mark_order_paid(pool, order_id, chain, tx_hash).await {
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

    if let Some(mut draft) = get_session(user_id, sessions) {
        draft.step = DraftStep::PaymentConfirmed;
        set_session(user_id, draft, sessions);
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
    .reply_markup(status_keyboard())
    .await
    .map(|_| ())
}

async fn verify_payment_with_retries<V>(
    service: &PaymentService<V>,
    request: PaymentVerificationRequest,
) -> Result<PaymentVerificationOutcome, PaymentVerificationError>
where
    V: PaymentVerifier,
{
    let mut last_error = None;
    for attempt in 1..=EXTERNAL_STEP_MAX_ATTEMPTS {
        info!(
            order_id = %request.order_id,
            chain = request.chain.as_str(),
            attempt,
            "checking payment transaction"
        );
        match service.verify_tx(request.clone()).await {
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
