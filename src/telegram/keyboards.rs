use rust_decimal::Decimal;
use teloxide::types::InlineKeyboardButton;
use teloxide::types::InlineKeyboardMarkup;

use super::helpers::{template_number, template_price};
use super::types::TemplateOption;

pub(crate) fn main_menu_keyboard() -> InlineKeyboardMarkup {
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

pub(crate) fn token_chain_keyboard() -> InlineKeyboardMarkup {
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

pub(crate) fn create_mode_keyboard(
    template_prices_usdt: &[Decimal],
    ai_creation_price_usdt: Decimal,
) -> InlineKeyboardMarkup {
    let (min, max) = template_prices_usdt
        .iter()
        .fold((Decimal::MAX, Decimal::ZERO), |(min, max), &p| {
            (min.min(p), max.max(p))
        });
    let template_label = if min == max {
        format!("📦 Template Mode | {} USDT", min)
    } else {
        format!("📦 Template Mode | {}-{} USDT", min, max)
    };
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback(
            template_label,
            "template_mode",
        )],
        vec![InlineKeyboardButton::callback(
            format!("🤖 Free Creation | {} USDT", ai_creation_price_usdt),
            "ai_mode",
        )],
        vec![InlineKeyboardButton::callback("⬅ Back", "main")],
    ])
}

pub(crate) fn template_keyboard(
    options: &[TemplateOption],
    prices: &[Decimal],
) -> InlineKeyboardMarkup {
    let mut rows = Vec::new();
    for chunk in options.chunks(2) {
        let mut row = Vec::new();
        for template in chunk {
            let number = template_number(options, &template.id);
            let price = template_price(options, &template.id, prices);
            row.push(InlineKeyboardButton::callback(
                format!("{number:02}. {} · {}U", template.name, price),
                format!("select_tpl:{}", template.id),
            ));
        }
        rows.push(row);
    }
    rows.push(vec![InlineKeyboardButton::callback("⬅ Back", "create")]);
    InlineKeyboardMarkup::new(rows)
}

pub(crate) fn template_confirmation_keyboard(
    template_id: &str,
    preview_url: &str,
) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback(
            "✅ Confirm Template",
            format!("confirm_tpl:{template_id}"),
        )],
        vec![InlineKeyboardButton::url(
            "🔗 Preview Template",
            url::Url::parse(preview_url).expect("valid preview url"),
        )],
        vec![InlineKeyboardButton::callback(
            "📦 Choose Another",
            "template_mode",
        )],
        vec![InlineKeyboardButton::callback("⬅ Back", "create")],
    ])
}

pub(crate) fn payment_chain_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback("💳 USDT ERC20", "pay_erc20")],
        vec![InlineKeyboardButton::callback("💳 USDT TRON", "pay_tron")],
        vec![InlineKeyboardButton::callback("💳 USDT SOL", "pay_sol")],
        vec![InlineKeyboardButton::callback("⬅ Back", "pay_menu")],
    ])
}

pub(crate) fn status_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback("📊 Check Status", "status")],
        vec![InlineKeyboardButton::callback(
            "👤 Contact Admin",
            "contact_admin",
        )],
    ])
}

pub(crate) fn cancel_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
        "🛑 Cancel Order",
        "cancel",
    )]])
}

pub(crate) fn optional_step_keyboard(skip_target: &str) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback(
            "⏭ Skip",
            format!("skip_optional:{skip_target}"),
        )],
        vec![InlineKeyboardButton::callback("🛑 Cancel Order", "cancel")],
    ])
}

pub(crate) fn alternative_domains_keyboard(
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

pub(crate) fn edit_order_list_keyboard(
    items: &[crate::db::OrderWithWebsite],
) -> InlineKeyboardMarkup {
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

pub(crate) fn edit_menu_keyboard(order_id: uuid::Uuid) -> InlineKeyboardMarkup {
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
