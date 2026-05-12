use std::sync::Arc;

use sqlx::SqlitePool;
use teloxide::prelude::*;
use teloxide::types::{CallbackQuery, ChatId, Message, ParseMode, Recipient, UserId};
use tracing::{info, warn};

use crate::{
    api_client::HttpClientDomainRegistrar,
    db,
    domain::{DomainRegistrar, NamecheapRegistrar},
    models::PaymentChain,
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
    sessions: Sessions,
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
    let current_step = session_step_name(user_id, &sessions).await;
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
            "/status" => return show_status(&bot, msg.chat.id, user_id, &sessions, &pool).await,
            "/history" => return show_history(&bot, msg.chat.id, user_id, &pool).await,
            "/continue" => {
                return handle_continue_command(
                    &bot,
                    msg.chat.id,
                    user_id,
                    &sessions,
                    &pool,
                    &config,
                )
                .await;
            }
            "/cancel" => {
                cancel_user_order(&pool, user_id, &sessions).await;
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

    handle_order_step(bot, msg, user_id, config, sessions, &pool).await
}

pub(crate) async fn handle_callback(
    bot: Bot,
    query: CallbackQuery,
    config: Arc<TelegramUiConfig>,
    sessions: Sessions,
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
    let current_step = session_step_name(query.from.id, &sessions).await;
    info!(
        user_id = query.from.id.0 as i64,
        chat_id = %chat_id,
        callback = query.data.as_deref().unwrap_or("<none>"),
        session_step = current_step.unwrap_or("none"),
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
            let draft = match start_new_order_session(
                &pool,
                query.from.id,
                query.from.username.as_deref(),
                Some(query.from.first_name.as_str()),
                query.from.last_name.as_deref(),
                OrderDraft::new_ai(config.ai_creation_price_usdt),
            )
            .await
            {
                Ok(draft) => draft,
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
                            "failed to start ai order session"
                        );
                        "<b>Start failed.</b>\nPlease try again."
                    };
                    return edit_screen(&bot, target, msg, main_menu_keyboard()).await;
                }
            };
            set_session(query.from.id, draft, &sessions);
            info!(
                user_id = query.from.id.0 as i64,
                build_mode = "ai_free",
                next_step = DraftStep::WaitingLogo.as_str(),
                "new order session started"
            );
            edit_ask_logo(&bot, target).await
        }
        Some("pay_menu") => edit_payment_chain_menu(&bot, target).await,
        Some("status") => edit_status(&bot, target, query.from.id, &sessions, &pool).await,
        Some("history") => edit_history(&bot, target, query.from.id, &pool).await,
        Some("contact_admin") => edit_contact_admin(&bot, target, &config).await,
        Some("pay_erc20") => {
            edit_payment_chain_and_ask_tx(
                &bot,
                target,
                query.from.id,
                PaymentChain::Erc20,
                &config.usdt_erc20_address,
                &sessions,
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
                &sessions,
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
                &sessions,
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
                    let draft = match start_new_order_session(
                        &pool,
                        query.from.id,
                        query.from.username.as_deref(),
                        Some(query.from.first_name.as_str()),
                        query.from.last_name.as_deref(),
                        OrderDraft::new_template(
                            template.clone(),
                            template_price(
                                &config.template_options,
                                &template.id,
                                &config.template_prices_usdt,
                            ),
                        ),
                    )
                    .await
                    {
                        Ok(draft) => draft,
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
                                    "failed to start template order session"
                                );
                                "<b>Start failed.</b>\nPlease try again."
                            };
                            return edit_screen(&bot, target, msg, main_menu_keyboard()).await;
                        }
                    };
                    set_session(query.from.id, draft, &sessions);
                    info!(
                        user_id = query.from.id.0 as i64,
                        build_mode = "template",
                        template_id = template.id,
                        next_step = DraftStep::WaitingLogo.as_str(),
                        "new order session started"
                    );
                    edit_ask_logo(&bot, target).await
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
            handle_alternative_domain_selection(
                &bot,
                target,
                query.from.id,
                domain,
                &config,
                &sessions,
                &pool,
            )
            .await
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
            cancel_user_order(&pool, query.from.id, &sessions).await;
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

async fn start_new_order_session(
    pool: &SqlitePool,
    telegram_user_id: UserId,
    username: Option<&str>,
    first_name: Option<&str>,
    last_name: Option<&str>,
    mut draft: OrderDraft,
) -> anyhow::Result<OrderDraft> {
    let db_user_id = db::upsert_user(
        pool,
        telegram_user_id.0 as i64,
        username,
        first_name,
        last_name,
        true,
    )
    .await?;

    if db::has_pending_order(pool, telegram_user_id.0 as i64).await? {
        anyhow::bail!("has pending order");
    }

    let build_mode = match draft.build_mode {
        BuildModeDraft::Template => crate::models::BuildMode::Template,
        BuildModeDraft::AiFree => crate::models::BuildMode::AiFree,
    };
    let order_id = db::create_site_order(
        pool,
        db_user_id,
        build_mode,
        draft.template_id.as_deref(),
        draft.site_price_usdt,
        None,
    )
    .await?;
    info!(
        user_id = telegram_user_id.0 as i64,
        order_id = %order_id,
        site_price_usdt = %draft.site_price_usdt,
        "draft order persisted before asset collection"
    );
    draft.db_user_id = Some(db_user_id);
    draft.order_id = Some(order_id);
    Ok(draft)
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

async fn cancel_user_order(pool: &SqlitePool, user_id: UserId, sessions: &Sessions) {
    let order_id = get_session(user_id, sessions).and_then(|draft| draft.order_id);
    remove_session(user_id, sessions);
    match db::cancel_order_for_user(pool, user_id.0 as i64, order_id).await {
        Ok(cancelled) => {
            info!(
                user_id = user_id.0 as i64,
                order_id = %order_id.map(|id| id.to_string()).unwrap_or_else(|| "<latest>".to_string()),
                cancelled_orders = cancelled,
                "cancelled pending order"
            );
        }
        Err(error) => warn!(
            ?error,
            user_id = user_id.0 as i64,
            order_id = %order_id.map(|id| id.to_string()).unwrap_or_else(|| "<latest>".to_string()),
            "failed to cancel pending order"
        ),
    }
}

async fn handle_order_step(
    bot: Bot,
    msg: Message,
    user_id: UserId,
    config: Arc<TelegramUiConfig>,
    sessions: Sessions,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    let Some(mut draft) = get_session(user_id, &sessions) else {
        if let Some(text) = msg.text().map(str::trim)
            && looks_like_tx_hash(text)
        {
            match db::get_latest_pending_payment_context_for_user(pool, user_id.0 as i64).await {
                Ok(Some(context)) => {
                    return process_payment_tx_context(PaymentTxProcess {
                        bot: &bot,
                        chat_id: msg.chat.id,
                        user_id,
                        config: &config,
                        sessions: &sessions,
                        pool,
                        context,
                        tx_hash: text,
                    })
                    .await;
                }
                Ok(None) => {}
                Err(error) => {
                    warn!(
                        ?error,
                        user_id = user_id.0 as i64,
                        "failed to recover pending payment context"
                    );
                    return bot
                        .send_message(
                            msg.chat.id,
                            "⚠️ <b>Payment check failed.</b>\nContact admin.",
                        )
                        .parse_mode(ParseMode::Html)
                        .await
                        .map(|_| ());
                }
            }
        }
        return show_main_menu(&bot, msg.chat.id, &config).await;
    };
    if draft.is_expired() {
        warn!(
            user_id = user_id.0 as i64,
            order_id = %draft.order_id.map(|id| id.to_string()).unwrap_or_else(|| "<none>".to_string()),
            last_step = draft.step.as_str(),
            "order session expired while collecting input"
        );
        cancel_user_order(pool, user_id, &sessions).await;
        return bot
            .send_message(
                msg.chat.id,
                "<b>Order expired.</b>\nStart again with /create.",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(main_menu_keyboard())
            .await
            .map(|_| ());
    }

    match draft.step {
        DraftStep::WaitingLogo => {
            let Some(photo) = msg.photo().and_then(|photos| photos.last()) else {
                return bot
                    .send_message(msg.chat.id, "🖼 <b>1/7 Logo</b>\nUpload token logo.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            draft.logo_file_id = Some(photo.file.id.0.clone());
            draft.step = DraftStep::WaitingTicker;
            info!(
                user_id = user_id.0 as i64,
                chat_id = %msg.chat.id,
                next_step = DraftStep::WaitingTicker.as_str(),
                "logo received; advancing order wizard"
            );
            set_session(user_id, draft, &sessions);
            bot.send_message(
                msg.chat.id,
                "✅ Logo saved.\n🔤 <b>2/7 Ticker</b>\nExample: <code>PEPE</code>",
            )
            .parse_mode(ParseMode::Html)
            .await
            .map(|_| ())
        }
        DraftStep::WaitingTicker => {
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
            draft.ticker = Some(ticker);
            draft.step = DraftStep::WaitingTokenName;
            info!(
                user_id = user_id.0 as i64,
                chat_id = %msg.chat.id,
                next_step = DraftStep::WaitingTokenName.as_str(),
                "ticker accepted; advancing order wizard"
            );
            set_session(user_id, draft, &sessions);
            bot.send_message(
                msg.chat.id,
                "✅ Ticker saved.\n🏷 <b>3/7 Name</b>\nExample: <code>Pepe Coin</code>",
            )
            .parse_mode(ParseMode::Html)
            .await
            .map(|_| ())
        }
        DraftStep::WaitingTokenName => {
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
            draft.token_name = Some(text.to_string());
            draft.step = DraftStep::WaitingContractAddress;
            info!(
                user_id = user_id.0 as i64,
                chat_id = %msg.chat.id,
                next_step = DraftStep::WaitingContractAddress.as_str(),
                "token name accepted; advancing order wizard"
            );
            set_session(user_id, draft, &sessions);
            bot.send_message(
                msg.chat.id,
                "✅ Name saved.\n🔗 <b>4/7 Contract</b>\nSend contract address.",
            )
            .parse_mode(ParseMode::Html)
            .await
            .map(|_| ())
        }
        DraftStep::WaitingContractAddress => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(
                        msg.chat.id,
                        "🔗 <b>4/7 Contract</b>\nSend contract address.",
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            if validate_contract_address(text).is_err() {
                return bot
                    .send_message(
                        msg.chat.id,
                        "<b>Invalid contract.</b>\nSend EVM, TRON, or SOL address.",
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }
            draft.contract_address = Some(text.to_string());
            draft.step = DraftStep::WaitingXLink;
            info!(
                user_id = user_id.0 as i64,
                chat_id = %msg.chat.id,
                next_step = DraftStep::WaitingXLink.as_str(),
                "contract address accepted; advancing order wizard"
            );
            set_session(user_id, draft, &sessions);
            bot.send_message(
                msg.chat.id,
                "✅ Contract saved.\n✕ <b>5/7 X</b>\n<code>https://x.com/yourproject</code>",
            )
            .parse_mode(ParseMode::Html)
            .await
            .map(|_| ())
        }
        DraftStep::WaitingXLink => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(msg.chat.id, "✕ <b>5/7 X</b>\nSend X link.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            if validate_x_url(text).is_err() {
                return bot
                    .send_message(
                        msg.chat.id,
                        "<b>Invalid X link.</b>\nUse <code>https://x.com/username</code>",
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }
            draft.x_link = Some(text.to_string());
            draft.step = DraftStep::WaitingTelegramLink;
            info!(
                user_id = user_id.0 as i64,
                chat_id = %msg.chat.id,
                next_step = DraftStep::WaitingTelegramLink.as_str(),
                "x link accepted; advancing order wizard"
            );
            set_session(user_id, draft, &sessions);
            bot.send_message(
                msg.chat.id,
                "✅ X saved.\n💬 <b>6/7 Telegram</b>\n<code>https://t.me/yourgroup</code>",
            )
            .parse_mode(ParseMode::Html)
            .await
            .map(|_| ())
        }
        DraftStep::WaitingTelegramLink => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(msg.chat.id, "💬 <b>6/7 Telegram</b>\nSend Telegram link.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            if validate_telegram_url(text).is_err() {
                return bot
                    .send_message(
                        msg.chat.id,
                        "<b>Invalid Telegram link.</b>\nUse <code>https://t.me/yourgroup</code>",
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }
            draft.telegram_link = Some(text.to_string());
            draft.step = DraftStep::WaitingWebsiteUrl;
            info!(
                user_id = user_id.0 as i64,
                chat_id = %msg.chat.id,
                next_step = DraftStep::WaitingWebsiteUrl.as_str(),
                "telegram link accepted; advancing order wizard"
            );
            set_session(user_id, draft, &sessions);
            bot.send_message(
                msg.chat.id,
                "✅ Telegram saved.\n🌐 <b>7/7 Domain</b>\nExample: <code>pepe.xyz</code>",
            )
            .parse_mode(ParseMode::Html)
            .await
            .map(|_| ())
        }
        DraftStep::WaitingWebsiteUrl => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(msg.chat.id, "🌐 <b>7/7 Domain</b>\nSend domain.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            let domain = normalize_domain(text);
            info!(
                user_id = user_id.0 as i64,
                chat_id = %msg.chat.id,
                domain = %domain,
                "checking requested domain"
            );
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

            let registrar: Box<dyn DomainRegistrar> = if let (Some(base_url), Some(api_key)) = (
                config.api_service_base_url.as_deref(),
                config.api_service_api_key.as_deref(),
            ) {
                Box::new(HttpClientDomainRegistrar::new(
                    base_url.to_string(),
                    api_key.to_string(),
                ))
            } else {
                Box::new(NamecheapRegistrar::new(config.namecheap.clone()))
            };
            match quote_domain_with_retries(registrar.as_ref(), &domain).await {
                Ok(quote) if quote.available && !quote.is_premium => {
                    info!(
                        user_id = user_id.0 as i64,
                        chat_id = %msg.chat.id,
                        domain = %quote.domain,
                        price_usd = %quote.price_usd,
                        "domain quote available"
                    );
                    let db_user_id =
                        match db::upsert_user(pool, user_id.0 as i64, None, None, None, true).await
                        {
                            Ok(id) => id,
                            Err(e) => {
                                warn!(
                                    ?e,
                                    user_id = user_id.0 as i64,
                                    "failed to upsert user for domain quote"
                                );
                                return bot
                                    .send_message(
                                        msg.chat.id,
                                        "<b>Quote failed.</b>\nPlease try again.",
                                    )
                                    .parse_mode(ParseMode::Html)
                                    .await
                                    .map(|_| ());
                            }
                        };
                    draft.website_url = Some(domain);
                    draft.db_user_id = Some(db_user_id);
                    draft.domain_price_usdt = Some(quote.price_usd);
                    let total = draft.site_price_usdt + quote.price_usd;
                    match db::record_domain_quote(pool, db_user_id, &quote, quote.price_usd).await {
                        Ok(quote_id) => {
                            info!(
                                user_id = user_id.0 as i64,
                                quote_id = %quote_id,
                                domain = %quote.domain,
                                "domain quote persisted"
                            );
                            if let Some(order_id) = draft.order_id
                                && let Err(error) = db::prepare_order_for_payment(
                                    pool,
                                    order_id,
                                    total,
                                    Some(quote_id),
                                )
                                .await
                            {
                                warn!(
                                    ?error,
                                    user_id = user_id.0 as i64,
                                    order_id = %order_id,
                                    quote_id = %quote_id,
                                    "failed to attach domain quote to pending order"
                                );
                                return bot
                                    .send_message(
                                        msg.chat.id,
                                        "<b>Quote save failed.</b>\nPlease try again.",
                                    )
                                    .parse_mode(ParseMode::Html)
                                    .await
                                    .map(|_| ());
                            }
                            draft.domain_quote_id = Some(quote_id)
                        }
                        Err(e) => {
                            warn!(?e, "failed to record domain quote");
                            return bot
                                .send_message(
                                    msg.chat.id,
                                    "<b>Quote failed.</b>\nPlease try again.",
                                )
                                .parse_mode(ParseMode::Html)
                                .await
                                .map(|_| ());
                        }
                    }
                    draft.total_usdt = Some(total);
                    draft.step = DraftStep::ReadyToPay;
                    info!(
                        user_id = user_id.0 as i64,
                        chat_id = %msg.chat.id,
                        domain = %quote.domain,
                        total_usdt = %total,
                        next_step = DraftStep::ReadyToPay.as_str(),
                        "order ready for payment chain selection"
                    );
                    set_session(user_id, draft.clone(), &sessions);
                    bot.send_message(
                        msg.chat.id,
                        format!(
                            "✅ <b>Domain available</b>\nDomain: <b>{}</b> USDT\nWebsite: <b>{}</b> USDT\nTotal: <b>{}</b> USDT\n\nChoose payment chain.",
                            quote.price_usd, draft.site_price_usdt, total
                        ),
                    )
                    .parse_mode(ParseMode::Html)
                    .reply_markup(payment_chain_keyboard())
                    .await
                    .map(|_| ())
                }
                Ok(_) | Err(_) => {
                    info!(
                        user_id = user_id.0 as i64,
                        chat_id = %msg.chat.id,
                        domain = %domain,
                        "requested domain unavailable; checking alternatives"
                    );
                    let tld = domain.rsplit_once('.').map(|(_, t)| t).unwrap_or("");
                    let base_name = domain
                        .rsplit_once('.')
                        .map(|(name, _)| name)
                        .unwrap_or(&domain);
                    let alternatives =
                        check_alternative_domains(registrar.as_ref(), base_name, tld).await;

                    if alternatives.is_empty() {
                        bot.send_message(
                            msg.chat.id,
                            "<b>Domain unavailable.</b>\nSend another domain.",
                        )
                        .parse_mode(ParseMode::Html)
                        .await
                        .map(|_| ())
                    } else {
                        draft.alternative_domains = Some(alternatives.clone());
                        set_session(user_id, draft.clone(), &sessions);
                        bot.send_message(
                            msg.chat.id,
                            format!("🔍 <b>{} is taken.</b>\nChoose one:", html_escape(&domain),),
                        )
                        .parse_mode(ParseMode::Html)
                        .reply_markup(alternative_domains_keyboard(&alternatives))
                        .await
                        .map(|_| ())
                    }
                }
            }
        }
        DraftStep::ReadyToPay => bot
            .send_message(msg.chat.id, "💳 <b>Payment</b>\nChoose a chain.")
            .parse_mode(ParseMode::Html)
            .reply_markup(payment_chain_keyboard())
            .await
            .map(|_| ()),
        DraftStep::WaitingTx => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(msg.chat.id, "💳 <b>Payment</b>\nPaste TX hash.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            if !looks_like_tx_hash(text) {
                return bot
                    .send_message(msg.chat.id, "<b>Invalid TX.</b>\nPaste transaction hash.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }

            let Some(order_id) = draft.order_id else {
                return bot
                    .send_message(msg.chat.id, "⚠️ <b>No order found.</b>\nUse /create.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };

            let context = match db::get_payment_context_by_order(pool, order_id).await {
                Ok(context) => context,
                Err(error) => {
                    warn!(?error, order_id = %order_id, "failed to load payment context");
                    return bot
                        .send_message(
                            msg.chat.id,
                            "⚠️ <b>Payment check failed.</b>\nContact admin.",
                        )
                        .parse_mode(ParseMode::Html)
                        .await
                        .map(|_| ());
                }
            };

            process_payment_tx_context(PaymentTxProcess {
                bot: &bot,
                chat_id: msg.chat.id,
                user_id,
                config: &config,
                sessions: &sessions,
                pool,
                context,
                tx_hash: text,
            })
            .await
        }
        DraftStep::PaymentConfirmed => bot
            .send_message(msg.chat.id, "<b>Payment confirmed.</b>\nWebsite is queued.")
            .parse_mode(ParseMode::Html)
            .reply_markup(status_keyboard())
            .await
            .map(|_| ()),
    }
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
    sessions: &Sessions,
    pool: &SqlitePool,
    _config: &TelegramUiConfig,
) -> ResponseResult<()> {
    if let Some(draft) = get_session(user_id, sessions) {
        if draft.is_expired() {
            cancel_user_order(pool, user_id, sessions).await;
            return bot
                .send_message(chat_id, "<b>Order expired.</b>\nUse /create.")
                .parse_mode(ParseMode::Html)
                .reply_markup(main_menu_keyboard())
                .await
                .map(|_| ());
        }
        return continue_step_screen(bot, chat_id, &draft).await;
    }

    match db::get_pending_order_for_user(pool, user_id.0 as i64).await {
        Ok(Some(order)) => {
            let build_mode = match order.build_mode {
                Some(crate::models::BuildMode::Template) => BuildModeDraft::Template,
                _ => BuildModeDraft::AiFree,
            };
            let draft = OrderDraft {
                build_mode,
                step: DraftStep::ReadyToPay,
                site_price_usdt: order.amount_usdt,
                template_id: order.template_id.clone(),
                logo_file_id: None,
                ticker: None,
                token_name: None,
                contract_address: None,
                x_link: None,
                telegram_link: None,
                website_url: None,
                domain_quote_id: None,
                domain_price_usdt: None,
                alternative_domains: None,
                total_usdt: Some(order.amount_usdt),
                payment_chain: None,
                order_id: Some(order.id),
                db_user_id: Some(order.user_id),
                updated_at: chrono::Utc::now(),
            };
            set_session(user_id, draft.clone(), sessions);
            continue_step_screen(bot, chat_id, &draft).await
        }
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
    sessions: &Sessions,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    let Some(mut draft) = get_session(user_id, sessions) else {
        return edit_screen(
            bot,
            target,
            "<b>No active order.</b>\nUse /create.",
            main_menu_keyboard(),
        )
        .await;
    };
    if draft.is_expired() {
        cancel_user_order(pool, user_id, sessions).await;
        return edit_screen(
            bot,
            target,
            "<b>Order expired.</b>\nUse /create.",
            main_menu_keyboard(),
        )
        .await;
    }

    let Some(total) = draft.total_usdt else {
        return edit_screen(
            bot,
            target,
            "<b>Payment not ready.</b>\nComplete all steps first.",
            cancel_keyboard(),
        )
        .await;
    };

    if address.trim().is_empty() {
        return edit_screen(
            bot,
            target,
            "<b>Payment address missing.</b>\nContact admin.",
            main_menu_keyboard(),
        )
        .await;
    }

    let db_user_id = if let Some(db_user_id) = draft.db_user_id {
        db_user_id
    } else {
        match db::upsert_user(pool, user_id.0 as i64, None, None, None, true).await {
            Ok(id) => id,
            Err(e) => {
                warn!(?e, user_id = user_id.0 as i64, "failed to upsert user");
                return edit_screen(
                    bot,
                    target,
                    "<b>Order failed.</b>\nPlease try again.",
                    main_menu_keyboard(),
                )
                .await;
            }
        }
    };

    let build_mode = match draft.build_mode {
        BuildModeDraft::Template => crate::models::BuildMode::Template,
        BuildModeDraft::AiFree => crate::models::BuildMode::AiFree,
    };
    let template_id = draft.template_id.as_deref();
    let order_id = if let Some(order_id) = draft.order_id {
        order_id
    } else {
        let order_id = match db::get_or_create_site_order(
            pool,
            db_user_id,
            build_mode,
            template_id,
            total,
            draft.domain_quote_id,
        )
        .await
        {
            Ok(id) => id,
            Err(e) => {
                warn!(?e, "failed to create site order");
                return edit_screen(
                    bot,
                    target,
                    "<b>Order failed.</b>\nPlease try again.",
                    main_menu_keyboard(),
                )
                .await;
            }
        };
        draft.order_id = Some(order_id);
        info!(
            user_id = user_id.0 as i64,
            order_id = %order_id,
            total_usdt = %total,
            chain = chain.as_str(),
            "site order created before payment"
        );
        set_session(user_id, draft.clone(), sessions);
        order_id
    };

    let Some(input) = website_input_from_draft(&draft) else {
        return edit_screen(
            bot,
            target,
            "<b>Payment not ready.</b>\nComplete all steps first.",
            cancel_keyboard(),
        )
        .await;
    };
    if let Err(error) =
        db::prepare_order_for_payment(pool, order_id, total, draft.domain_quote_id).await
    {
        warn!(
            ?error,
            user_id = user_id.0 as i64,
            order_id = %order_id,
            "failed to update order total and domain quote before payment"
        );
        return edit_screen(
            bot,
            target,
            "<b>Payment setup failed.</b>\nUse /create.",
            main_menu_keyboard(),
        )
        .await;
    }
    let website_id = match db::upsert_website_for_order(pool, db_user_id, order_id, &input).await {
        Ok(id) => {
            info!(
                user_id = user_id.0 as i64,
                order_id = %order_id,
                website_id = %id,
                domain = input.requested_domain.as_deref().unwrap_or("<none>"),
                "website input persisted before payment"
            );
            id
        }
        Err(error) => {
            warn!(?error, order_id = %order_id, "failed to persist website input before payment");
            return edit_screen(
                bot,
                target,
                "<b>Order save failed.</b>\nPlease try again.",
                main_menu_keyboard(),
            )
            .await;
        }
    };
    if let Some(quote_id) = draft.domain_quote_id
        && let Err(error) = db::attach_quote_website(pool, quote_id, website_id).await
    {
        warn!(?error, quote_id = %quote_id, website_id = %website_id, "failed to attach quote to website");
    }

    let payment_window = match db::attach_payment_chain(pool, order_id, chain).await {
        Ok(window) => {
            info!(
                user_id = user_id.0 as i64,
                order_id = %order_id,
                chain = chain.as_str(),
                payment_started_at = %window.started_at,
                payment_expires_at = %window.expires_at,
                "payment window started"
            );
            window
        }
        Err(e) => {
            warn!(?e, order_id = %order_id, "failed to attach payment chain");
            return edit_screen(
                bot,
                target,
                "<b>Payment start failed.</b>\nPlease try again.",
                main_menu_keyboard(),
            )
            .await;
        }
    };

    draft.payment_chain = Some(chain);
    draft.step = DraftStep::WaitingTx;
    draft.order_id = Some(order_id);
    draft.db_user_id = Some(db_user_id);
    info!(
        user_id = user_id.0 as i64,
        order_id = %order_id,
        chain = chain.as_str(),
        next_step = DraftStep::WaitingTx.as_str(),
        "waiting for payment transaction"
    );
    set_session(user_id, draft, sessions);

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
    sessions: &Sessions,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    let Some(mut draft) = get_session(user_id, sessions) else {
        return edit_screen(
            bot,
            target,
            "<b>No active order.</b>\nUse /create.",
            main_menu_keyboard(),
        )
        .await;
    };
    if draft.is_expired() {
        cancel_user_order(pool, user_id, sessions).await;
        return edit_screen(
            bot,
            target,
            "<b>Order expired.</b>\nUse /create.",
            main_menu_keyboard(),
        )
        .await;
    }

    let Some(alternatives) = &draft.alternative_domains else {
        return edit_screen(
            bot,
            target,
            "<b>Session expired.</b>\nUse /create.",
            main_menu_keyboard(),
        )
        .await;
    };

    let Some(quote) = alternatives.iter().find(|q| q.domain == domain).cloned() else {
        return edit_screen(
            bot,
            target,
            "<b>Domain expired.</b>\nUse /create.",
            main_menu_keyboard(),
        )
        .await;
    };

    let site_price_usdt = draft.site_price_usdt;

    let db_user_id = match db::upsert_user(pool, user_id.0 as i64, None, None, None, true).await {
        Ok(id) => id,
        Err(e) => {
            warn!(
                ?e,
                user_id = user_id.0 as i64,
                "failed to upsert user for alt domain"
            );
            return edit_screen(
                bot,
                target,
                "<b>Selection failed.</b>\nPlease try again.",
                main_menu_keyboard(),
            )
            .await;
        }
    };

    let domain_quote_id =
        match db::record_domain_quote(pool, db_user_id, &quote, quote.price_usd).await {
            Ok(id) => id,
            Err(e) => {
                warn!(?e, "failed to record domain quote for alt domain");
                return edit_screen(
                    bot,
                    target,
                    "<b>Selection failed.</b>\nPlease try again.",
                    main_menu_keyboard(),
                )
                .await;
            }
        };

    let total = site_price_usdt + quote.price_usd;
    if let Some(order_id) = draft.order_id
        && let Err(error) =
            db::prepare_order_for_payment(pool, order_id, total, Some(domain_quote_id)).await
    {
        warn!(
            ?error,
            user_id = user_id.0 as i64,
            order_id = %order_id,
            quote_id = %domain_quote_id,
            "failed to attach alternative domain quote to pending order"
        );
        return edit_screen(
            bot,
            target,
            "<b>Quote save failed.</b>\nPlease try again.",
            main_menu_keyboard(),
        )
        .await;
    }
    draft.website_url = Some(domain.to_string());
    draft.db_user_id = Some(db_user_id);
    draft.domain_price_usdt = Some(quote.price_usd);
    draft.domain_quote_id = Some(domain_quote_id);
    draft.total_usdt = Some(total);
    draft.step = DraftStep::ReadyToPay;
    draft.alternative_domains = None;
    set_session(user_id, draft.clone(), sessions);

    edit_screen(
        bot,
        target,
        format!(
            "✅ <b>Domain selected</b>\n{domain}\nDomain: <b>{}</b> USDT\nWebsite: <b>{}</b> USDT\nTotal: <b>{}</b> USDT\n\nChoose payment chain.",
            quote.price_usd, site_price_usdt, total
        ),
        payment_chain_keyboard(),
    )
    .await
}
