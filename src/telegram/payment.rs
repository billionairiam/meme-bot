use std::path::Path;

use sqlx::SqlitePool;
use teloxide::prelude::*;
use teloxide::types::{ChatId, ParseMode, UserId};
use tokio::time::{Duration as TokioDuration, sleep};
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    db,
    payment::{
        ChainPaymentVerifier, PaymentService, PaymentVerificationError, PaymentVerificationOutcome,
        PaymentVerificationRequest, PaymentVerifier,
    },
};

use super::domain_flow::maybe_purchase_domain_after_tx;
use super::helpers::{
    download_telegram_logo, get_session, html_escape, payment_address_for_chain,
    payment_error_for_user, set_session,
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
    pub context: db::PaymentOrderContext,
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
        context,
        tx_hash,
    } = request;
    let order_id = context.order_id;
    let chain = context.payment_chain;
    let mut status_parts: Vec<String> = vec![];
    info!(
        user_id = user_id.0 as i64,
        order_id = %order_id,
        chain = chain.as_str(),
        required_amount_usdt = %context.amount_usdt,
        "payment tx submitted; starting verification"
    );

    if context.payment_status == "paid" {
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
            observed_amount_usdt: context.amount_usdt,
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
        let payment_expires_at = context.payment_expires_at.unwrap_or_else(|| {
            warn!(order_id = %order_id, "payment_expires_at is NULL, falling back to created_at + payment window");
            context.created_at + chrono::Duration::minutes(db::PAYMENT_WINDOW_MINUTES)
        });
        let payment_started_at = context.payment_started_at.unwrap_or_else(|| {
            warn!(order_id = %order_id, "payment_started_at is NULL, falling back from payment_expires_at");
            payment_expires_at - chrono::Duration::minutes(db::PAYMENT_WINDOW_MINUTES)
        });
        let verify_request = PaymentVerificationRequest {
            order_id,
            chain,
            tx_hash: tx_hash.to_string(),
            expected_to_address,
            required_amount_usdt: context.amount_usdt,
            payment_started_at,
            submitted_at: tx_submitted_at,
            expires_at: payment_expires_at,
        };
        let verification = verify_payment_with_retries(&service, verify_request).await;
        if let Err(error) = db::record_payment_attempt(
            pool,
            order_id,
            context.user_id,
            chain,
            tx_hash,
            verification.as_ref(),
        )
        .await
        {
            warn!(?error, order_id = %order_id, "failed to record payment attempt");
        }

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

    if let Err(error) =
        db::confirm_payment_and_mark_paid(pool, order_id, chain, tx_hash, &outcome).await
    {
        warn!(?error, order_id = %order_id, "failed to confirm payment transaction");
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
        "payment recorded and order marked paid"
    );
    status_parts.push(format!(
        "Paid: {} USDT. Confirmations: {}.",
        outcome.observed_amount_usdt,
        outcome
            .confirmations
            .map(|value| value.to_string())
            .unwrap_or_else(|| "confirmed".to_string())
    ));

    match (context.website_id, context.website_input) {
        (Some(website_id), Some(mut input)) => {
            if !input.logo_asset_path.trim().is_empty()
                && !Path::new(&input.logo_asset_path).exists()
            {
                info!(
                    user_id = user_id.0 as i64,
                    order_id = %order_id,
                    website_id = %website_id,
                    "downloading telegram logo before queueing generation"
                );
                match download_telegram_logo(bot, &input.logo_asset_path, &order_id.to_string())
                    .await
                {
                    Ok(path) => {
                        info!(
                            user_id = user_id.0 as i64,
                            order_id = %order_id,
                            website_id = %website_id,
                            logo_path = %path,
                            "telegram logo downloaded"
                        );
                        if let Err(error) =
                            db::update_website_logo_path(pool, website_id, &path).await
                        {
                            warn!(?error, website_id = %website_id, "failed to persist downloaded logo path");
                        }
                        input.logo_asset_path = path;
                    }
                    Err(error) => {
                        warn!(?error, order_id = %order_id, "failed to download logo; keeping stored logo reference");
                    }
                }
            }

            let (domain_ready, purchase_message) = maybe_purchase_domain_after_tx(
                config,
                pool,
                context.domain_quote_id,
                input.requested_domain.as_deref(),
            )
            .await;
            if !purchase_message.is_empty() {
                status_parts.push(purchase_message.trim().to_string());
            }
            if !domain_ready {
                warn!(
                    user_id = user_id.0 as i64,
                    order_id = %order_id,
                    website_id = %website_id,
                    "domain not ready; order moved toward manual review"
                );
                if let Err(error) = db::mark_order_manual_review(pool, order_id).await {
                    warn!(?error, order_id = %order_id, "failed to mark manual review");
                }
                status_parts.push("Manual review required. Contact admin.".to_string());
            } else if let Err(error) =
                enqueue_generation_job_with_retries(pool, order_id, website_id).await
            {
                warn!(?error, order_id = %order_id, "failed to enqueue generation job after retries");
                let _ = db::mark_order_manual_review(pool, order_id).await;
                status_parts.push("⚠️ Failed to queue generation. Contact admin.".to_string());
            } else {
                info!(
                    user_id = user_id.0 as i64,
                    order_id = %order_id,
                    website_id = %website_id,
                    "generation job queued"
                );
                status_parts.push("Website queued.".to_string());
            }
        }
        _ => {
            warn!(order_id = %order_id, "paid order is missing persisted website input");
            let _ = db::mark_order_manual_review(pool, order_id).await;
            status_parts.push("⚠️ Website input missing. Contact admin.".to_string());
        }
    }

    if let Some(mut draft) = get_session(user_id, sessions) {
        draft.step = DraftStep::PaymentConfirmed;
        set_session(user_id, draft, sessions);
    }

    bot.send_message(
        chat_id,
        format!("✅ <b>Payment confirmed.</b>\n{}", status_parts.join("\n")),
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

async fn enqueue_generation_job_with_retries(
    pool: &SqlitePool,
    order_id: Uuid,
    website_id: Uuid,
) -> anyhow::Result<Uuid> {
    let mut last_error = None;
    for attempt in 1..=EXTERNAL_STEP_MAX_ATTEMPTS {
        match db::enqueue_generation_job(pool, order_id, website_id).await {
            Ok(id) => return Ok(id),
            Err(error) if attempt < EXTERNAL_STEP_MAX_ATTEMPTS => {
                warn!(?error, order_id = %order_id, attempt, "failed to enqueue generation job; retrying");
                last_error = Some(error);
                sleep(TokioDuration::from_secs(attempt as u64)).await;
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error
        .unwrap_or_else(|| anyhow::anyhow!("enqueue generation job failed after retries")))
}
