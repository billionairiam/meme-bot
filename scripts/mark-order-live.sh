#!/usr/bin/env bash
# Mark a needs_manual_review order as live after admin has handled it.
#
# Usage:
#   ./scripts/mark-order-live.sh <order_id> [db_path]
#
# Examples:
#   ./scripts/mark-order-live.sh 67c5b61a-7696-4de8-89e7-36e7b4605aad
#   ./scripts/mark-order-live.sh 67c5b61a-7696-4de8-89e7-36e7b4605aad ./db.sqlite3
#
# Without arguments: lists all orders currently in needs_manual_review.

set -euo pipefail

DB="${2:-./db.sqlite3}"

if [ ! -f "$DB" ]; then
    echo "❌ Database not found: $DB"
    exit 1
fi

# No arguments — list orders needing review
if [ -z "${1:-}" ]; then
    echo "📋 Orders in needs_manual_review:"
    echo ""
    sqlite3 -header -column "$DB" "
        SELECT o.id AS order_id,
               u.telegram_user_id,
               o.amount_usdt,
               o.payment_chain,
               w.requested_domain,
               o.updated_at
        FROM orders o
        JOIN users u ON o.user_id = u.id
        LEFT JOIN websites w ON w.order_id = o.id
        WHERE o.site_status = 'needs_manual_review'
        ORDER BY o.updated_at DESC;
    "
    echo ""
    echo "Usage: $0 <order_id> [db_path]"
    exit 0
fi

ORDER_ID="$1"

# Verify the order exists and is in needs_manual_review
CURRENT=$(sqlite3 "$DB" "SELECT site_status FROM orders WHERE id = '$ORDER_ID';")
if [ -z "$CURRENT" ]; then
    echo "❌ Order not found: $ORDER_ID"
    exit 1
fi
if [ "$CURRENT" != "needs_manual_review" ]; then
    echo "⚠️  Order $ORDER_ID is in '$CURRENT', not 'needs_manual_review'"
    read -rp "Continue anyway? [y/N] " confirm
    if [[ ! "$confirm" =~ ^[Yy]$ ]]; then
        echo "Aborted."
        exit 0
    fi
fi

# Show order details before updating
echo "📦 Order details:"
sqlite3 -header -column "$DB" "
    SELECT o.id AS order_id,
           u.telegram_user_id,
           o.amount_usdt,
           o.payment_chain,
           o.site_status,
           w.requested_domain,
           w.deployment_status
    FROM orders o
    JOIN users u ON o.user_id = u.id
    LEFT JOIN websites w ON w.order_id = o.id
    WHERE o.id = '$ORDER_ID';
"
echo ""

read -rp "Mark this order as live? [y/N] " confirm
if [[ ! "$confirm" =~ ^[Yy]$ ]]; then
    echo "Aborted."
    exit 0
fi

sqlite3 "$DB" "
    UPDATE orders
    SET site_status = 'live', updated_at = datetime('now')
    WHERE id = '$ORDER_ID';

    UPDATE generation_jobs
    SET status = 'succeeded', finished_at = datetime('now'), locked_by = NULL, locked_at = NULL
    WHERE order_id = '$ORDER_ID' AND status != 'succeeded';

    UPDATE websites
    SET deployment_status = 'live', updated_at = datetime('now')
    WHERE order_id = '$ORDER_ID';
"

echo "✅ Order $ORDER_ID marked as live."
