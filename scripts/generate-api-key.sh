#!/usr/bin/env bash
set -euo pipefail

KEY=$(openssl rand -base64 48)

echo "API_SERVICE_API_KEY=$KEY"
echo ""
echo "Add the line above to both:"
echo "  .env.api-server (API server)"
echo "  .env.meme-bot    (meme-bot)"
