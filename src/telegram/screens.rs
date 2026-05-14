use rust_decimal::Decimal;
use sqlx::SqlitePool;
use teloxide::prelude::*;
use teloxide::types::{ChatId, InlineKeyboardMarkup, ParseMode, UserId};
use tracing::warn;

use crate::db;
use crate::models::PaymentChain;

use super::helpers::{build_mode_label, html_escape, is_message_not_modified};
use super::keyboards::*;
use super::types::{DraftStep, OrderDraft, Sessions, TelegramUiConfig, UiMessage};

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
        html_escape(&config.template_catalog_url)
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
    "🖼 <b>1/7 Logo</b>\nUpload token logo."
}

async fn status_text(user_id: UserId, sessions: &Sessions, pool: &SqlitePool) -> String {
    if let Some(draft) = super::helpers::get_session(user_id, sessions) {
        return format!(
            "📊 <b>Current Order</b>\nStatus: <code>{:?}</code>\nMode: <b>{}</b>\nPrice: <b>{}</b> USDT",
            draft.step,
            build_mode_label(&draft.build_mode),
            draft.site_price_usdt
        );
    }
    match db::get_user_order(pool, user_id.0 as i64).await {
        Ok(Some(order)) => {
            let site_display = match order.site_status.as_str() {
                "collecting_assets" => "📝 Collecting info",
                "queued" => "⏳ Queued for generation",
                "generating" => "⚙️ Generating website",
                "deploying" => "🚀 Deploying",
                "live" => "✅ Live",
                "needs_manual_review" => "🔧 Under admin review",
                "failed" => "❌ Failed",
                other => other,
            };
            format!(
                "📊 <b>Order Status</b>\nPayment: {}\nSite: {}\nAmount: {} USDT\nChain: {}\nDomain: {}",
                order.payment_status,
                site_display,
                order.amount_usdt,
                order.payment_chain.as_deref().unwrap_or("—"),
                order.requested_domain.as_deref().unwrap_or("—"),
            )
        }
        Ok(None) => "📊 <b>Order Status</b>\nNo active order. Use /create.".to_string(),
        Err(_) => "📊 <b>Order Status</b>\nCheck failed. Try later.".to_string(),
    }
}

async fn history_text(user_id: UserId, pool: &SqlitePool) -> String {
    match db::get_user_paid_order_history(pool, user_id.0 as i64, 10).await {
        Ok(items) if items.is_empty() => "🧾 <b>History</b>\nNo paid orders.".to_string(),
        Ok(items) => {
            let mut lines = vec!["🧾 <b>Paid Orders</b>".to_string()];
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
                    "{}. {} · {} USDT · {} · {} · {}",
                    index + 1,
                    html_escape(domain),
                    item.amount_usdt,
                    chain,
                    item.site_status,
                    paid_at
                ));
            }
            lines.join("\n")
        }
        Err(error) => {
            warn!(
                ?error,
                user_id = user_id.0 as i64,
                "failed to load paid order history"
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
    edit_screen(
        bot,
        target,
        template_mode_text(),
        template_keyboard(&config.template_options, &config.template_prices_usdt),
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

pub(crate) async fn edit_ask_logo(bot: &Bot, target: UiMessage) -> ResponseResult<()> {
    edit_screen(bot, target, ask_logo_text(), cancel_keyboard()).await
}

pub(crate) async fn show_status(
    bot: &Bot,
    chat_id: ChatId,
    user_id: UserId,
    sessions: &Sessions,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    send_screen(
        bot,
        chat_id,
        status_text(user_id, sessions, pool).await,
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
    sessions: &Sessions,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    edit_screen(
        bot,
        target,
        status_text(user_id, sessions, pool).await,
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

pub(crate) async fn continue_step_screen(
    bot: &Bot,
    chat_id: ChatId,
    draft: &OrderDraft,
) -> ResponseResult<()> {
    match draft.step {
        DraftStep::WaitingLogo => bot
            .send_message(chat_id, ask_logo_text())
            .parse_mode(ParseMode::Html)
            .reply_markup(cancel_keyboard())
            .await
            .map(|_| ()),
        DraftStep::WaitingTicker => bot
            .send_message(chat_id, "🔤 <b>2/7 Ticker</b>\nSend token ticker. Eg: PEPE")
            .parse_mode(ParseMode::Html)
            .reply_markup(cancel_keyboard())
            .await
            .map(|_| ()),
        DraftStep::WaitingTokenName => bot
            .send_message(
                chat_id,
                "🏷 <b>3/7 Token Name</b>\nSend token name. Eg: Pepe Coin",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(cancel_keyboard())
            .await
            .map(|_| ()),
        DraftStep::WaitingContractAddress => bot
            .send_message(
                chat_id,
                "📝 <b>4/7 Contract Address</b>\nPaste contract address.",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(cancel_keyboard())
            .await
            .map(|_| ()),
        DraftStep::WaitingXLink => bot
            .send_message(
                chat_id,
                "🐦 <b>5/7 X (Twitter) Link</b>\nPaste X link or send '-' to skip.",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(cancel_keyboard())
            .await
            .map(|_| ()),
        DraftStep::WaitingTelegramLink => bot
            .send_message(
                chat_id,
                "📱 <b>6/7 Telegram Link</b>\nPaste TG link or send '-' to skip.",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(cancel_keyboard())
            .await
            .map(|_| ()),
        DraftStep::WaitingWebsiteUrl => bot
            .send_message(
                chat_id,
                "🌐 <b>7/7 Domain</b>\nSend domain name.\nEg: mytoken.xyz",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(cancel_keyboard())
            .await
            .map(|_| ()),
        DraftStep::ReadyToPay => bot
            .send_message(chat_id, payment_chain_text())
            .parse_mode(ParseMode::Html)
            .reply_markup(payment_chain_keyboard())
            .await
            .map(|_| ()),
        DraftStep::WaitingTx => bot
            .send_message(chat_id, "💳 <b>Payment</b>\nPaste TX hash.")
            .parse_mode(ParseMode::Html)
            .reply_markup(cancel_keyboard())
            .await
            .map(|_| ()),
        DraftStep::PaymentConfirmed => bot
            .send_message(chat_id, "<b>Payment confirmed.</b>\nWebsite is queued.")
            .parse_mode(ParseMode::Html)
            .reply_markup(status_keyboard())
            .await
            .map(|_| ()),
    }
}
