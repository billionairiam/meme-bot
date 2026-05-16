use rust_decimal::Decimal;
use sqlx::SqlitePool;
use teloxide::prelude::*;
use teloxide::types::{ChatId, InlineKeyboardMarkup, ParseMode, UserId};
use tracing::warn;

use crate::db;
use crate::models::{Order, OrderStatus, PaymentChain};

use super::helpers::{html_escape, is_message_not_modified};
use super::keyboards::*;
use super::types::{TelegramUiConfig, UiMessage};

pub(crate) async fn send_screen(
    bot: &Bot,
    chat_id: ChatId,
    text: impl Into<String>,
    reply_markup: InlineKeyboardMarkup,
) -> ResponseResult<()> {
    bot.send_message(chat_id, text)
        .parse_mode(ParseMode::Html)
        .reply_markup(reply_markup)
        .await
        .map(|_| ())
}

pub(crate) async fn edit_screen(
    bot: &Bot,
    target: UiMessage,
    text: impl Into<String>,
    reply_markup: InlineKeyboardMarkup,
) -> ResponseResult<()> {
    match bot
        .edit_message_text(target.chat_id, target.message_id, text)
        .parse_mode(ParseMode::Html)
        .reply_markup(reply_markup)
        .await
    {
        Ok(_) => Ok(()),
        Err(error) if is_message_not_modified(&error) => Ok(()),
        Err(error) => Err(error),
    }
}

pub(crate) async fn clear_screen_keyboard(bot: &Bot, target: UiMessage) -> ResponseResult<()> {
    match bot
        .edit_message_reply_markup(target.chat_id, target.message_id)
        .await
    {
        Ok(_) => Ok(()),
        Err(error) if is_message_not_modified(&error) => Ok(()),
        Err(error) => Err(error),
    }
}

fn main_menu_text(config: &TelegramUiConfig) -> String {
    format!(
        "🚀 <b>Auto Meme Web Bot</b>\nCreate meme websites.\n\nTemplates: {}",
        html_escape(&config.template_preview_base_url)
    )
}

fn create_menu_text() -> &'static str {
    "🧩 <b>Create Website</b>\nChoose a mode."
}

fn template_mode_text() -> &'static str {
    "📦 <b>Template Mode</b>\nChoose a template."
}

fn template_confirmation_text(
    template: &super::types::TemplateOption,
    price_usdt: Decimal,
) -> String {
    format!(
        "📦 <b>{}</b>\nPrice: <b>{}</b> USDT\n🔗 <a href=\"{}\">Preview template</a>",
        html_escape(&template.name),
        price_usdt,
        &template.preview_url,
    )
}

fn ask_logo_text() -> &'static str {
    "🖼 <b>2/8 Logo</b>\nUpload token logo."
}

fn choose_chain_text() -> &'static str {
    "⛓ <b>1/8 Chain</b>\nChoose the meme website chain."
}

async fn status_text(user_id: UserId, pool: &SqlitePool) -> String {
    let orders = db::OrderStore::new(pool.clone());
    match orders.get_active(user_id.0 as i64).await {
        Ok(orders) if orders.is_empty() => {
            "📊 <b>Order Status</b>\nNo active order. Use /create.".to_string()
        }
        Ok(orders) => {
            let mut lines = vec!["📊 <b>Order Status</b>".to_string()];
            for order in &orders {
                lines.push(format!(
                    "• {} · {} USDT · {} · {}",
                    order.status.display_label(),
                    order.total_usdt,
                    order.payment_chain.map(|c| c.as_str()).unwrap_or("—"),
                    order.requested_domain.as_deref().unwrap_or("—"),
                ));
            }
            lines.join("\n")
        }
        Err(_) => "📊 <b>Order Status</b>\nCheck failed. Try later.".to_string(),
    }
}

async fn history_text(user_id: UserId, pool: &SqlitePool) -> String {
    let orders = db::OrderStore::new(pool.clone());
    match orders.get_completed(user_id.0 as i64, 10).await {
        Ok(items) if items.is_empty() => "🧾 <b>History</b>\nNo completed orders.".to_string(),
        Ok(items) => {
            let mut lines = vec!["🧾 <b>Completed Orders</b>".to_string()];
            for (index, item) in items.iter().enumerate() {
                let domain = item
                    .custom_domain
                    .as_deref()
                    .or(item.requested_domain.as_deref())
                    .or(item.cloudflare_pages_url.as_deref())
                    .unwrap_or("—");
                let chain = item.payment_chain.as_deref().unwrap_or("—");
                let paid_at = item
                    .paid_at
                    .map(|value| value.format("%Y-%m-%d").to_string())
                    .unwrap_or_else(|| "—".to_string());
                lines.push(format!(
                    "{}. {} · {} USDT · {} · {}",
                    index + 1,
                    html_escape(domain),
                    item.total_usdt,
                    chain,
                    paid_at
                ));
            }
            lines.join("\n")
        }
        Err(error) => {
            warn!(
                ?error,
                user_id = user_id.0 as i64,
                "failed to load completed order history"
            );
            "🧾 <b>History</b>\nCheck failed. Try later.".to_string()
        }
    }
}

pub(crate) fn payment_chain_text() -> &'static str {
    "💳 <b>Payment</b>\nChoose USDT chain."
}

pub fn payment_instruction(
    order_id: uuid::Uuid,
    amount_usdt: Decimal,
    chain: PaymentChain,
    address: &str,
    payment_window: &db::PaymentWindow,
) -> String {
    format!(
        "💳 <b>Pay</b>\nOrder: <code>{order_id}</code>\nAmount: <b>{amount_usdt}</b> USDT\nChain: <b>{}</b>\nAddress:\n<code>{}</code>\n\nExpires: <code>{}</code> UTC\nSend TX hash after payment.",
        chain.as_str(),
        html_escape(address),
        payment_window.expires_at.format("%Y-%m-%d %H:%M:%S")
    )
}

pub fn edit_payment_instruction(
    order_id: uuid::Uuid,
    amount_usdt: Decimal,
    chain: PaymentChain,
    address: &str,
    payment_window: &db::PaymentWindow,
) -> String {
    format!(
        "💳 <b>Edit Fee</b>\nOrder: <code>{order_id}</code>\nAmount: <b>{amount_usdt}</b> USDT\nChain: <b>{}</b>\nAddress:\n<code>{}</code>\n\nExpires: <code>{}</code> UTC\nSend TX hash after payment.",
        chain.as_str(),
        html_escape(address),
        payment_window.expires_at.format("%Y-%m-%d %H:%M:%S")
    )
}

pub(crate) async fn show_main_menu(
    bot: &Bot,
    chat_id: ChatId,
    config: &TelegramUiConfig,
) -> ResponseResult<()> {
    send_screen(bot, chat_id, main_menu_text(config), main_menu_keyboard()).await
}

pub(crate) async fn edit_main_menu(
    bot: &Bot,
    target: UiMessage,
    config: &TelegramUiConfig,
) -> ResponseResult<()> {
    edit_screen(bot, target, main_menu_text(config), main_menu_keyboard()).await
}

pub(crate) async fn show_create_menu(
    bot: &Bot,
    chat_id: ChatId,
    template_prices_usdt: &[Decimal],
    ai_creation_price_usdt: Decimal,
) -> ResponseResult<()> {
    send_screen(
        bot,
        chat_id,
        create_menu_text(),
        create_mode_keyboard(template_prices_usdt, ai_creation_price_usdt),
    )
    .await
}

pub(crate) async fn edit_create_menu(
    bot: &Bot,
    target: UiMessage,
    template_prices_usdt: &[Decimal],
    ai_creation_price_usdt: Decimal,
) -> ResponseResult<()> {
    edit_screen(
        bot,
        target,
        create_menu_text(),
        create_mode_keyboard(template_prices_usdt, ai_creation_price_usdt),
    )
    .await
}

pub(crate) async fn edit_template_mode(
    bot: &Bot,
    target: UiMessage,
    config: &TelegramUiConfig,
) -> ResponseResult<()> {
    edit_template_mode_page(bot, target, config, 0).await
}

pub(crate) async fn edit_template_mode_page(
    bot: &Bot,
    target: UiMessage,
    config: &TelegramUiConfig,
    page: usize,
) -> ResponseResult<()> {
    edit_screen(
        bot,
        target,
        template_mode_text(),
        template_keyboard_page(&config.template_options, &config.template_prices_usdt, page),
    )
    .await
}

pub(crate) async fn show_template_confirmation(
    bot: &Bot,
    chat_id: ChatId,
    template_id: &str,
    config: &TelegramUiConfig,
) -> ResponseResult<()> {
    let Some(template) = super::helpers::find_template(&config.template_options, template_id)
    else {
        return send_screen(
            bot,
            chat_id,
            "<b>Template not found.</b>\nChoose again.",
            template_keyboard(&config.template_options, &config.template_prices_usdt),
        )
        .await;
    };
    let price = super::helpers::template_price(
        &config.template_options,
        template_id,
        &config.template_prices_usdt,
    );

    send_screen(
        bot,
        chat_id,
        template_confirmation_text(&template, price),
        template_confirmation_keyboard(template_id, &template.preview_url),
    )
    .await
}

pub(crate) async fn edit_template_confirmation(
    bot: &Bot,
    target: UiMessage,
    template_id: &str,
    config: &TelegramUiConfig,
) -> ResponseResult<()> {
    let Some(template) = super::helpers::find_template(&config.template_options, template_id)
    else {
        return edit_screen(
            bot,
            target,
            "<b>Template not found.</b>\nChoose again.",
            template_keyboard(&config.template_options, &config.template_prices_usdt),
        )
        .await;
    };
    let price = super::helpers::template_price(
        &config.template_options,
        template_id,
        &config.template_prices_usdt,
    );

    edit_screen(
        bot,
        target,
        template_confirmation_text(&template, price),
        template_confirmation_keyboard(template_id, &template.preview_url),
    )
    .await
}

pub(crate) async fn edit_choose_chain(bot: &Bot, target: UiMessage) -> ResponseResult<()> {
    edit_screen(bot, target, choose_chain_text(), token_chain_keyboard()).await
}

pub(crate) async fn show_edit_list(
    bot: &Bot,
    chat_id: ChatId,
    user_id: UserId,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    let orders = db::OrderStore::new(pool.clone());
    match orders.get_completed(user_id.0 as i64, 10).await {
        Ok(items) if items.is_empty() => {
            send_screen(
                bot,
                chat_id,
                "✏️ <b>Edit Website</b>\nNo live websites yet.",
                main_menu_keyboard(),
            )
            .await
        }
        Ok(items) => {
            send_screen(
                bot,
                chat_id,
                "✏️ <b>Edit Website</b>\nChoose a live website.",
                edit_order_list_keyboard(&items),
            )
            .await
        }
        Err(error) => {
            warn!(
                ?error,
                user_id = user_id.0 as i64,
                "failed to load edit list"
            );
            send_screen(
                bot,
                chat_id,
                "✏️ <b>Edit Website</b>\nCheck failed. Try later.",
                main_menu_keyboard(),
            )
            .await
        }
    }
}

pub(crate) async fn edit_edit_list(
    bot: &Bot,
    target: UiMessage,
    user_id: UserId,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    let orders = db::OrderStore::new(pool.clone());
    match orders.get_completed(user_id.0 as i64, 10).await {
        Ok(items) if items.is_empty() => {
            edit_screen(
                bot,
                target,
                "✏️ <b>Edit Website</b>\nNo live websites yet.",
                main_menu_keyboard(),
            )
            .await
        }
        Ok(items) => {
            edit_screen(
                bot,
                target,
                "✏️ <b>Edit Website</b>\nChoose a live website.",
                edit_order_list_keyboard(&items),
            )
            .await
        }
        Err(error) => {
            warn!(
                ?error,
                user_id = user_id.0 as i64,
                "failed to load edit list"
            );
            edit_screen(
                bot,
                target,
                "✏️ <b>Edit Website</b>\nCheck failed. Try later.",
                main_menu_keyboard(),
            )
            .await
        }
    }
}

pub(crate) fn edit_order_text(order: &Order, free_used: i64) -> String {
    let pending_contract = order
        .pending_contract_address
        .as_deref()
        .or(order.contract_address.as_deref())
        .unwrap_or("—");
    let pending_x = order
        .pending_x_url
        .as_deref()
        .or(order.x_url.as_deref())
        .unwrap_or("—");
    let pending_tg = order
        .pending_telegram_url
        .as_deref()
        .or(order.telegram_url.as_deref())
        .unwrap_or("—");
    let free_left = (db::FREE_EDIT_LIMIT - free_used).max(0);
    format!(
        "✏️ <b>Edit Website</b>\nFree edits left: <b>{free_left}</b>\n\nCA:\n<code>{}</code>\nX:\n<code>{}</code>\nTelegram:\n<code>{}</code>",
        html_escape(pending_contract),
        html_escape(pending_x),
        html_escape(pending_tg)
    )
}

pub(crate) async fn show_status(
    bot: &Bot,
    chat_id: ChatId,
    user_id: UserId,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    send_screen(
        bot,
        chat_id,
        status_text(user_id, pool).await,
        main_menu_keyboard(),
    )
    .await
}

pub(crate) async fn show_history(
    bot: &Bot,
    chat_id: ChatId,
    user_id: UserId,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    send_screen(
        bot,
        chat_id,
        history_text(user_id, pool).await,
        main_menu_keyboard(),
    )
    .await
}

pub(crate) async fn edit_status(
    bot: &Bot,
    target: UiMessage,
    user_id: UserId,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    edit_screen(
        bot,
        target,
        status_text(user_id, pool).await,
        main_menu_keyboard(),
    )
    .await
}

pub(crate) async fn edit_history(
    bot: &Bot,
    target: UiMessage,
    user_id: UserId,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    edit_screen(
        bot,
        target,
        history_text(user_id, pool).await,
        main_menu_keyboard(),
    )
    .await
}

pub(crate) async fn edit_payment_chain_menu(bot: &Bot, target: UiMessage) -> ResponseResult<()> {
    edit_screen(bot, target, payment_chain_text(), payment_chain_keyboard()).await
}

pub(crate) async fn edit_contact_admin(
    bot: &Bot,
    target: UiMessage,
    config: &TelegramUiConfig,
) -> ResponseResult<()> {
    edit_screen(
        bot,
        target,
        format!(
            "👤 <b>Contact admin</b>\n{}",
            html_escape(&config.admin_contact)
        ),
        main_menu_keyboard(),
    )
    .await
}

/// Show the appropriate prompt for the order's current wizard step.
pub(crate) async fn continue_order_screen(
    bot: &Bot,
    chat_id: ChatId,
    order: &Order,
) -> ResponseResult<()> {
    match order.status {
        OrderStatus::NeedChain => bot
            .send_message(chat_id, choose_chain_text())
            .parse_mode(ParseMode::Html)
            .reply_markup(token_chain_keyboard())
            .await
            .map(|_| ()),
        OrderStatus::NeedLogo => bot
            .send_message(chat_id, ask_logo_text())
            .parse_mode(ParseMode::Html)
            .reply_markup(cancel_keyboard())
            .await
            .map(|_| ()),
        OrderStatus::NeedTicker => bot
            .send_message(
                chat_id,
                "🔤 <b>3/8 Ticker</b>\nSend token ticker.\nEg: PEPE",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(cancel_keyboard())
            .await
            .map(|_| ()),
        OrderStatus::NeedName => bot
            .send_message(
                chat_id,
                "🏷 <b>4/8 Token Name</b>\nSend token name. Eg: Pepe Coin",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(cancel_keyboard())
            .await
            .map(|_| ()),
        OrderStatus::NeedContract => bot
            .send_message(
                chat_id,
                "📝 <b>5/8 Contract Address</b>\nPaste contract address or tap Skip.",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(optional_step_keyboard("contract"))
            .await
            .map(|_| ()),
        OrderStatus::NeedXLink => bot
            .send_message(chat_id, "✕ <b>6/8 X Link</b>\nPaste X link or tap Skip.")
            .parse_mode(ParseMode::Html)
            .reply_markup(optional_step_keyboard("x"))
            .await
            .map(|_| ()),
        OrderStatus::NeedTme => bot
            .send_message(
                chat_id,
                "📱 <b>7/8 Telegram Link</b>\nPaste TG link or tap Skip.",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(optional_step_keyboard("telegram"))
            .await
            .map(|_| ()),
        OrderStatus::NeedDomain => bot
            .send_message(
                chat_id,
                "🌐 <b>8/8 Domain</b>\nSend domain name.\nEg: mytoken.xyz",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(cancel_keyboard())
            .await
            .map(|_| ()),
        OrderStatus::NeedPay => bot
            .send_message(chat_id, payment_chain_text())
            .parse_mode(ParseMode::Html)
            .reply_markup(payment_chain_keyboard())
            .await
            .map(|_| ()),
        OrderStatus::PaidAndGenerating => bot
            .send_message(
                chat_id,
                "⚙️ <b>Generating...</b>\nYour website is being created.",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(status_keyboard())
            .await
            .map(|_| ()),
        OrderStatus::Done => bot
            .send_message(
                chat_id,
                "✅ <b>Done!</b>\nYour website is live. Check /history.",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(main_menu_keyboard())
            .await
            .map(|_| ()),
        _ => bot
            .send_message(
                chat_id,
                format!("📊 Order status: {}", order.status.display_label()),
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(main_menu_keyboard())
            .await
            .map(|_| ()),
    }
}
