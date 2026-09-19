use rust_decimal::Decimal;
use teloxide::types::InlineKeyboardButton;
use teloxide::types::InlineKeyboardMarkup;

use super::types::{TEMPLATE_PRICE_USDT, TEMPLATE_SITE_URL};

pub(crate) fn main_menu() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback(
            "🚀 Create Website",
            "create",
        )],
        vec![InlineKeyboardButton::callback("✏️ Edit Website", "edit")],
        vec![InlineKeyboardButton::callback("📊 Order Status", "status")],
        vec![InlineKeyboardButton::callback("🧾 Paid History", "history")],
        vec![InlineKeyboardButton::callback(
            "👤 Contact Admin",
            "contact_admin",
        )],
    ])
}

pub(crate) fn token_chain() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback("ETH", "token_chain:eth"),
            InlineKeyboardButton::callback("BSC", "token_chain:bsc"),
        ],
        vec![
            InlineKeyboardButton::callback("SOL", "token_chain:sol"),
            InlineKeyboardButton::callback("TON", "token_chain:ton"),
        ],
        vec![InlineKeyboardButton::callback("🛑 Cancel Order", "cancel")],
    ])
}

pub(crate) fn create_mode(ai_creation_price_usdt: Decimal) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback(
            format!("📦 Template Mode | {TEMPLATE_PRICE_USDT} USDT"),
            "template_mode",
        )],
        vec![InlineKeyboardButton::callback(
            format!("🤖 Free Creation | {} USDT", ai_creation_price_usdt),
            "ai_mode",
        )],
        vec![InlineKeyboardButton::callback("⬅ Back", "main")],
    ])
}

pub(crate) fn template_contact() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::url(
            "🌐 View Templates",
            url::Url::parse(TEMPLATE_SITE_URL).expect("valid template website url"),
        )],
        vec![InlineKeyboardButton::callback(
            "👤 Contact Admin",
            "contact_admin",
        )],
        vec![InlineKeyboardButton::callback("⬅ Back", "create")],
    ])
}

pub(crate) fn payment_chain() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback("💳 USDT ERC20", "pay_erc20")],
        vec![InlineKeyboardButton::callback("💳 USDT TRON", "pay_tron")],
        vec![InlineKeyboardButton::callback("💳 USDT SOL", "pay_sol")],
        vec![InlineKeyboardButton::callback("⬅ Back", "pay_menu")],
    ])
}

pub(crate) fn status() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback("📊 Check Status", "status")],
        vec![InlineKeyboardButton::callback(
            "👤 Contact Admin",
            "contact_admin",
        )],
    ])
}

pub(crate) fn cancel() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
        "🛑 Cancel Order",
        "cancel",
    )]])
}

pub(crate) fn optional_step(skip_target: &str) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback(
            "⏭ Skip",
            format!("skip_optional:{skip_target}"),
        )],
        vec![InlineKeyboardButton::callback("🛑 Cancel Order", "cancel")],
    ])
}

pub(crate) fn alternative_domains(
    alternatives: &[crate::domain::DomainQuote],
) -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();
    for quote in alternatives.iter().take(12) {
        rows.push(vec![InlineKeyboardButton::callback(
            format!("{} — {} USDT", quote.domain, quote.price_usd.round_dp(2)),
            format!("alt_domain:{}", quote.domain),
        )]);
    }
    rows.push(vec![InlineKeyboardButton::callback(
        "✏️ Enter another domain",
        "alt_domain_custom",
    )]);
    InlineKeyboardMarkup::new(rows)
}

pub(crate) fn edit_order_list(items: &[crate::db::OrderWithWebsite]) -> InlineKeyboardMarkup {
    let mut rows = Vec::new();
    for item in items.iter().take(10) {
        let label = item
            .custom_domain
            .as_deref()
            .or(item.requested_domain.as_deref())
            .or(item.token_ticker.as_deref())
            .unwrap_or("Website");
        rows.push(vec![InlineKeyboardButton::callback(
            format!("✏️ {label}"),
            format!("edit_select:{}", item.order_id),
        )]);
    }
    rows.push(vec![InlineKeyboardButton::callback("⬅ Back", "main")]);
    InlineKeyboardMarkup::new(rows)
}

pub(crate) fn edit_menu(order_id: uuid::Uuid) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback(
            "CA",
            format!("edit_field:{order_id}:contract"),
        )],
        vec![InlineKeyboardButton::callback(
            "X",
            format!("edit_field:{order_id}:x"),
        )],
        vec![InlineKeyboardButton::callback(
            "Telegram",
            format!("edit_field:{order_id}:telegram"),
        )],
        vec![InlineKeyboardButton::callback(
            "✅ Apply",
            format!("edit_apply:{order_id}"),
        )],
        vec![InlineKeyboardButton::callback(
            "🛑 Cancel Edit",
            format!("edit_cancel:{order_id}"),
        )],
    ])
}
#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;
    use teloxide::types::InlineKeyboardButtonKind;

    use super::{create_mode, template_contact};

    #[test]
    fn create_menu_shows_fixed_200_usdt_template_price() {
        let keyboard = create_mode(Decimal::new(50, 0));

        assert_eq!(
            keyboard.inline_keyboard[0][0].text,
            "📦 Template Mode | 200 USDT"
        );
    }

    #[test]
    fn template_mode_links_to_store_and_admin_instead_of_template_checkout() {
        let keyboard = template_contact();

        assert_eq!(keyboard.inline_keyboard.len(), 3);
        assert_eq!(keyboard.inline_keyboard[0][0].text, "🌐 View Templates");
        assert!(matches!(
            &keyboard.inline_keyboard[0][0].kind,
            InlineKeyboardButtonKind::Url(url) if url.as_str() == "https://memewebs.store/"
        ));
        assert_eq!(keyboard.inline_keyboard[1][0].text, "👤 Contact Admin");
        assert_eq!(
            keyboard.inline_keyboard[1][0].kind,
            InlineKeyboardButtonKind::CallbackData("contact_admin".to_string())
        );
        assert_eq!(
            keyboard.inline_keyboard[2][0].kind,
            InlineKeyboardButtonKind::CallbackData("create".to_string())
        );
    }

    #[test]
    fn template_contact_cannot_start_template_checkout() {
        let keyboard = template_contact();
        let callbacks: Vec<&str> = keyboard
            .inline_keyboard
            .iter()
            .flatten()
            .filter_map(|button| match &button.kind {
                InlineKeyboardButtonKind::CallbackData(data) => Some(data.as_str()),
                _ => None,
            })
            .collect();

        assert_eq!(callbacks, ["contact_admin", "create"]);
        assert!(
            !callbacks
                .iter()
                .any(|data| data.starts_with("confirm_tpl:"))
        );
        assert!(matches!(
            &keyboard.inline_keyboard[0][0].kind,
            InlineKeyboardButtonKind::Url(url) if url.as_str() == "https://memewebs.store/"
        ));
    }
}
