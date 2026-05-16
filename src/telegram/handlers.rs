use std::sync::Arc;

use sqlx::SqlitePool;
use teloxide::prelude::*;
use teloxide::types::{CallbackQuery, ChatId, Message, ParseMode, Recipient, UserId};
use tracing::{info, warn};

use crate::{
    api_client::HttpClientDomainRegistrar,
    db,
    domain::DomainRegistrar,
    models::{BuildMode, Order, OrderStatus, PaymentChain},
    validation::{
        validate_contract_address, validate_domain, validate_telegram_url, validate_x_url,
    },
};

use super::domain_flow::{check_alternative_domains, quote_domain_with_retries};
use super::helpers::*;
use super::keyboards::*;
use super::payment::{PaymentTxProcess, process_payment_tx_context};
use super::screens::*;
use super::types::*;

pub(crate) async fn handle_message(
    bot: Bot,
    msg: Message,
    config: Arc<TelegramUiConfig>,
    pool: SqlitePool,
) -> ResponseResult<()> {
    if !ensure_group_member(&bot, &msg, &config).await? {
        return Ok(());
    }

    let Some(user) = msg.from.as_ref() else {
        info!(
            chat_id = %msg.chat.id,
            message_kind = message_kind(&msg),
            "telegram message without user"
        );
        return show_main_menu(&bot, msg.chat.id, &config).await;
    };
    let user_id = user.id;
    let current_order = db::get_unpaid_order(&pool, user_id.0 as i64)
        .await
        .ok()
        .flatten();
    let current_step = current_order.as_ref().map(|o| o.status.as_str());
    info!(
        user_id = user_id.0 as i64,
        chat_id = %msg.chat.id,
        message_kind = message_kind(&msg),
        session_step = current_step.unwrap_or("none"),
        "telegram message received"
    );
    expire_stale_orders_for_user(&pool, user_id).await;

    if let Some(text) = msg.text() {
        if let Some(template_id) = parse_template_deep_link(text) {
            return show_template_confirmation(&bot, msg.chat.id, template_id, &config).await;
        }

        match text.trim() {
            "/start" | "/help" => return show_main_menu(&bot, msg.chat.id, &config).await,
            "/create" => {
                return show_create_menu(
                    &bot,
                    msg.chat.id,
                    &config.template_prices_usdt,
                    config.ai_creation_price_usdt,
                )
                .await;
            }
            "/status" => return show_status(&bot, msg.chat.id, user_id, &pool).await,
            "/history" => return show_history(&bot, msg.chat.id, user_id, &pool).await,
            "/continue" => {
                return handle_continue_command(&bot, msg.chat.id, user_id, &pool, &config).await;
            }
            "/cancel" => {
                cancel_user_order(&pool, user_id).await;
                return bot
                    .send_message(msg.chat.id, "<b>Order cancelled.</b>")
                    .parse_mode(ParseMode::Html)
                    .reply_markup(main_menu_keyboard())
                    .await
                    .map(|_| ());
            }
            _ => {}
        }
    }

    handle_order_step(bot, msg, user_id, config, &pool).await
}

pub(crate) async fn handle_callback(
    bot: Bot,
    query: CallbackQuery,
    config: Arc<TelegramUiConfig>,
    pool: SqlitePool,
) -> ResponseResult<()> {
    bot.answer_callback_query(query.id.clone()).await?;

    let Some(message) = query.message.as_ref() else {
        return Ok(());
    };
    let chat_id = message.chat().id;
    let target = UiMessage {
        chat_id,
        message_id: message.id(),
    };
    let current_step = {
        let order = db::get_unpaid_order(&pool, query.from.id.0 as i64)
            .await
            .ok()
            .flatten();
        order.map(|o| o.status.as_str().to_string())
    };
    info!(
        user_id = query.from.id.0 as i64,
        chat_id = %chat_id,
        callback = query.data.as_deref().unwrap_or("<none>"),
        session_step = current_step.as_deref().unwrap_or("none"),
        "telegram callback received"
    );

    if !ensure_group_member_for_user(&bot, chat_id, query.from.id, &config).await? {
        return Ok(());
    }
    expire_stale_orders_for_user(&pool, query.from.id).await;

    match query.data.as_deref() {
        Some("main") => edit_main_menu(&bot, target, &config).await,
        Some("create") => {
            edit_create_menu(
                &bot,
                target,
                &config.template_prices_usdt,
                config.ai_creation_price_usdt,
            )
            .await
        }
        Some("template_mode") => edit_template_mode(&bot, target, &config).await,
        Some("ai_mode") => {
            clear_screen_keyboard(&bot, target).await?;
            match start_new_order(
                &pool,
                query.from.id,
                query.from.username.as_deref(),
                Some(query.from.first_name.as_str()),
                query.from.last_name.as_deref(),
                BuildMode::AiFree,
                None,
                config.ai_creation_price_usdt,
            )
            .await
            {
                Ok(order_id) => {
                    info!(
                        user_id = query.from.id.0 as i64,
                        order_id = %order_id,
                        build_mode = "ai_free",
                        "new order created"
                    );
                    edit_ask_logo(&bot, target).await
                }
                Err(error) => {
                    let msg = if format!("{error:#}").contains("pending order") {
                        warn!(
                            ?error,
                            user_id = query.from.id.0 as i64,
                            "blocked ai order: pending order exists"
                        );
                        "⚠️ <b>Unfinished order.</b>\nUse /continue or /cancel first."
                    } else {
                        warn!(
                            ?error,
                            user_id = query.from.id.0 as i64,
                            "failed to start ai order"
                        );
                        "<b>Start failed.</b>\nPlease try again."
                    };
                    edit_screen(&bot, target, msg, main_menu_keyboard()).await
                }
            }
        }
        Some("pay_menu") => edit_payment_chain_menu(&bot, target).await,
        Some("status") => edit_status(&bot, target, query.from.id, &pool).await,
        Some("history") => edit_history(&bot, target, query.from.id, &pool).await,
        Some("contact_admin") => edit_contact_admin(&bot, target, &config).await,
        Some("pay_erc20") => {
            edit_payment_chain_and_ask_tx(
                &bot,
                target,
                query.from.id,
                PaymentChain::Erc20,
                &config.usdt_erc20_address,
                &pool,
            )
            .await
        }
        Some("pay_tron") => {
            edit_payment_chain_and_ask_tx(
                &bot,
                target,
                query.from.id,
                PaymentChain::Tron,
                &config.usdt_tron_address,
                &pool,
            )
            .await
        }
        Some("pay_sol") => {
            edit_payment_chain_and_ask_tx(
                &bot,
                target,
                query.from.id,
                PaymentChain::Sol,
                &config.usdt_sol_address,
                &pool,
            )
            .await
        }
        Some(data) if data.starts_with("select_tpl:") => {
            let template_id = data.trim_start_matches("select_tpl:");
            edit_template_confirmation(&bot, target, template_id, &config).await
        }
        Some(data) if data.starts_with("confirm_tpl:") => {
            match find_template(
                &config.template_options,
                data.trim_start_matches("confirm_tpl:"),
            ) {
                Some(template) => {
                    clear_screen_keyboard(&bot, target).await?;
                    let price = template_price(
                        &config.template_options,
                        &template.id,
                        &config.template_prices_usdt,
                    );
                    match start_new_order(
                        &pool,
                        query.from.id,
                        query.from.username.as_deref(),
                        Some(query.from.first_name.as_str()),
                        query.from.last_name.as_deref(),
                        BuildMode::Template,
                        Some(&template.id),
                        price,
                    )
                    .await
                    {
                        Ok(order_id) => {
                            info!(
                                user_id = query.from.id.0 as i64,
                                order_id = %order_id,
                                template_id = template.id,
                                build_mode = "template",
                                "new template order created"
                            );
                            edit_ask_logo(&bot, target).await
                        }
                        Err(error) => {
                            let msg = if format!("{error:#}").contains("pending order") {
                                warn!(
                                    ?error,
                                    user_id = query.from.id.0 as i64,
                                    template_id = template.id,
                                    "blocked template order: pending order exists"
                                );
                                "⚠️ <b>Unfinished order.</b>\nUse /continue or /cancel first."
                            } else {
                                warn!(
                                    ?error,
                                    user_id = query.from.id.0 as i64,
                                    template_id = template.id,
                                    "failed to start template order"
                                );
                                "<b>Start failed.</b>\nPlease try again."
                            };
                            edit_screen(&bot, target, msg, main_menu_keyboard()).await
                        }
                    }
                }
                None => {
                    edit_screen(
                        &bot,
                        target,
                        "<b>Template not found.</b>\nChoose again.",
                        template_keyboard(&config.template_options, &config.template_prices_usdt),
                    )
                    .await
                }
            }
        }
        Some(data) if data.starts_with("alt_domain:") => {
            let domain = data.trim_start_matches("alt_domain:");
            handle_alternative_domain_selection(&bot, target, query.from.id, domain, &config, &pool)
                .await
        }
        Some(data) if data.starts_with("skip_optional:") => {
            let field = data.trim_start_matches("skip_optional:");
            handle_optional_step_skip(&bot, target, query.from.id, field, &pool).await
        }
        Some("alt_domain_custom") => {
            edit_screen(
                &bot,
                target,
                "✏️ <b>Domain</b>\nExample: <code>pepe.xyz</code>",
                cancel_keyboard(),
            )
            .await
        }
        Some("cancel") => {
            cancel_user_order(&pool, query.from.id).await;
            edit_screen(
                &bot,
                target,
                "🛑 <b>Order cancelled</b>",
                main_menu_keyboard(),
            )
            .await
        }
        _ => edit_main_menu(&bot, target, &config).await,
    }
}

async fn start_new_order(
    pool: &SqlitePool,
    telegram_user_id: UserId,
    username: Option<&str>,
    first_name: Option<&str>,
    last_name: Option<&str>,
    build_mode: BuildMode,
    template_id: Option<&str>,
    site_price_usdt: rust_decimal::Decimal,
) -> anyhow::Result<uuid::Uuid> {
    let db_user_id = db::upsert_user(
        pool,
        telegram_user_id.0 as i64,
        username,
        first_name,
        last_name,
        true,
    )
    .await?;

    if db::has_unpaid_order(pool, telegram_user_id.0 as i64).await? {
        anyhow::bail!("has pending order");
    }

    let order_id =
        db::create_order(pool, db_user_id, build_mode, template_id, site_price_usdt).await?;
    info!(
        user_id = telegram_user_id.0 as i64,
        order_id = %order_id,
        site_price_usdt = %site_price_usdt,
        "order created"
    );
    Ok(order_id)
}

async fn expire_stale_orders_for_user(pool: &SqlitePool, user_id: UserId) {
    match db::expire_stale_pending_orders_for_user(pool, user_id.0 as i64).await {
        Ok(expired) if expired > 0 => {
            info!(
                user_id = user_id.0 as i64,
                expired_orders = expired,
                "expired stale pending orders for user"
            );
        }
        Ok(_) => {}
        Err(error) => warn!(
            ?error,
            user_id = user_id.0 as i64,
            "failed to expire stale pending orders"
        ),
    }
}

async fn cancel_user_order(pool: &SqlitePool, user_id: UserId) {
    match db::cancel_order_for_user(pool, user_id.0 as i64, None).await {
        Ok(cancelled) => {
            info!(
                user_id = user_id.0 as i64,
                cancelled_orders = cancelled,
                "cancelled pending order"
            );
        }
        Err(error) => warn!(
            ?error,
            user_id = user_id.0 as i64,
            "failed to cancel pending order"
        ),
    }
}

async fn handle_order_step(
    bot: Bot,
    msg: Message,
    user_id: UserId,
    config: Arc<TelegramUiConfig>,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    let order = match db::get_unpaid_order(pool, user_id.0 as i64).await {
        Ok(Some(order)) => order,
        Ok(None) => {
            // No active order — check if it's a TX hash for NeedPay order
            if let Some(text) = msg.text().map(str::trim)
                && looks_like_tx_hash(text)
            {
                // Already checked above via get_unpaid_order, no match
            }
            return show_main_menu(&bot, msg.chat.id, &config).await;
        }
        Err(error) => {
            warn!(?error, user_id = user_id.0 as i64, "failed to load order");
            return show_main_menu(&bot, msg.chat.id, &config).await;
        }
    };
    let order_id = order.id;

    match order.status {
        OrderStatus::NeedLogo => {
            let Some(photo) = msg.photo().and_then(|photos| photos.last()) else {
                return bot
                    .send_message(msg.chat.id, "🖼 <b>1/7 Logo</b>\nUpload token logo.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            let file_id = photo.file.id.0.clone();
            let temp_id = uuid::Uuid::new_v4().to_string();
            match download_telegram_logo(&bot, &file_id, &temp_id).await {
                Ok(local_path) => {
                    if let Err(e) = db::update_order_field_and_status(
                        pool,
                        order_id,
                        "logo_asset_path",
                        &local_path,
                        OrderStatus::NeedTicker,
                    )
                    .await
                    {
                        warn!(?e, %order_id, "failed to save logo");
                        return bot
                            .send_message(msg.chat.id, "❌ Save failed. Try again.")
                            .parse_mode(ParseMode::Html)
                            .await
                            .map(|_| ());
                    }
                    info!(user_id = user_id.0 as i64, %order_id, logo_path = %local_path, "logo saved");
                    bot.send_message(
                        msg.chat.id,
                        "✅ Logo saved.\n🔤 <b>2/7 Ticker</b>\nExample: <code>PEPE</code>",
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ())
                }
                Err(e) => {
                    warn!(user_id = user_id.0 as i64, error = %e, "failed to download logo");
                    bot.send_message(
                        msg.chat.id,
                        "❌ Failed to download logo. Please try uploading again.",
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ())
                }
            }
        }
        OrderStatus::NeedTicker => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(msg.chat.id, "🔤 <b>2/7 Ticker</b>\nSend ticker.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            let ticker = text.to_ascii_uppercase();
            if ticker.len() < 2
                || ticker.len() > 12
                || !ticker.chars().all(|ch| ch.is_ascii_alphanumeric())
            {
                return bot
                    .send_message(
                        msg.chat.id,
                        "<b>Invalid ticker.</b>\nUse 2-12 letters/numbers.",
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }
            if let Err(e) = db::update_order_field_and_status(
                pool,
                order_id,
                "token_ticker",
                &ticker,
                OrderStatus::NeedName,
            )
            .await
            {
                warn!(?e, %order_id, "failed to save ticker");
            }
            bot.send_message(
                msg.chat.id,
                "✅ Ticker saved.\n🏷 <b>3/7 Name</b>\nExample: <code>Pepe Coin</code>",
            )
            .parse_mode(ParseMode::Html)
            .await
            .map(|_| ())
        }
        OrderStatus::NeedName => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(msg.chat.id, "🏷 <b>3/7 Name</b>\nSend token name.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            if text.len() < 2 || text.len() > 48 {
                return bot
                    .send_message(msg.chat.id, "<b>Invalid name.</b>\nUse 2-48 characters.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }
            if let Err(e) = db::update_order_field_and_status(
                pool,
                order_id,
                "token_name",
                text,
                OrderStatus::NeedContract,
            )
            .await
            {
                warn!(?e, %order_id, "failed to save token name");
            }
            bot.send_message(
                msg.chat.id,
                "✅ Name saved.\n🔗 <b>4/7 Contract</b>\nSend contract address or tap Skip.",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(optional_step_keyboard("contract"))
            .await
            .map(|_| ())
        }
        OrderStatus::NeedContract => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(
                        msg.chat.id,
                        "🔗 <b>4/7 Contract</b>\nSend contract address or tap Skip.",
                    )
                    .parse_mode(ParseMode::Html)
                    .reply_markup(optional_step_keyboard("contract"))
                    .await
                    .map(|_| ());
            };
            if validate_contract_address(text).is_err() {
                return bot
                    .send_message(
                        msg.chat.id,
                        "<b>Invalid contract.</b>\nSend EVM, TRON, or SOL address, or tap Skip.",
                    )
                    .parse_mode(ParseMode::Html)
                    .reply_markup(optional_step_keyboard("contract"))
                    .await
                    .map(|_| ());
            }
            if let Err(e) = db::update_order_field_and_status(
                pool,
                order_id,
                "contract_address",
                text,
                OrderStatus::NeedXLink,
            )
            .await
            {
                warn!(?e, %order_id, "failed to save contract");
            }
            bot.send_message(
                msg.chat.id,
                "✅ Contract saved.\n✕ <b>5/7 X</b>\n<code>https://x.com/yourproject</code>\nSend X link or tap Skip.",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(optional_step_keyboard("x"))
            .await
            .map(|_| ())
        }
        OrderStatus::NeedXLink => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(msg.chat.id, "✕ <b>5/7 X</b>\nSend X link or tap Skip.")
                    .parse_mode(ParseMode::Html)
                    .reply_markup(optional_step_keyboard("x"))
                    .await
                    .map(|_| ());
            };
            if validate_x_url(text).is_err() {
                return bot
                    .send_message(
                        msg.chat.id,
                        "<b>Invalid X link.</b>\nUse <code>https://x.com/username</code>, or tap Skip.",
                    )
                    .parse_mode(ParseMode::Html)
                    .reply_markup(optional_step_keyboard("x"))
                    .await
                    .map(|_| ());
            }
            if let Err(e) = db::update_order_field_and_status(
                pool,
                order_id,
                "x_url",
                text,
                OrderStatus::NeedTme,
            )
            .await
            {
                warn!(?e, %order_id, "failed to save x_url");
            }
            bot.send_message(
                msg.chat.id,
                "✅ X saved.\n💬 <b>6/7 Telegram</b>\n<code>https://t.me/yourgroup</code>\nSend Telegram link or tap Skip.",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(optional_step_keyboard("telegram"))
            .await
            .map(|_| ())
        }
        OrderStatus::NeedTme => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(
                        msg.chat.id,
                        "💬 <b>6/7 Telegram</b>\nSend Telegram link or tap Skip.",
                    )
                    .parse_mode(ParseMode::Html)
                    .reply_markup(optional_step_keyboard("telegram"))
                    .await
                    .map(|_| ());
            };
            if validate_telegram_url(text).is_err() {
                return bot
                    .send_message(
                        msg.chat.id,
                        "<b>Invalid Telegram link.</b>\nUse <code>https://t.me/yourgroup</code>, or tap Skip.",
                    )
                    .parse_mode(ParseMode::Html)
                    .reply_markup(optional_step_keyboard("telegram"))
                    .await
                    .map(|_| ());
            }
            if let Err(e) = db::update_order_field_and_status(
                pool,
                order_id,
                "telegram_url",
                text,
                OrderStatus::NeedDomain,
            )
            .await
            {
                warn!(?e, %order_id, "failed to save telegram_url");
            }
            bot.send_message(
                msg.chat.id,
                "✅ Telegram saved.\n🌐 <b>7/7 Domain</b>\nExample: <code>pepe.xyz</code>",
            )
            .parse_mode(ParseMode::Html)
            .await
            .map(|_| ())
        }
        OrderStatus::NeedDomain => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(msg.chat.id, "🌐 <b>7/7 Domain</b>\nSend domain.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            let domain = normalize_domain(text);
            if validate_domain(&domain).is_err() {
                return bot
                    .send_message(
                        msg.chat.id,
                        "<b>Invalid domain.</b>\nTry .com, .xyz, .fun, or .site.",
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }
            handle_domain_step(&bot, msg.chat.id, user_id, &order, &domain, &config, pool).await
        }
        OrderStatus::NeedPay => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(
                        msg.chat.id,
                        "💳 <b>Payment</b>\nChoose a chain or paste TX hash.",
                    )
                    .parse_mode(ParseMode::Html)
                    .reply_markup(payment_chain_keyboard())
                    .await
                    .map(|_| ());
            };
            if !looks_like_tx_hash(text) {
                return bot
                    .send_message(
                        msg.chat.id,
                        "💳 <b>Payment</b>\nChoose a chain or paste TX hash.",
                    )
                    .parse_mode(ParseMode::Html)
                    .reply_markup(payment_chain_keyboard())
                    .await
                    .map(|_| ());
            }
            let chain = order.payment_chain.unwrap_or(PaymentChain::Erc20);
            process_payment_tx_context(PaymentTxProcess {
                bot: &bot,
                chat_id: msg.chat.id,
                user_id,
                config: &config,
                pool,
                order_id,
                chain,
                amount_usdt: order.total_usdt,
                tx_hash: text,
            })
            .await
        }
        OrderStatus::PaidAndGenerating => bot
            .send_message(
                msg.chat.id,
                "⚙️ <b>Generating...</b>\nYour website is being created.",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(status_keyboard())
            .await
            .map(|_| ()),
        _ => show_main_menu(&bot, msg.chat.id, &config).await,
    }
}

async fn handle_domain_step(
    bot: &Bot,
    chat_id: ChatId,
    user_id: UserId,
    order: &Order,
    domain: &str,
    config: &TelegramUiConfig,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    let order_id = order.id;

    if config.skip_domain_purchase {
        warn!(user_id = user_id.0 as i64, %domain, "SKIP_DOMAIN_PURCHASE: accepting domain without quote");
        let total = order.site_price_usdt;
        if let Err(e) =
            db::set_order_domain_pricing(pool, order_id, rust_decimal::Decimal::ZERO, total).await
        {
            warn!(?e, %order_id, "failed to set domain pricing");
        }
        if let Err(e) = db::update_order_field_and_status(
            pool,
            order_id,
            "requested_domain",
            domain,
            OrderStatus::NeedPay,
        )
        .await
        {
            warn!(?e, %order_id, "failed to update domain/status");
        }
        return bot.send_message(
            chat_id,
            format!(
                "✅ <b>Domain accepted</b> (testing mode)\nDomain: <b>{domain}</b>\nWebsite: <b>{}</b> USDT\nTotal: <b>{total}</b> USDT\n\nChoose payment chain.",
                order.site_price_usdt
            ),
        )
        .parse_mode(ParseMode::Html)
        .reply_markup(payment_chain_keyboard())
        .await.map(|_| ());
    }

    let (Some(base_url), Some(api_key)) = (
        config.api_service_base_url.as_deref(),
        config.api_service_api_key.as_deref(),
    ) else {
        return bot
            .send_message(
                chat_id,
                "❌ Domain service is not configured. Contact admin.",
            )
            .parse_mode(ParseMode::Html)
            .await
            .map(|_| ());
    };
    let registrar: Box<dyn DomainRegistrar> = Box::new(HttpClientDomainRegistrar::new(
        base_url.to_string(),
        api_key.to_string(),
    ));
    match quote_domain_with_retries(registrar.as_ref(), domain).await {
        Ok(quote) if quote.available && !quote.is_premium => {
            let total = order.site_price_usdt + quote.price_usd;
            if let Err(e) =
                db::set_order_domain_pricing(pool, order_id, quote.price_usd, total).await
            {
                warn!(?e, %order_id, "failed to set domain pricing");
                return bot
                    .send_message(chat_id, "<b>Quote save failed.</b>\nPlease try again.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }
            if let Err(e) = db::update_order_field_and_status(
                pool,
                order_id,
                "requested_domain",
                &quote.domain,
                OrderStatus::NeedPay,
            )
            .await
            {
                warn!(?e, %order_id, "failed to update domain/status");
            }
            info!(user_id = user_id.0 as i64, %order_id, domain = %quote.domain, %total, "domain accepted, ready for payment");
            bot.send_message(
                chat_id,
                format!(
                    "✅ <b>Domain available</b>\nDomain: <b>{}</b> USDT\nWebsite: <b>{}</b> USDT\nTotal: <b>{}</b> USDT\n\nChoose payment chain.",
                    quote.price_usd, order.site_price_usdt, total
                ),
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(payment_chain_keyboard())
            .await.map(|_| ())
        }
        Ok(_) | Err(_) => {
            info!(user_id = user_id.0 as i64, %domain, "domain unavailable; checking alternatives");
            let tld = domain.rsplit_once('.').map(|(_, t)| t).unwrap_or("");
            let base_name = domain
                .rsplit_once('.')
                .map(|(name, _)| name)
                .unwrap_or(domain);
            let alternatives = check_alternative_domains(registrar.as_ref(), base_name, tld).await;

            if alternatives.is_empty() {
                bot.send_message(chat_id, "<b>Domain unavailable.</b>\nSend another domain.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ())
            } else {
                // Store alternatives in DB
                if let Ok(json) = serde_json::to_string(&alternatives) {
                    let _ =
                        db::update_order_field(pool, order_id, "alternative_domains_json", &json)
                            .await;
                }
                bot.send_message(
                    chat_id,
                    format!("🔍 <b>{} is taken.</b>\nChoose one:", html_escape(domain)),
                )
                .parse_mode(ParseMode::Html)
                .reply_markup(alternative_domains_keyboard(&alternatives))
                .await
                .map(|_| ())
            }
        }
    }
}

async fn handle_optional_step_skip(
    bot: &Bot,
    target: UiMessage,
    user_id: UserId,
    field: &str,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    let order = match db::get_unpaid_order(pool, user_id.0 as i64).await {
        Ok(Some(order)) => order,
        Ok(None) => {
            return edit_screen(
                bot,
                target,
                "<b>No active order.</b>\nUse /create.",
                main_menu_keyboard(),
            )
            .await;
        }
        Err(e) => {
            warn!(
                ?e,
                user_id = user_id.0 as i64,
                "failed to load order for optional skip"
            );
            return edit_screen(
                bot,
                target,
                "<b>Error.</b>\nTry again.",
                main_menu_keyboard(),
            )
            .await;
        }
    };

    let (expected_status, db_field, next_status, next_text, next_keyboard) = match field {
        "contract" => (
            OrderStatus::NeedContract,
            "contract_address",
            OrderStatus::NeedXLink,
            "⏭ Contract skipped.\n✕ <b>5/7 X</b>\nSend X link or tap Skip.",
            optional_step_keyboard("x"),
        ),
        "x" => (
            OrderStatus::NeedXLink,
            "x_url",
            OrderStatus::NeedTme,
            "⏭ X skipped.\n💬 <b>6/7 Telegram</b>\nSend Telegram link or tap Skip.",
            optional_step_keyboard("telegram"),
        ),
        "telegram" => (
            OrderStatus::NeedTme,
            "telegram_url",
            OrderStatus::NeedDomain,
            "⏭ Telegram skipped.\n🌐 <b>7/7 Domain</b>\nExample: <code>pepe.xyz</code>",
            cancel_keyboard(),
        ),
        other => {
            warn!(field = other, "unknown optional skip field");
            return edit_screen(bot, target, "<b>Unknown action.</b>", main_menu_keyboard()).await;
        }
    };

    if order.status != expected_status {
        return edit_screen(
            bot,
            target,
            "<b>This step is no longer active.</b>\nUse /continue.",
            main_menu_keyboard(),
        )
        .await;
    }

    if let Err(e) =
        db::update_order_field_and_status(pool, order.id, db_field, "", next_status).await
    {
        warn!(?e, order_id = %order.id, field, "failed to skip optional field");
        return edit_screen(
            bot,
            target,
            "<b>Save failed.</b>\nPlease try again.",
            cancel_keyboard(),
        )
        .await;
    }

    edit_screen(bot, target, next_text, next_keyboard).await
}

async fn ensure_group_member(
    bot: &Bot,
    msg: &Message,
    config: &TelegramUiConfig,
) -> ResponseResult<bool> {
    let Some(user) = msg.from.as_ref() else {
        return Ok(true);
    };
    ensure_group_member_for_user(bot, msg.chat.id, user.id, config).await
}

async fn ensure_group_member_for_user(
    bot: &Bot,
    chat_id: ChatId,
    user_id: UserId,
    config: &TelegramUiConfig,
) -> ResponseResult<bool> {
    let Some(group) = &config.required_group else {
        return Ok(true);
    };

    let result = match group {
        RequiredGroup::Id(id) => bot.get_chat_member(ChatId(*id), user_id).await,
        RequiredGroup::Username(username) => {
            bot.get_chat_member(Recipient::ChannelUsername(username.clone()), user_id)
                .await
        }
    };

    match result {
        Ok(member) if member.is_present() => Ok(true),
        _ => {
            bot.send_message(
                chat_id,
                "<b>Access required</b>\nJoin: https://t.me/MeMe_Web_Support",
            )
            .parse_mode(ParseMode::Html)
            .await?;
            Ok(false)
        }
    }
}

async fn handle_continue_command(
    bot: &Bot,
    chat_id: ChatId,
    user_id: UserId,
    pool: &SqlitePool,
    _config: &TelegramUiConfig,
) -> ResponseResult<()> {
    match db::get_unpaid_order(pool, user_id.0 as i64).await {
        Ok(Some(order)) => continue_order_screen(bot, chat_id, &order).await,
        Ok(None) => bot
            .send_message(chat_id, "<b>No active order.</b>\nUse /create.")
            .parse_mode(ParseMode::Html)
            .reply_markup(main_menu_keyboard())
            .await
            .map(|_| ()),
        Err(error) => {
            warn!(
                ?error,
                user_id = user_id.0 as i64,
                "failed to load pending order"
            );
            bot.send_message(chat_id, "<b>Check failed.</b>\nTry later.")
                .parse_mode(ParseMode::Html)
                .reply_markup(main_menu_keyboard())
                .await
                .map(|_| ())
        }
    }
}

async fn edit_payment_chain_and_ask_tx(
    bot: &Bot,
    target: UiMessage,
    user_id: UserId,
    chain: PaymentChain,
    address: &str,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    let order = match db::get_unpaid_order(pool, user_id.0 as i64).await {
        Ok(Some(order)) => order,
        Ok(None) => {
            return edit_screen(
                bot,
                target,
                "<b>No active order.</b>\nUse /create.",
                main_menu_keyboard(),
            )
            .await;
        }
        Err(e) => {
            warn!(
                ?e,
                user_id = user_id.0 as i64,
                "failed to load order for payment chain"
            );
            return edit_screen(
                bot,
                target,
                "<b>Error.</b>\nTry again.",
                main_menu_keyboard(),
            )
            .await;
        }
    };

    if order.status != OrderStatus::NeedPay {
        return edit_screen(
            bot,
            target,
            "<b>Payment not ready.</b>\nComplete all steps first.",
            cancel_keyboard(),
        )
        .await;
    }

    if address.trim().is_empty() {
        return edit_screen(
            bot,
            target,
            "<b>Payment address missing.</b>\nContact admin.",
            main_menu_keyboard(),
        )
        .await;
    }

    let order_id = order.id;
    let total = order.total_usdt;

    let payment_window = match db::attach_payment_chain(pool, order_id, chain).await {
        Ok(window) => {
            info!(
                user_id = user_id.0 as i64,
                %order_id,
                chain = chain.as_str(),
                payment_started_at = %window.started_at,
                payment_expires_at = %window.expires_at,
                "payment window started"
            );
            window
        }
        Err(e) => {
            warn!(?e, %order_id, "failed to attach payment chain");
            return edit_screen(
                bot,
                target,
                "<b>Payment start failed.</b>\nPlease try again.",
                main_menu_keyboard(),
            )
            .await;
        }
    };

    edit_screen(
        bot,
        target,
        super::screens::payment_instruction(order_id, total, chain, address, &payment_window),
        cancel_keyboard(),
    )
    .await
}

async fn handle_alternative_domain_selection(
    bot: &Bot,
    target: UiMessage,
    user_id: UserId,
    domain: &str,
    _config: &TelegramUiConfig,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    let order = match db::get_unpaid_order(pool, user_id.0 as i64).await {
        Ok(Some(order)) => order,
        Ok(None) => {
            return edit_screen(
                bot,
                target,
                "<b>No active order.</b>\nUse /create.",
                main_menu_keyboard(),
            )
            .await;
        }
        Err(e) => {
            warn!(
                ?e,
                user_id = user_id.0 as i64,
                "failed to load order for alt domain"
            );
            return edit_screen(
                bot,
                target,
                "<b>Error.</b>\nTry again.",
                main_menu_keyboard(),
            )
            .await;
        }
    };

    let alternatives =
        parse_alternative_domains(order.alternative_domains_json.as_deref()).unwrap_or_default();
    let Some(quote) = alternatives.iter().find(|q| q.domain == domain).cloned() else {
        return edit_screen(
            bot,
            target,
            "<b>Domain expired.</b>\nSend a new domain.",
            main_menu_keyboard(),
        )
        .await;
    };

    let order_id = order.id;
    let total = order.site_price_usdt + quote.price_usd;

    if let Err(e) = db::set_order_domain_pricing(pool, order_id, quote.price_usd, total).await {
        warn!(?e, %order_id, "failed to set domain pricing for alt domain");
        return edit_screen(
            bot,
            target,
            "<b>Quote save failed.</b>\nPlease try again.",
            main_menu_keyboard(),
        )
        .await;
    }
    if let Err(e) = db::update_order_field_and_status(
        pool,
        order_id,
        "requested_domain",
        domain,
        OrderStatus::NeedPay,
    )
    .await
    {
        warn!(?e, %order_id, "failed to update domain/status for alt domain");
    }
    // Clear stored alternatives
    let _ = db::clear_order_field(pool, order_id, "alternative_domains_json").await;

    edit_screen(
        bot,
        target,
        format!(
            "✅ <b>Domain selected</b>\n{domain}\nDomain: <b>{}</b> USDT\nWebsite: <b>{}</b> USDT\nTotal: <b>{}</b> USDT\n\nChoose payment chain.",
            quote.price_usd, order.site_price_usdt, total
        ),
        payment_chain_keyboard(),
    )
    .await
}
