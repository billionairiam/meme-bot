use rust_decimal::Decimal;
use sqlx::SqlitePool;
use teloxide::prelude::*;
use teloxide::types::{ChatId, InlineKeyboardMarkup, ParseMode, UserId};
use tracing::warn;

use crate::db;
use crate::models::{Order, OrderStatus, PaymentChain};

use super::helpers::{html_escape, is_message_not_modified};
use super::keyboards::Keyboards;
use super::types::{TEMPLATE_PRICE_USDT, TEMPLATE_SITE_URL, TelegramUiConfig, UiMessage};

pub(crate) struct Screens<'a> {
    bot: &'a Bot,
}

impl<'a> Screens<'a> {
    pub(crate) fn new(bot: &'a Bot) -> Self {
        Self { bot }
    }

    pub(crate) async fn send_screen(
        &self,
        chat_id: ChatId,
        text: impl Into<String>,
        reply_markup: InlineKeyboardMarkup,
    ) -> ResponseResult<()> {
        self.bot
            .send_message(chat_id, text)
            .parse_mode(ParseMode::Html)
            .reply_markup(reply_markup)
            .await
            .map(|_| ())
    }

    pub(crate) async fn edit_screen(
        &self,
        target: UiMessage,
        text: impl Into<String>,
        reply_markup: InlineKeyboardMarkup,
    ) -> ResponseResult<()> {
        match self
            .bot
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

    pub(crate) async fn clear_screen_keyboard(&self, target: UiMessage) -> ResponseResult<()> {
        match self
            .bot
            .edit_message_reply_markup(target.chat_id, target.message_id)
            .await
        {
            Ok(_) => Ok(()),
            Err(error) if is_message_not_modified(&error) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

fn main_menu_text(_config: &TelegramUiConfig) -> String {
    format!(
        "🚀 <b>Auto Meme Web Bot</b>\nCreate meme websites.\n\nTemplates: {}",
        html_escape(TEMPLATE_SITE_URL)
    )
}

fn create_menu_text() -> &'static str {
    "🧩 <b>Create Website</b>\nChoose a mode."
}

fn template_mode_text(admin_contact: &str) -> String {
    format!(
        "📦 <b>Template Mode</b>\n\nPrice: <b>{TEMPLATE_PRICE_USDT} USDT</b>\nTemplates: {TEMPLATE_SITE_URL}\nAdmin: <code>{}</code>\n\nChoose a template on the website, then contact the admin to create your website.",
        html_escape(admin_contact)
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
            use std::fmt::Write;
            let mut text = String::from("📊 <b>Order Status</b>");
            for order in &orders {
                write!(
                    text,
                    "\n• {} · {} USDT · {} · {}",
                    order.status.display_label(),
                    order.total_usdt,
                    order.payment_chain.map(|c| c.as_str()).unwrap_or("—"),
                    order.requested_domain.as_deref().unwrap_or("—"),
                )
                .unwrap();
            }
            text
        }
        Err(_) => "📊 <b>Order Status</b>\nCheck failed. Try later.".to_string(),
    }
}

async fn history_text(user_id: UserId, pool: &SqlitePool) -> String {
    let orders = db::OrderStore::new(pool.clone());
    match orders.get_completed(user_id.0 as i64, 10).await {
        Ok(items) if items.is_empty() => "🧾 <b>History</b>\nNo completed orders.".to_string(),
        Ok(items) => {
            use std::fmt::Write;
            let mut text = String::from("🧾 <b>Completed Orders</b>");
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
                write!(
                    text,
                    "\n{}. {} · {} USDT · {} · {}",
                    index + 1,
                    html_escape(domain),
                    item.total_usdt,
                    chain,
                    paid_at
                )
                .unwrap();
            }
            text
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

fn payment_chain_text() -> &'static str {
    "💳 <b>Payment</b>\nChoose USDT chain."
}

impl<'a> Screens<'a> {
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
        &self,
        chat_id: ChatId,
        config: &TelegramUiConfig,
    ) -> ResponseResult<()> {
        self.send_screen(chat_id, main_menu_text(config), Keyboards::main_menu())
            .await
    }

    pub(crate) async fn edit_main_menu(
        &self,
        target: UiMessage,
        config: &TelegramUiConfig,
    ) -> ResponseResult<()> {
        self.edit_screen(target, main_menu_text(config), Keyboards::main_menu())
            .await
    }

    pub(crate) async fn show_create_menu(
        &self,
        chat_id: ChatId,
        ai_creation_price_usdt: Decimal,
    ) -> ResponseResult<()> {
        self.send_screen(
            chat_id,
            create_menu_text(),
            Keyboards::create_mode(ai_creation_price_usdt),
        )
        .await
    }

    pub(crate) async fn edit_create_menu(
        &self,
        target: UiMessage,
        ai_creation_price_usdt: Decimal,
    ) -> ResponseResult<()> {
        self.edit_screen(
            target,
            create_menu_text(),
            Keyboards::create_mode(ai_creation_price_usdt),
        )
        .await
    }

    pub(crate) async fn edit_template_mode(
        &self,
        target: UiMessage,
        config: &TelegramUiConfig,
    ) -> ResponseResult<()> {
        self.edit_screen(
            target,
            template_mode_text(&config.admin_contact),
            Keyboards::template_contact(),
        )
        .await
    }

    pub(crate) async fn show_template_mode(
        &self,
        chat_id: ChatId,
        config: &TelegramUiConfig,
    ) -> ResponseResult<()> {
        self.send_screen(
            chat_id,
            template_mode_text(&config.admin_contact),
            Keyboards::template_contact(),
        )
        .await
    }

    pub(crate) async fn edit_choose_chain(&self, target: UiMessage) -> ResponseResult<()> {
        self.edit_screen(target, choose_chain_text(), Keyboards::token_chain())
            .await
    }

    pub(crate) async fn show_edit_list(
        &self,
        chat_id: ChatId,
        user_id: UserId,
        pool: &SqlitePool,
    ) -> ResponseResult<()> {
        let orders = db::OrderStore::new(pool.clone());
        match orders.get_completed(user_id.0 as i64, 10).await {
            Ok(items) if items.is_empty() => {
                self.send_screen(
                    chat_id,
                    "✏️ <b>Edit Website</b>\nNo live websites yet.",
                    Keyboards::main_menu(),
                )
                .await
            }
            Ok(items) => {
                self.send_screen(
                    chat_id,
                    "✏️ <b>Edit Website</b>\nChoose a live website.",
                    Keyboards::edit_order_list(&items),
                )
                .await
            }
            Err(error) => {
                warn!(
                    ?error,
                    user_id = user_id.0 as i64,
                    "failed to load edit list"
                );
                self.send_screen(
                    chat_id,
                    "✏️ <b>Edit Website</b>\nCheck failed. Try later.",
                    Keyboards::main_menu(),
                )
                .await
            }
        }
    }

    pub(crate) async fn edit_edit_list(
        &self,
        target: UiMessage,
        user_id: UserId,
        pool: &SqlitePool,
    ) -> ResponseResult<()> {
        let orders = db::OrderStore::new(pool.clone());
        match orders.get_completed(user_id.0 as i64, 10).await {
            Ok(items) if items.is_empty() => {
                self.edit_screen(
                    target,
                    "✏️ <b>Edit Website</b>\nNo live websites yet.",
                    Keyboards::main_menu(),
                )
                .await
            }
            Ok(items) => {
                self.edit_screen(
                    target,
                    "✏️ <b>Edit Website</b>\nChoose a live website.",
                    Keyboards::edit_order_list(&items),
                )
                .await
            }
            Err(error) => {
                warn!(
                    ?error,
                    user_id = user_id.0 as i64,
                    "failed to load edit list"
                );
                self.edit_screen(
                    target,
                    "✏️ <b>Edit Website</b>\nCheck failed. Try later.",
                    Keyboards::main_menu(),
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
        &self,
        chat_id: ChatId,
        user_id: UserId,
        pool: &SqlitePool,
    ) -> ResponseResult<()> {
        self.send_screen(
            chat_id,
            status_text(user_id, pool).await,
            Keyboards::main_menu(),
        )
        .await
    }

    pub(crate) async fn show_history(
        &self,
        chat_id: ChatId,
        user_id: UserId,
        pool: &SqlitePool,
    ) -> ResponseResult<()> {
        self.send_screen(
            chat_id,
            history_text(user_id, pool).await,
            Keyboards::main_menu(),
        )
        .await
    }

    pub(crate) async fn edit_status(
        &self,
        target: UiMessage,
        user_id: UserId,
        pool: &SqlitePool,
    ) -> ResponseResult<()> {
        self.edit_screen(
            target,
            status_text(user_id, pool).await,
            Keyboards::main_menu(),
        )
        .await
    }

    pub(crate) async fn edit_history(
        &self,
        target: UiMessage,
        user_id: UserId,
        pool: &SqlitePool,
    ) -> ResponseResult<()> {
        self.edit_screen(
            target,
            history_text(user_id, pool).await,
            Keyboards::main_menu(),
        )
        .await
    }

    pub(crate) async fn edit_payment_chain_menu(&self, target: UiMessage) -> ResponseResult<()> {
        self.edit_screen(target, payment_chain_text(), Keyboards::payment_chain())
            .await
    }

    pub(crate) async fn edit_contact_admin(
        &self,
        target: UiMessage,
        config: &TelegramUiConfig,
    ) -> ResponseResult<()> {
        self.edit_screen(
            target,
            format!(
                "👤 <b>Contact admin</b>\n{}",
                html_escape(&config.admin_contact)
            ),
            Keyboards::main_menu(),
        )
        .await
    }

    /// Show the appropriate prompt for the order's current wizard step.
    pub(crate) async fn continue_order_screen(
        &self,
        chat_id: ChatId,
        order: &Order,
    ) -> ResponseResult<()> {
        match order.status {
            OrderStatus::NeedChain => self
                .bot
                .send_message(chat_id, choose_chain_text())
                .parse_mode(ParseMode::Html)
                .reply_markup(Keyboards::token_chain())
                .await
                .map(|_| ()),
            OrderStatus::NeedLogo => self
                .bot
                .send_message(chat_id, ask_logo_text())
                .parse_mode(ParseMode::Html)
                .reply_markup(Keyboards::cancel())
                .await
                .map(|_| ()),
            OrderStatus::NeedTicker => self
                .bot
                .send_message(
                    chat_id,
                    "🔤 <b>3/8 Ticker</b>\nSend token ticker.\nEg: PEPE",
                )
                .parse_mode(ParseMode::Html)
                .reply_markup(Keyboards::cancel())
                .await
                .map(|_| ()),
            OrderStatus::NeedName => self
                .bot
                .send_message(
                    chat_id,
                    "🏷 <b>4/8 Token Name</b>\nSend token name. Eg: Pepe Coin",
                )
                .parse_mode(ParseMode::Html)
                .reply_markup(Keyboards::cancel())
                .await
                .map(|_| ()),
            OrderStatus::NeedContract => self
                .bot
                .send_message(
                    chat_id,
                    "📝 <b>5/8 Contract Address</b>\nPaste contract address or tap Skip.",
                )
                .parse_mode(ParseMode::Html)
                .reply_markup(Keyboards::optional_step("contract"))
                .await
                .map(|_| ()),
            OrderStatus::NeedXLink => self
                .bot
                .send_message(chat_id, "✕ <b>6/8 X Link</b>\nPaste X link or tap Skip.")
                .parse_mode(ParseMode::Html)
                .reply_markup(Keyboards::optional_step("x"))
                .await
                .map(|_| ()),
            OrderStatus::NeedTme => self
                .bot
                .send_message(
                    chat_id,
                    "📱 <b>7/8 Telegram Link</b>\nPaste TG link or tap Skip.",
                )
                .parse_mode(ParseMode::Html)
                .reply_markup(Keyboards::optional_step("telegram"))
                .await
                .map(|_| ()),
            OrderStatus::NeedDomain => self
                .bot
                .send_message(
                    chat_id,
                    "🌐 <b>8/8 Domain</b>\nSend domain name.\nEg: mytoken.xyz",
                )
                .parse_mode(ParseMode::Html)
                .reply_markup(Keyboards::cancel())
                .await
                .map(|_| ()),
            OrderStatus::NeedPay => self
                .bot
                .send_message(chat_id, payment_chain_text())
                .parse_mode(ParseMode::Html)
                .reply_markup(Keyboards::payment_chain())
                .await
                .map(|_| ()),
            OrderStatus::PaidAndGenerating => self
                .bot
                .send_message(
                    chat_id,
                    "⚙️ <b>Generating...</b>\nYour website is being created.",
                )
                .parse_mode(ParseMode::Html)
                .reply_markup(Keyboards::status())
                .await
                .map(|_| ()),
            OrderStatus::Done => self
                .bot
                .send_message(
                    chat_id,
                    "✅ <b>Done!</b>\nYour website is live. Check /history.",
                )
                .parse_mode(ParseMode::Html)
                .reply_markup(Keyboards::main_menu())
                .await
                .map(|_| ()),
            _ => self
                .bot
                .send_message(
                    chat_id,
                    format!("📊 Order status: {}", order.status.display_label()),
                )
                .parse_mode(ParseMode::Html)
                .reply_markup(Keyboards::main_menu())
                .await
                .map(|_| ()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::template_mode_text;

    #[test]
    fn template_mode_directs_users_to_store_and_admin_at_200_usdt() {
        let text = template_mode_text("@configured_admin");

        assert!(text.contains("https://memewebs.store"));
        assert!(text.contains("200 USDT"));
        assert!(text.contains("@configured_admin"));
    }
}
