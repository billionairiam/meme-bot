use std::sync::Arc;

use sqlx::SqlitePool;
use teloxide::prelude::*;
use teloxide::types::{CallbackQuery, ChatId, Message, ParseMode, Recipient, UserId};
use tracing::{info, warn};

use crate::{
    api_client::HttpClientDomainRegistrar,
    db,
    deployment::github::{RepoTextEditRequest, replace_repo_text_and_push},
    domain::DomainRegistrar,
    models::{BuildMode, Order, OrderStatus, PaymentChain, TokenChain},
    validation::{
        DEFAULT_CONTRACT_ADDRESS, DEFAULT_TELEGRAM_URL, DEFAULT_X_URL,
        validate_contract_address_for_chain, validate_domain, validate_telegram_url,
        validate_x_url,
    },
};

use super::domain_flow::DomainFlow;
use super::helpers::*;
use super::keyboards::Keyboards;
use super::payment::PaymentTxProcess;
use super::screens::Screens;
use super::types::*;

fn users(pool: &SqlitePool) -> db::UserStore {
    db::UserStore::new(pool.clone())
}

fn orders(pool: &SqlitePool) -> db::OrderStore {
    db::OrderStore::new(pool.clone())
}

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
        return Screens::new(&bot)
            .show_main_menu(msg.chat.id, &config)
            .await;
    };
    let user_id = user.id;
    let current_order = orders(&pool)
        .get_unpaid(user_id.0 as i64)
        .await
        .ok()
        .flatten();
    let current_step = current_order.as_ref().map(|o| o.status.as_str());
    info!(
        user_id = user_id.0 as i64,
        chat_id = %msg.chat.id,
        message_kind = message_kind(&msg),
        order_status = current_step.unwrap_or("none"),
        "telegram message received"
    );
    expire_stale_orders_for_user(&pool, user_id).await;

    if let Some(text) = msg.text() {
        if let Some(template_id) = parse_template_deep_link(text) {
            return Screens::new(&bot)
                .show_template_confirmation(msg.chat.id, template_id, &config)
                .await;
        }

        match text.trim() {
            "/start" | "/help" => {
                return Screens::new(&bot)
                    .show_main_menu(msg.chat.id, &config)
                    .await;
            }
            "/create" => {
                return Screens::new(&bot)
                    .show_create_menu(
                        msg.chat.id,
                        &config.template_prices_usdt,
                        config.ai_creation_price_usdt,
                    )
                    .await;
            }
            "/status" => {
                return Screens::new(&bot)
                    .show_status(msg.chat.id, user_id, &pool)
                    .await;
            }
            "/history" => {
                return Screens::new(&bot)
                    .show_history(msg.chat.id, user_id, &pool)
                    .await;
            }
            "/edit" => {
                return Screens::new(&bot)
                    .show_edit_list(msg.chat.id, user_id, &pool)
                    .await;
            }
            "/continue" => {
                return handle_continue_command(&bot, msg.chat.id, user_id, &pool, &config).await;
            }
            "/cancel" => {
                cancel_user_order(&pool, user_id).await;
                return bot
                    .send_message(msg.chat.id, "<b>Order cancelled.</b>")
                    .parse_mode(ParseMode::Html)
                    .reply_markup(Keyboards::main_menu())
                    .await
                    .map(|_| ());
            }
            _ => {}
        }
    }

    if current_order.is_none()
        && handle_edit_message(
            &bot,
            msg.chat.id,
            msg.text().map(str::trim),
            user_id,
            &config,
            &pool,
        )
        .await?
    {
        return Ok(());
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
    info!(
        user_id = query.from.id.0 as i64,
        chat_id = %chat_id,
        callback = query.data.as_deref().unwrap_or("<none>"),
        "telegram callback received"
    );

    if !ensure_group_member_for_user(&bot, chat_id, query.from.id, &config).await? {
        return Ok(());
    }
    expire_stale_orders_for_user(&pool, query.from.id).await;

    match query.data.as_deref() {
        Some("main") => Screens::new(&bot).edit_main_menu(target, &config).await,
        Some("create") => {
            Screens::new(&bot)
                .edit_create_menu(
                    target,
                    &config.template_prices_usdt,
                    config.ai_creation_price_usdt,
                )
                .await
        }
        Some("template_mode") => Screens::new(&bot).edit_template_mode(target, &config).await,
        Some(data) if data.starts_with("tpl_page:") => {
            let page = data
                .trim_start_matches("tpl_page:")
                .parse::<usize>()
                .unwrap_or(0);
            Screens::new(&bot)
                .edit_template_mode_page(target, &config, page)
                .await
        }
        Some("ai_mode") => {
            Screens::new(&bot).clear_screen_keyboard(target).await?;
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
                    Screens::new(&bot).edit_choose_chain(target).await
                }
                Err(error) => {
                    let msg = if error.to_string().contains("pending order") {
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
                    Screens::new(&bot)
                        .edit_screen(target, msg, Keyboards::main_menu())
                        .await
                }
            }
        }
        Some("pay_menu") => Screens::new(&bot).edit_payment_chain_menu(target).await,
        Some("status") => {
            Screens::new(&bot)
                .edit_status(target, query.from.id, &pool)
                .await
        }
        Some("history") => {
            Screens::new(&bot)
                .edit_history(target, query.from.id, &pool)
                .await
        }
        Some("edit") => {
            Screens::new(&bot)
                .edit_edit_list(target, query.from.id, &pool)
                .await
        }
        Some("contact_admin") => Screens::new(&bot).edit_contact_admin(target, &config).await,
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
            Screens::new(&bot)
                .edit_template_confirmation(target, template_id, &config)
                .await
        }
        Some(data) if data.starts_with("confirm_tpl:") => {
            match TemplateOption::find(
                &config.template_options,
                data.trim_start_matches("confirm_tpl:"),
            ) {
                Some(template) => {
                    Screens::new(&bot).clear_screen_keyboard(target).await?;
                    let price = TemplateOption::price_by_id(
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
                            Screens::new(&bot).edit_choose_chain(target).await
                        }
                        Err(error) => {
                            let msg = if error.to_string().contains("pending order") {
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
                            Screens::new(&bot)
                                .edit_screen(target, msg, Keyboards::main_menu())
                                .await
                        }
                    }
                }
                None => {
                    Screens::new(&bot)
                        .edit_screen(
                            target,
                            "<b>Template not found.</b>\nChoose again.",
                            Keyboards::template(
                                &config.template_options,
                                &config.template_prices_usdt,
                            ),
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
        Some(data) if data.starts_with("token_chain:") => {
            handle_token_chain_selection(
                &bot,
                target,
                query.from.id,
                data.trim_start_matches("token_chain:"),
                &pool,
            )
            .await
        }
        Some(data) if data.starts_with("edit_select:") => {
            handle_edit_select(
                &bot,
                target,
                query.from.id,
                data.trim_start_matches("edit_select:"),
                &pool,
            )
            .await
        }
        Some(data) if data.starts_with("edit_field:") => {
            handle_edit_field_select(
                &bot,
                target,
                query.from.id,
                data.trim_start_matches("edit_field:"),
                &pool,
            )
            .await
        }
        Some(data) if data.starts_with("edit_apply:") => {
            handle_edit_apply(
                &bot,
                target,
                query.from.id,
                data.trim_start_matches("edit_apply:"),
                &config,
                &pool,
            )
            .await
        }
        Some(data) if data.starts_with("edit_cancel:") => {
            handle_edit_cancel(
                &bot,
                target,
                query.from.id,
                data.trim_start_matches("edit_cancel:"),
                &pool,
            )
            .await
        }
        Some(data) if data.starts_with("skip_optional:") => {
            let field = data.trim_start_matches("skip_optional:");
            handle_optional_step_skip(&bot, target, query.from.id, field, &pool).await
        }
        Some("alt_domain_custom") => {
            Screens::new(&bot)
                .edit_screen(
                    target,
                    "✏️ <b>Domain</b>\nExample: <code>pepe.xyz</code>",
                    Keyboards::cancel(),
                )
                .await
        }
        Some("cancel") => {
            cancel_user_order(&pool, query.from.id).await;
            Screens::new(&bot)
                .edit_screen(target, "🛑 <b>Order cancelled</b>", Keyboards::main_menu())
                .await
        }
        _ => Screens::new(&bot).edit_main_menu(target, &config).await,
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
    let db_user_id = users(pool)
        .upsert(
            telegram_user_id.0 as i64,
            username,
            first_name,
            last_name,
            true,
        )
        .await?;

    if orders(pool).has_unpaid(telegram_user_id.0 as i64).await? {
        anyhow::bail!("has pending order");
    }

    let order_id = orders(pool)
        .create(db_user_id, build_mode, template_id, site_price_usdt)
        .await?;
    info!(
        user_id = telegram_user_id.0 as i64,
        order_id = %order_id,
        site_price_usdt = %site_price_usdt,
        "order created"
    );
    Ok(order_id)
}

async fn expire_stale_orders_for_user(pool: &SqlitePool, user_id: UserId) {
    match orders(pool)
        .expire_stale_pending_for_user(user_id.0 as i64)
        .await
    {
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
    match orders(pool).cancel_for_user(user_id.0 as i64, None).await {
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
    let order = match orders(pool).get_unpaid(user_id.0 as i64).await {
        Ok(Some(order)) => order,
        Ok(None) => {
            // No active order — check if it's a TX hash for NeedPay order
            if let Some(text) = msg.text().map(str::trim)
                && looks_like_tx_hash(text)
            {
                // Already checked above via get_unpaid_order, no match
            }
            return Screens::new(&bot)
                .show_main_menu(msg.chat.id, &config)
                .await;
        }
        Err(error) => {
            warn!(?error, user_id = user_id.0 as i64, "failed to load order");
            return Screens::new(&bot)
                .show_main_menu(msg.chat.id, &config)
                .await;
        }
    };
    let order_id = order.id;

    match order.status {
        OrderStatus::NeedChain => bot
            .send_message(
                msg.chat.id,
                "⛓ <b>1/8 Chain</b>\nChoose the meme website chain.",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(Keyboards::token_chain())
            .await
            .map(|_| ()),
        OrderStatus::NeedLogo => {
            let Some(photo) = msg.photo().and_then(|photos| photos.last()) else {
                return bot
                    .send_message(msg.chat.id, "🖼 <b>2/8 Logo</b>\nUpload token logo.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            let file_id = photo.file.id.0.clone();
            let temp_id = uuid::Uuid::new_v4().to_string();
            match download_telegram_logo(&bot, &file_id, &temp_id).await {
                Ok(local_path) => {
                    if let Err(e) = orders(pool)
                        .update_field_and_status(
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
                        "✅ Logo saved.\n🔤 <b>3/8 Ticker</b>\nExample: <code>PEPE</code>",
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
                    .send_message(msg.chat.id, "🔤 <b>3/8 Ticker</b>\nSend ticker.")
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
            if let Err(e) = orders(pool)
                .update_field_and_status(order_id, "token_ticker", &ticker, OrderStatus::NeedName)
                .await
            {
                warn!(?e, %order_id, "failed to save ticker");
            }
            bot.send_message(
                msg.chat.id,
                "✅ Ticker saved.\n🏷 <b>4/8 Name</b>\nExample: <code>Pepe Coin</code>",
            )
            .parse_mode(ParseMode::Html)
            .await
            .map(|_| ())
        }
        OrderStatus::NeedName => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(msg.chat.id, "🏷 <b>4/8 Name</b>\nSend token name.")
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
            if let Err(e) = orders(pool)
                .update_field_and_status(order_id, "token_name", text, OrderStatus::NeedContract)
                .await
            {
                warn!(?e, %order_id, "failed to save token name");
            }
            bot.send_message(
                msg.chat.id,
                "✅ Name saved.\n🔗 <b>5/8 Contract</b>\nSend contract address or tap Skip.",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(Keyboards::optional_step("contract"))
            .await
            .map(|_| ())
        }
        OrderStatus::NeedContract => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(
                        msg.chat.id,
                        "🔗 <b>5/8 Contract</b>\nSend contract address or tap Skip.",
                    )
                    .parse_mode(ParseMode::Html)
                    .reply_markup(Keyboards::optional_step("contract"))
                    .await
                    .map(|_| ());
            };
            let Some(chain) = order.token_chain else {
                return bot
                    .send_message(msg.chat.id, "⛓ <b>Chain missing.</b>\nUse /continue.")
                    .parse_mode(ParseMode::Html)
                    .reply_markup(Keyboards::token_chain())
                    .await
                    .map(|_| ());
            };
            if validate_contract_address_for_chain(chain, text).is_err() {
                return bot
                    .send_message(
                        msg.chat.id,
                        "<b>Invalid contract.</b>\nSend a contract for the selected chain, or tap Skip.",
                    )
                    .parse_mode(ParseMode::Html)
                    .reply_markup(Keyboards::optional_step("contract"))
                    .await
                    .map(|_| ());
            }
            if let Err(e) = orders(pool)
                .update_field_and_status(order_id, "contract_address", text, OrderStatus::NeedXLink)
                .await
            {
                warn!(?e, %order_id, "failed to save contract");
            }
            bot.send_message(
                msg.chat.id,
                "✅ Contract saved.\n✕ <b>6/8 X</b>\n<code>https://x.com/yourproject</code>\nSend X link or tap Skip.",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(Keyboards::optional_step("x"))
            .await
            .map(|_| ())
        }
        OrderStatus::NeedXLink => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(msg.chat.id, "✕ <b>6/8 X</b>\nSend X link or tap Skip.")
                    .parse_mode(ParseMode::Html)
                    .reply_markup(Keyboards::optional_step("x"))
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
                    .reply_markup(Keyboards::optional_step("x"))
                    .await
                    .map(|_| ());
            }
            if let Err(e) = orders(pool)
                .update_field_and_status(order_id, "x_url", text, OrderStatus::NeedTme)
                .await
            {
                warn!(?e, %order_id, "failed to save x_url");
            }
            bot.send_message(
                msg.chat.id,
                "✅ X saved.\n💬 <b>7/8 Telegram</b>\n<code>https://t.me/yourgroup</code>\nSend Telegram link or tap Skip.",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(Keyboards::optional_step("telegram"))
            .await
            .map(|_| ())
        }
        OrderStatus::NeedTme => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(
                        msg.chat.id,
                        "💬 <b>7/8 Telegram</b>\nSend Telegram link or tap Skip.",
                    )
                    .parse_mode(ParseMode::Html)
                    .reply_markup(Keyboards::optional_step("telegram"))
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
                    .reply_markup(Keyboards::optional_step("telegram"))
                    .await
                    .map(|_| ());
            }
            if let Err(e) = orders(pool)
                .update_field_and_status(order_id, "telegram_url", text, OrderStatus::NeedDomain)
                .await
            {
                warn!(?e, %order_id, "failed to save telegram_url");
            }
            bot.send_message(
                msg.chat.id,
                "✅ Telegram saved.\n🌐 <b>8/8 Domain</b>\nExample: <code>pepe.xyz</code>",
            )
            .parse_mode(ParseMode::Html)
            .await
            .map(|_| ())
        }
        OrderStatus::NeedDomain => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(msg.chat.id, "🌐 <b>8/8 Domain</b>\nSend domain.")
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
                    .reply_markup(Keyboards::payment_chain())
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
                    .reply_markup(Keyboards::payment_chain())
                    .await
                    .map(|_| ());
            }
            let chain = order.payment_chain.unwrap_or(PaymentChain::Erc20);
            PaymentTxProcess {
                bot: &bot,
                chat_id: msg.chat.id,
                user_id,
                config: &config,
                pool,
                order_id,
                chain,
                amount_usdt: order.total_usdt,
                tx_hash: text,
            }
            .process()
            .await
        }
        OrderStatus::PaidAndGenerating => bot
            .send_message(
                msg.chat.id,
                "⚙️ <b>Generating...</b>\nYour website is being created.",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(Keyboards::status())
            .await
            .map(|_| ()),
        _ => {
            Screens::new(&bot)
                .show_main_menu(msg.chat.id, &config)
                .await
        }
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
        if let Err(e) = orders(pool)
            .set_domain_pricing(order_id, rust_decimal::Decimal::ZERO, total)
            .await
        {
            warn!(?e, %order_id, "failed to set domain pricing");
        }
        if let Err(e) = orders(pool)
            .update_field_and_status(order_id, "requested_domain", domain, OrderStatus::NeedPay)
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
        .reply_markup(Keyboards::payment_chain())
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
    match DomainFlow::new(registrar.as_ref())
        .quote_with_retries(domain)
        .await
    {
        Ok(quote) if quote.available && !quote.is_premium => {
            let total = order.site_price_usdt + quote.price_usd;
            if let Err(e) = orders(pool)
                .set_domain_pricing(order_id, quote.price_usd, total)
                .await
            {
                warn!(?e, %order_id, "failed to set domain pricing");
                return bot
                    .send_message(chat_id, "<b>Quote save failed.</b>\nPlease try again.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }
            if let Err(e) = orders(pool)
                .update_field_and_status(
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
            .reply_markup(Keyboards::payment_chain())
            .await.map(|_| ())
        }
        Ok(_) | Err(_) => {
            info!(user_id = user_id.0 as i64, %domain, "domain unavailable; checking alternatives");
            let tld = domain.rsplit_once('.').map(|(_, t)| t).unwrap_or("");
            let base_name = domain
                .rsplit_once('.')
                .map(|(name, _)| name)
                .unwrap_or(domain);
            let alternatives = DomainFlow::new(registrar.as_ref())
                .check_alternatives(base_name, tld)
                .await;

            if alternatives.is_empty() {
                bot.send_message(chat_id, "<b>Domain unavailable.</b>\nSend another domain.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ())
            } else {
                // Store alternatives in DB
                if let Ok(json) = serde_json::to_string(&alternatives) {
                    let _ = orders(pool)
                        .update_field(order_id, "alternative_domains_json", &json)
                        .await;
                }
                bot.send_message(
                    chat_id,
                    format!("🔍 <b>{} is taken.</b>\nChoose one:", html_escape(domain)),
                )
                .parse_mode(ParseMode::Html)
                .reply_markup(Keyboards::alternative_domains(&alternatives))
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
    let order = match orders(pool).get_unpaid(user_id.0 as i64).await {
        Ok(Some(order)) => order,
        Ok(None) => {
            return Screens::new(bot)
                .edit_screen(
                    target,
                    "<b>No active order.</b>\nUse /create.",
                    Keyboards::main_menu(),
                )
                .await;
        }
        Err(e) => {
            warn!(
                ?e,
                user_id = user_id.0 as i64,
                "failed to load order for optional skip"
            );
            return Screens::new(bot)
                .edit_screen(target, "<b>Error.</b>\nTry again.", Keyboards::main_menu())
                .await;
        }
    };

    let (expected_status, db_field, value, next_status, next_text, next_keyboard) = match field {
        "contract" => (
            OrderStatus::NeedContract,
            "contract_address",
            DEFAULT_CONTRACT_ADDRESS,
            OrderStatus::NeedXLink,
            "⏭ Contract skipped.\n✕ <b>6/8 X</b>\nSend X link or tap Skip.",
            Keyboards::optional_step("x"),
        ),
        "x" => (
            OrderStatus::NeedXLink,
            "x_url",
            DEFAULT_X_URL,
            OrderStatus::NeedTme,
            "⏭ X skipped.\n💬 <b>7/8 Telegram</b>\nSend Telegram link or tap Skip.",
            Keyboards::optional_step("telegram"),
        ),
        "telegram" => (
            OrderStatus::NeedTme,
            "telegram_url",
            DEFAULT_TELEGRAM_URL,
            OrderStatus::NeedDomain,
            "⏭ Telegram skipped.\n🌐 <b>8/8 Domain</b>\nExample: <code>pepe.xyz</code>",
            Keyboards::cancel(),
        ),
        other => {
            warn!(field = other, "unknown optional skip field");
            return Screens::new(bot)
                .edit_screen(target, "<b>Unknown action.</b>", Keyboards::main_menu())
                .await;
        }
    };

    if order.status != expected_status {
        return Screens::new(bot)
            .edit_screen(
                target,
                "<b>This step is no longer active.</b>\nUse /continue.",
                Keyboards::main_menu(),
            )
            .await;
    }

    if let Err(e) = orders(pool)
        .update_field_and_status(order.id, db_field, value, next_status)
        .await
    {
        warn!(?e, order_id = %order.id, field, "failed to skip optional field");
        return Screens::new(bot)
            .edit_screen(
                target,
                "<b>Save failed.</b>\nPlease try again.",
                Keyboards::cancel(),
            )
            .await;
    }

    Screens::new(bot)
        .edit_screen(target, next_text, next_keyboard)
        .await
}

async fn handle_token_chain_selection(
    bot: &Bot,
    target: UiMessage,
    user_id: UserId,
    value: &str,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    let Some(chain) = TokenChain::from_db(value) else {
        return Screens::new(bot)
            .edit_screen(target, "<b>Unknown chain.</b>", Keyboards::token_chain())
            .await;
    };
    let order = match orders(pool).get_unpaid(user_id.0 as i64).await {
        Ok(Some(order)) => order,
        _ => {
            return Screens::new(bot)
                .edit_screen(
                    target,
                    "<b>No active order.</b>\nUse /create.",
                    Keyboards::main_menu(),
                )
                .await;
        }
    };
    if order.status != OrderStatus::NeedChain {
        return Screens::new(bot)
            .edit_screen(
                target,
                "<b>This step is no longer active.</b>\nUse /continue.",
                Keyboards::main_menu(),
            )
            .await;
    }
    if let Err(error) = orders(pool)
        .update_field_and_status(
            order.id,
            "token_chain",
            chain.as_str(),
            OrderStatus::NeedLogo,
        )
        .await
    {
        warn!(?error, order_id = %order.id, "failed to save token chain");
        return Screens::new(bot)
            .edit_screen(target, "<b>Save failed.</b>", Keyboards::cancel())
            .await;
    }
    Screens::new(bot)
        .edit_screen(
            target,
            format!(
                "✅ Chain saved: <b>{}</b>\n🖼 <b>2/8 Logo</b>\nUpload token logo.",
                chain.display_label()
            ),
            Keyboards::cancel(),
        )
        .await
}

async fn handle_edit_select(
    bot: &Bot,
    target: UiMessage,
    user_id: UserId,
    order_id: &str,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    let Ok(order_id) = uuid::Uuid::parse_str(order_id) else {
        return Screens::new(bot)
            .edit_screen(target, "<b>Invalid order.</b>", Keyboards::main_menu())
            .await;
    };
    let order = match orders(pool)
        .get_completed_for_user(user_id.0 as i64, order_id)
        .await
    {
        Ok(Some(order)) => order,
        _ => {
            return Screens::new(bot)
                .edit_screen(target, "<b>Website not found.</b>", Keyboards::main_menu())
                .await;
        }
    };
    if order.edit_status == "none"
        && let Err(error) = orders(pool).start_edit_draft(order_id).await
    {
        warn!(?error, %order_id, "failed to start edit draft");
        return Screens::new(bot)
            .edit_screen(target, "<b>Edit failed.</b>", Keyboards::main_menu())
            .await;
    }
    let order = orders(pool)
        .get(order_id)
        .await
        .ok()
        .flatten()
        .unwrap_or(order);
    let free_used = orders(pool)
        .user_edit_success_count(user_id.0 as i64)
        .await
        .unwrap_or(0);
    Screens::new(bot)
        .edit_screen(
            target,
            Screens::edit_order_text(&order, free_used),
            Keyboards::edit_menu(order_id),
        )
        .await
}

async fn handle_edit_field_select(
    bot: &Bot,
    target: UiMessage,
    user_id: UserId,
    data: &str,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    let mut parts = data.split(':');
    let Some(order_id) = parts.next().and_then(|v| uuid::Uuid::parse_str(v).ok()) else {
        return Screens::new(bot)
            .edit_screen(target, "<b>Invalid order.</b>", Keyboards::main_menu())
            .await;
    };
    let Some(field) = parts.next() else {
        return Screens::new(bot)
            .edit_screen(target, "<b>Invalid field.</b>", Keyboards::main_menu())
            .await;
    };
    if !matches!(field, "contract" | "x" | "telegram") {
        return Screens::new(bot)
            .edit_screen(target, "<b>Invalid field.</b>", Keyboards::main_menu())
            .await;
    }
    match orders(pool)
        .get_completed_for_user(user_id.0 as i64, order_id)
        .await
    {
        Ok(Some(_)) => {}
        _ => {
            return Screens::new(bot)
                .edit_screen(target, "<b>Website not found.</b>", Keyboards::main_menu())
                .await;
        }
    }
    if let Err(error) = orders(pool)
        .set_pending_edit_field(order_id, Some(field))
        .await
    {
        warn!(?error, %order_id, field, "failed to select edit field");
        return Screens::new(bot)
            .edit_screen(target, "<b>Edit failed.</b>", Keyboards::main_menu())
            .await;
    }
    let prompt = match field {
        "contract" => "Send new contract address.",
        "x" => "Send new X URL.",
        "telegram" => "Send new Telegram URL.",
        _ => unreachable!(),
    };
    Screens::new(bot)
        .edit_screen(
            target,
            format!("✏️ <b>Edit</b>\n{prompt}"),
            Keyboards::cancel(),
        )
        .await
}

async fn handle_edit_cancel(
    bot: &Bot,
    target: UiMessage,
    user_id: UserId,
    order_id: &str,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    let Ok(order_id) = uuid::Uuid::parse_str(order_id) else {
        return Screens::new(bot)
            .edit_screen(target, "<b>Invalid order.</b>", Keyboards::main_menu())
            .await;
    };
    if orders(pool)
        .get_completed_for_user(user_id.0 as i64, order_id)
        .await
        .ok()
        .flatten()
        .is_some()
    {
        let _ = orders(pool).clear_edit_draft(order_id).await;
    }
    Screens::new(bot)
        .edit_screen(target, "🛑 <b>Edit cancelled.</b>", Keyboards::main_menu())
        .await
}

async fn handle_edit_apply(
    bot: &Bot,
    target: UiMessage,
    user_id: UserId,
    order_id: &str,
    config: &TelegramUiConfig,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    let Ok(order_id) = uuid::Uuid::parse_str(order_id) else {
        return Screens::new(bot)
            .edit_screen(target, "<b>Invalid order.</b>", Keyboards::main_menu())
            .await;
    };
    let order = match orders(pool)
        .get_completed_for_user(user_id.0 as i64, order_id)
        .await
    {
        Ok(Some(order)) => order,
        _ => {
            return Screens::new(bot)
                .edit_screen(target, "<b>Website not found.</b>", Keyboards::main_menu())
                .await;
        }
    };
    match dry_run_edit(&order, config).await {
        Ok(0) => {
            let _ = orders(pool).clear_edit_draft(order_id).await;
            return Screens::new(bot)
                .edit_screen(target, "<b>No changes.</b>", Keyboards::main_menu())
                .await;
        }
        Ok(_) => {}
        Err(error) => {
            warn!(?error, %order_id, "edit dry-run failed");
            let _ = orders(pool).mark_edit_manual_review(order_id).await;
            return Screens::new(bot)
                .edit_screen(
                    target,
                    "<b>Edit needs manual review.</b>\nContact admin.",
                    Keyboards::main_menu(),
                )
                .await;
        }
    }

    let free_used = orders(pool)
        .user_edit_success_count(user_id.0 as i64)
        .await
        .unwrap_or(0);
    if free_used < db::FREE_EDIT_LIMIT {
        return apply_edit_and_reply(bot, target, &order, config, pool).await;
    }

    if let Err(error) = orders(pool).mark_edit_need_pay(order_id).await {
        warn!(?error, %order_id, "failed to mark edit need pay");
        return Screens::new(bot)
            .edit_screen(target, "<b>Edit failed.</b>", Keyboards::main_menu())
            .await;
    }
    Screens::new(bot).edit_screen(
        target,
        format!(
            "💳 <b>Edit fee required</b>\nFree edits used: <b>{}</b>\nAmount: <b>{}</b> USDT\nChoose payment chain.",
            free_used,
            db::EDIT_PRICE_USDT
        ),
        Keyboards::payment_chain(),
    )
    .await
}

async fn handle_edit_message(
    bot: &Bot,
    chat_id: ChatId,
    text: Option<&str>,
    user_id: UserId,
    config: &TelegramUiConfig,
    pool: &SqlitePool,
) -> ResponseResult<bool> {
    let Some(mut order) = orders(pool)
        .get_active_edit(user_id.0 as i64)
        .await
        .ok()
        .flatten()
    else {
        return Ok(false);
    };
    let Some(text) = text else {
        return Ok(false);
    };

    if let Some(field) = order.pending_edit_field.clone() {
        let validation = match field.as_str() {
            "contract" => order
                .token_chain
                .map(|chain| validate_contract_address_for_chain(chain, text))
                .unwrap_or(Err(
                    crate::validation::ValidationError::InvalidContractAddress,
                )),
            "x" => validate_x_url(text),
            "telegram" => validate_telegram_url(text),
            _ => Err(crate::validation::ValidationError::InvalidDomain),
        };
        if validation.is_err() {
            bot.send_message(chat_id, "<b>Invalid value.</b>\nSend a valid value.")
                .parse_mode(ParseMode::Html)
                .await?;
            return Ok(true);
        }
        if let Err(error) = orders(pool)
            .update_pending_edit_value(order.id, &field, text)
            .await
        {
            warn!(?error, order_id = %order.id, field, "failed to save edit value");
            bot.send_message(chat_id, "<b>Save failed.</b>")
                .parse_mode(ParseMode::Html)
                .await?;
            return Ok(true);
        }
        order = orders(pool)
            .get(order.id)
            .await
            .ok()
            .flatten()
            .unwrap_or(order);
        let free_used = orders(pool)
            .user_edit_success_count(user_id.0 as i64)
            .await
            .unwrap_or(0);
        bot.send_message(chat_id, Screens::edit_order_text(&order, free_used))
            .parse_mode(ParseMode::Html)
            .reply_markup(Keyboards::edit_menu(order.id))
            .await?;
        return Ok(true);
    }

    if order.edit_status == "need_pay" && looks_like_tx_hash(text) {
        process_edit_payment_tx(bot, chat_id, user_id, &order, config, pool, text).await?;
        return Ok(true);
    }

    Ok(false)
}

async fn dry_run_edit(
    order: &Order,
    config: &TelegramUiConfig,
) -> Result<usize, crate::deployment::DeploymentError> {
    apply_repo_edit(order, config, true).await
}

async fn apply_repo_edit(
    order: &Order,
    config: &TelegramUiConfig,
    dry_run: bool,
) -> Result<usize, crate::deployment::DeploymentError> {
    let fallback_repo_name = format!("site-{}", &order.id.simple().to_string()[..8]);
    let repo_name = order
        .requested_domain
        .as_deref()
        .unwrap_or(&fallback_repo_name);
    replace_repo_text_and_push(RepoTextEditRequest {
        repo_owner: &config.github_owner,
        repo_name,
        github_token: &config.github_token,
        production_branch: "main",
        generated_sites_dir: &config.generated_sites_dir,
        replacements: vec![
            (
                order
                    .contract_address
                    .as_deref()
                    .unwrap_or(DEFAULT_CONTRACT_ADDRESS),
                order
                    .pending_contract_address
                    .as_deref()
                    .unwrap_or(DEFAULT_CONTRACT_ADDRESS),
            ),
            (
                order.x_url.as_deref().unwrap_or(DEFAULT_X_URL),
                order.pending_x_url.as_deref().unwrap_or(DEFAULT_X_URL),
            ),
            (
                order
                    .telegram_url
                    .as_deref()
                    .unwrap_or(DEFAULT_TELEGRAM_URL),
                order
                    .pending_telegram_url
                    .as_deref()
                    .unwrap_or(DEFAULT_TELEGRAM_URL),
            ),
        ],
        dry_run,
    })
    .await
}

async fn apply_edit_and_reply(
    bot: &Bot,
    target: UiMessage,
    order: &Order,
    config: &TelegramUiConfig,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    if let Err(error) = orders(pool).mark_edit_applying(order.id).await {
        warn!(?error, order_id = %order.id, "failed to mark edit applying");
        return Screens::new(bot)
            .edit_screen(target, "<b>Edit failed.</b>", Keyboards::main_menu())
            .await;
    }
    match apply_repo_edit(order, config, false).await {
        Ok(_) => {
            let contract = order
                .pending_contract_address
                .as_deref()
                .unwrap_or(DEFAULT_CONTRACT_ADDRESS);
            let x = order.pending_x_url.as_deref().unwrap_or(DEFAULT_X_URL);
            let telegram = order
                .pending_telegram_url
                .as_deref()
                .unwrap_or(DEFAULT_TELEGRAM_URL);
            if let Err(error) = orders(pool)
                .mark_edit_success_and_update_fields(order.id, contract, x, telegram)
                .await
            {
                warn!(?error, order_id = %order.id, "failed to record successful edit");
                return Screens::new(bot)
                    .edit_screen(
                        target,
                        "<b>Edit pushed, but DB update failed.</b>\nContact admin.",
                        Keyboards::main_menu(),
                    )
                    .await;
            }
            Screens::new(bot)
                .edit_screen(
                    target,
                    "✅ <b>Edit pushed.</b>\nCloudflare Pages will update from GitHub.",
                    Keyboards::main_menu(),
                )
                .await
        }
        Err(error) => {
            warn!(?error, order_id = %order.id, "failed to apply repo edit");
            let _ = orders(pool).mark_edit_manual_review(order.id).await;
            Screens::new(bot)
                .edit_screen(
                    target,
                    "<b>Edit needs manual review.</b>\nContact admin.",
                    Keyboards::main_menu(),
                )
                .await
        }
    }
}

async fn process_edit_payment_tx(
    bot: &Bot,
    chat_id: ChatId,
    user_id: UserId,
    order: &Order,
    config: &TelegramUiConfig,
    pool: &SqlitePool,
    tx_hash: &str,
) -> ResponseResult<()> {
    let chain = order.edit_payment_chain.unwrap_or(PaymentChain::Erc20);
    match orders(pool).tx_hash_already_used(chain, tx_hash).await {
        Ok(true) => {
            return bot
                .send_message(chat_id, "<b>TX already used.</b>\nSend a new TX.")
                .parse_mode(ParseMode::Html)
                .await
                .map(|_| ());
        }
        Ok(false) => {}
        Err(error) => warn!(?error, "failed to check edit tx reuse"),
    }

    if !config.skip_payment_verification {
        let expected_to_address = config.payment_address_for_chain(chain).trim().to_string();
        let Some(expires_at) = order.edit_payment_expires_at else {
            return bot
                .send_message(
                    chat_id,
                    "<b>Payment window missing.</b>\nChoose chain again.",
                )
                .parse_mode(ParseMode::Html)
                .reply_markup(Keyboards::payment_chain())
                .await
                .map(|_| ());
        };
        let verifier = crate::payment::ChainPaymentVerifier::new(config.shared_config());
        let request = crate::payment::PaymentVerificationRequest {
            order_id: order.id,
            chain,
            tx_hash: tx_hash.to_string(),
            expected_to_address,
            required_amount_usdt: rust_decimal::Decimal::from(db::EDIT_PRICE_USDT),
            payment_started_at: expires_at - chrono::Duration::minutes(db::PAYMENT_WINDOW_MINUTES),
            submitted_at: chrono::Utc::now(),
            expires_at,
        };
        if let Err(error) = super::payment::verify_payment_with_retries(&verifier, request).await {
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

    if let Err(error) = orders(pool).mark_edit_paid(order.id, chain, tx_hash).await {
        warn!(?error, order_id = %order.id, "failed to record edit payment");
        return bot
            .send_message(chat_id, "<b>Payment record failed.</b>\nContact admin.")
            .parse_mode(ParseMode::Html)
            .await
            .map(|_| ());
    }
    let order = orders(pool)
        .get(order.id)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| order.clone());
    apply_edit_and_send(bot, chat_id, user_id, &order, config, pool).await
}

async fn apply_edit_and_send(
    bot: &Bot,
    chat_id: ChatId,
    _user_id: UserId,
    order: &Order,
    config: &TelegramUiConfig,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    match apply_repo_edit(order, config, false).await {
        Ok(_) => {
            let contract = order
                .pending_contract_address
                .as_deref()
                .unwrap_or(DEFAULT_CONTRACT_ADDRESS);
            let x = order.pending_x_url.as_deref().unwrap_or(DEFAULT_X_URL);
            let telegram = order
                .pending_telegram_url
                .as_deref()
                .unwrap_or(DEFAULT_TELEGRAM_URL);
            if let Err(error) = orders(pool)
                .mark_edit_success_and_update_fields(order.id, contract, x, telegram)
                .await
            {
                warn!(?error, order_id = %order.id, "failed to record successful paid edit");
                return bot
                    .send_message(
                        chat_id,
                        "<b>Edit pushed, DB update failed.</b>\nContact admin.",
                    )
                    .parse_mode(ParseMode::Html)
                    .reply_markup(Keyboards::main_menu())
                    .await
                    .map(|_| ());
            }
            bot.send_message(
                chat_id,
                "✅ <b>Edit pushed.</b>\nCloudflare Pages will update from GitHub.",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(Keyboards::main_menu())
            .await
            .map(|_| ())
        }
        Err(error) => {
            warn!(?error, order_id = %order.id, "failed to apply paid repo edit");
            let _ = orders(pool).mark_edit_manual_review(order.id).await;
            bot.send_message(chat_id, "<b>Edit needs manual review.</b>\nContact admin.")
                .parse_mode(ParseMode::Html)
                .reply_markup(Keyboards::main_menu())
                .await
                .map(|_| ())
        }
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
    pool: &SqlitePool,
    _config: &TelegramUiConfig,
) -> ResponseResult<()> {
    match orders(pool).get_unpaid(user_id.0 as i64).await {
        Ok(Some(order)) => {
            Screens::new(bot)
                .continue_order_screen(chat_id, &order)
                .await
        }
        Ok(None) => bot
            .send_message(chat_id, "<b>No active order.</b>\nUse /create.")
            .parse_mode(ParseMode::Html)
            .reply_markup(Keyboards::main_menu())
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
                .reply_markup(Keyboards::main_menu())
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
    if let Ok(Some(edit_order)) = orders(pool).get_active_edit(user_id.0 as i64).await
        && edit_order.edit_status == "need_pay"
    {
        if address.trim().is_empty() {
            return Screens::new(bot)
                .edit_screen(
                    target,
                    "<b>Payment address missing.</b>\nContact admin.",
                    Keyboards::main_menu(),
                )
                .await;
        }
        let payment_window = match orders(pool)
            .attach_edit_payment_chain(edit_order.id, chain)
            .await
        {
            Ok(window) => window,
            Err(e) => {
                warn!(?e, order_id = %edit_order.id, "failed to attach edit payment chain");
                return Screens::new(bot)
                    .edit_screen(
                        target,
                        "<b>Payment start failed.</b>\nPlease try again.",
                        Keyboards::main_menu(),
                    )
                    .await;
            }
        };
        return Screens::new(bot)
            .edit_screen(
                target,
                Screens::edit_payment_instruction(
                    edit_order.id,
                    rust_decimal::Decimal::from(db::EDIT_PRICE_USDT),
                    chain,
                    address,
                    &payment_window,
                ),
                Keyboards::cancel(),
            )
            .await;
    }

    let order = match orders(pool).get_unpaid(user_id.0 as i64).await {
        Ok(Some(order)) => order,
        Ok(None) => {
            return Screens::new(bot)
                .edit_screen(
                    target,
                    "<b>No active order.</b>\nUse /create.",
                    Keyboards::main_menu(),
                )
                .await;
        }
        Err(e) => {
            warn!(
                ?e,
                user_id = user_id.0 as i64,
                "failed to load order for payment chain"
            );
            return Screens::new(bot)
                .edit_screen(target, "<b>Error.</b>\nTry again.", Keyboards::main_menu())
                .await;
        }
    };

    if order.status != OrderStatus::NeedPay {
        return Screens::new(bot)
            .edit_screen(
                target,
                "<b>Payment not ready.</b>\nComplete all steps first.",
                Keyboards::cancel(),
            )
            .await;
    }

    if address.trim().is_empty() {
        return Screens::new(bot)
            .edit_screen(
                target,
                "<b>Payment address missing.</b>\nContact admin.",
                Keyboards::main_menu(),
            )
            .await;
    }

    let order_id = order.id;
    let total = order.total_usdt;

    let payment_window = match orders(pool).attach_payment_chain(order_id, chain).await {
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
            return Screens::new(bot)
                .edit_screen(
                    target,
                    "<b>Payment start failed.</b>\nPlease try again.",
                    Keyboards::main_menu(),
                )
                .await;
        }
    };

    Screens::new(bot)
        .edit_screen(
            target,
            Screens::payment_instruction(order_id, total, chain, address, &payment_window),
            Keyboards::cancel(),
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
    let order = match orders(pool).get_unpaid(user_id.0 as i64).await {
        Ok(Some(order)) => order,
        Ok(None) => {
            return Screens::new(bot)
                .edit_screen(
                    target,
                    "<b>No active order.</b>\nUse /create.",
                    Keyboards::main_menu(),
                )
                .await;
        }
        Err(e) => {
            warn!(
                ?e,
                user_id = user_id.0 as i64,
                "failed to load order for alt domain"
            );
            return Screens::new(bot)
                .edit_screen(target, "<b>Error.</b>\nTry again.", Keyboards::main_menu())
                .await;
        }
    };

    let alternatives =
        parse_alternative_domains(order.alternative_domains_json.as_deref()).unwrap_or_default();
    let Some(quote) = alternatives.iter().find(|q| q.domain == domain).cloned() else {
        return Screens::new(bot)
            .edit_screen(
                target,
                "<b>Domain expired.</b>\nSend a new domain.",
                Keyboards::main_menu(),
            )
            .await;
    };

    let order_id = order.id;
    let total = order.site_price_usdt + quote.price_usd;

    if let Err(e) = orders(pool)
        .set_domain_pricing(order_id, quote.price_usd, total)
        .await
    {
        warn!(?e, %order_id, "failed to set domain pricing for alt domain");
        return Screens::new(bot)
            .edit_screen(
                target,
                "<b>Quote save failed.</b>\nPlease try again.",
                Keyboards::main_menu(),
            )
            .await;
    }
    if let Err(e) = orders(pool)
        .update_field_and_status(order_id, "requested_domain", domain, OrderStatus::NeedPay)
        .await
    {
        warn!(?e, %order_id, "failed to update domain/status for alt domain");
    }
    // Clear stored alternatives
    let _ = orders(pool)
        .clear_field(order_id, "alternative_domains_json")
        .await;

    Screens::new(bot).edit_screen(
        target,
        format!(
            "✅ <b>Domain selected</b>\n{domain}\nDomain: <b>{}</b> USDT\nWebsite: <b>{}</b> USDT\nTotal: <b>{}</b> USDT\n\nChoose payment chain.",
            quote.price_usd, order.site_price_usdt, total
        ),
        Keyboards::payment_chain(),
    )
    .await
}
