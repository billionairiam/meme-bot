#!/usr/bin/env bash
set -euo pipefail

if [ -z "${TELEGRAM_BOT_TOKEN:-}" ]; then
  echo "Error: TELEGRAM_BOT_TOKEN is not set"
  echo "Usage:"
  echo "  export TELEGRAM_BOT_TOKEN=your_bot_token"
  echo "  ./set_telegram_commands.sh"
  exit 1
fi

curl -sS -X POST "https://api.telegram.org/bot${TELEGRAM_BOT_TOKEN}/setMyCommands" \
  -H "Content-Type: application/json" \
  -d '{
    "commands": [
      {
        "command": "start",
        "description": "Open the main menu"
      },
      {
        "command": "create",
        "description": "Create a new order"
      },
      {
        "command": "status",
        "description": "Check your order status"
      },
      {
        "command": "cancel",
        "description": "Cancel the current order"
      }
    ]
  }'

echo
echo "Telegram bot commands updated."