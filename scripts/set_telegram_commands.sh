#!/usr/bin/env bash
set -euo pipefail

ENV_FILE="${MEME_BOT_ENV_FILE:-.env.meme-bot}"

if [ -f "${ENV_FILE}" ]; then
  set -a
  # shellcheck disable=SC1090
  source "${ENV_FILE}"
  set +a
fi

if [ -z "${TELEGRAM_BOT_TOKEN:-}" ]; then
  echo "Error: TELEGRAM_BOT_TOKEN is not set"
  echo "Usage:"
  echo "  ./scripts/set_telegram_commands.sh"
  echo "  MEME_BOT_ENV_FILE=.env.meme-bot ./scripts/set_telegram_commands.sh"
  exit 1
fi

curl -sS -X POST "https://api.telegram.org/bot${TELEGRAM_BOT_TOKEN}/setMyCommands" \
  -H "Content-Type: application/json" \
  -d '{
    "commands": [
      {
        "command": "start",
        "description": "Main menu"
      },
      {
        "command": "create",
        "description": "Create order"
      },
      {
        "command": "status",
        "description": "Order status"
      },
      {
        "command": "history",
        "description": "Paid history"
      },
      {
        "command": "continue",
        "description": "Continue order"
      },
      {
        "command": "cancel",
        "description": "Cancel order"
      }
    ]
  }'

echo
echo "Telegram bot commands updated."
