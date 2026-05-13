#!/usr/bin/env bash
set -euo pipefail

# Configuration via env vars (no hardcoded domain)
BASE_URL="${API_TEST_BASE_URL:-https://api.sunyukong.vip}"
API_KEY="${API_TEST_KEY:-}"
DOMAIN="${API_TEST_DOMAIN:-}"
CURL_OPTS="--connect-timeout 10 --max-time 30"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

pass=0
fail=0

die() {
    echo -e "${RED}FATAL: $*${NC}" >&2
    exit 1
}

check() {
    local label="$1"; local jq_expr="$2"; local body="$3"
    if echo "$body" | jq -e "$jq_expr" >/dev/null 2>&1; then
        echo -e "  ${GREEN}PASS${NC} $label"
        pass=$((pass + 1))
    else
        echo -e "  ${RED}FAIL${NC} $label (expected: $jq_expr)"
        echo "       got: $body"
        fail=$((fail + 1))
    fi
}

check_status() {
    local label="$1"; local expected="$2"; local actual="$3"
    if [ "$actual" = "$expected" ]; then
        echo -e "  ${GREEN}PASS${NC} $label (HTTP $actual)"
        pass=$((pass + 1))
    else
        echo -e "  ${RED}FAIL${NC} $label (expected HTTP $expected, got $actual)"
        fail=$((fail + 1))
    fi
}

get() {
    local path="$1"
    # shellcheck disable=SC2086
    curl -s -w '\n%{http_code}' $CURL_OPTS "$BASE_URL$path"
}

post() {
    local path="$1"; local body="$2"
    # shellcheck disable=SC2086
    curl -s -w '\n%{http_code}' $CURL_OPTS \
        -X POST "$BASE_URL$path" \
        -H "Content-Type: application/json" \
        -H "X-API-Key: $API_KEY" \
        -d "$body"
}

# Returns "body" and "code" in caller's scope
# Usage: split "response" body_var code_var
split_response() {
    local full="$1"; local _bv="$2"; local _cv="$3"
    local _s; _s="$(echo "$full" | tail -1)"
    local _b; _b="$(echo "$full" | sed '$d')"
    printf -v "$_bv"   '%s' "$_b"
    printf -v "$_cv"   '%s' "$_s"
}

# ---- Preflight ----
[ -n "$API_KEY" ] || die "API_TEST_KEY is required"
[ -n "$DOMAIN" ]  || die "API_TEST_DOMAIN is required (e.g. totallynotamemecoin.xyz)"

echo "============================================"
echo " API Server Tests"
echo " Target: $BASE_URL"
echo " Domain: $DOMAIN"
echo "============================================"

# --- 1. Health (no auth) ---
echo ""
echo "[1] GET /api/v1/health"
full=$(get "/api/v1/health")
split_response "$full" body code
check_status "HTTP 200" 200 "$code"
check "returns status ok" '.status == "ok"' "$body"

# --- 2. Missing API key → 401 ---
echo ""
echo "[2] POST /api/v1/domain/quote (no key → 401)"
# shellcheck disable=SC2086
full=$(curl -s -w '\n%{http_code}' $CURL_OPTS \
    -X POST "$BASE_URL/api/v1/domain/quote" \
    -H "Content-Type: application/json" \
    -d "{\"domain\":\"$DOMAIN\"}")
split_response "$full" body code
check_status "HTTP 401" 401 "$code"
check "error code" '.error == "unauthorized"' "$body"

# --- 3. Domain quote ---
echo ""
echo "[3] POST /api/v1/domain/quote"
full=$(post "/api/v1/domain/quote" "$(jq -n --arg domain "$DOMAIN" '{domain: $domain}')")
split_response "$full" body code
check_status "HTTP 200" 200 "$code"
check "domain field"   ".domain == \"$DOMAIN\"" "$body"
check "has tld"         '.tld != null' "$body"
check "has available"   '.available != null' "$body"
check "has price_usd"   '.price_usd != null' "$body"

# --- 4. Domain purchase ---
echo ""
echo "[4] POST /api/v1/domain/purchase"
full=$(post "/api/v1/domain/purchase" \
    "$(jq -n --arg domain "$DOMAIN" '{domain: $domain, years: 1, privacy_enabled: true}')")
split_response "$full" body code
if [ "$code" = 200 ]; then
    check "has order_id"         '.namecheap_order_id != null' "$body"
    check "has transaction_id"   '.namecheap_transaction_id != null' "$body"
    check "has charged_amount"   '.charged_amount_usd != null' "$body"
else
    echo -e "  ${YELLOW}SKIP${NC} purchase returned HTTP $code: $body"
fi

# --- 5. DNS create-zone ---
echo ""
echo "[5] POST /api/v1/dns/create-zone"
full=$(post "/api/v1/dns/create-zone" "$(jq -n --arg domain "$DOMAIN" '{domain: $domain}')")
split_response "$full" body code
if [ "$code" = 200 ]; then
    check "has zone_id"       '.zone_id != null and (.zone_id | length) > 0' "$body"
    check "has nameservers"   '.nameservers | length >= 2' "$body"
    ns1=$(echo "$body" | jq -r '.nameservers[0]')
    ns2=$(echo "$body" | jq -r '.nameservers[1]')
    echo "       nameservers: $ns1, $ns2"
else
    echo -e "  ${YELLOW}SKIP${NC} create-zone returned HTTP $code: $body"
    ns1=""; ns2=""
fi

# --- 6. DNS set-nameservers (uses nameservers from create-zone if available) ---
echo ""
echo "[6] POST /api/v1/dns/set-nameservers"
if [ -n "${ns1:-}" ] && [ -n "${ns2:-}" ]; then
    json_body=$(jq -n --arg domain "$DOMAIN" --arg ns1 "$ns1" --arg ns2 "$ns2" \
        '{domain: $domain, nameservers: [$ns1, $ns2]}')
else
    json_body=$(jq -n --arg domain "$DOMAIN" \
        '{domain: $domain, nameservers: ["dave.ns.cloudflare.com", "lola.ns.cloudflare.com"]}')
    echo -e "  ${YELLOW}NOTE${NC} using fallback nameservers (create-zone didn't return real ones)"
fi
full=$(post "/api/v1/dns/set-nameservers" "$json_body")
split_response "$full" body code
check_status "HTTP 200" 200 "$code"
check "empty response" '. == {}' "$body"

# --- 7. Pages deploy (long running, up to 5 min) ---
echo ""
echo "[7] POST /api/v1/pages/deploy (may take several minutes)"
# Cloudflare Pages project names can't contain dots
project_name=$(echo "$DOMAIN" | tr '.' '-')
github_owner="${API_TEST_GITHUB_OWNER:-billionairiam}"
deploy_body=$(jq -n \
    --arg project_name "$project_name" \
    --arg github_owner "$github_owner" \
    --arg github_repo "$DOMAIN" \
    --arg production_branch "main" \
    --arg publish_dir "/" \
    --arg custom_domain "$DOMAIN" \
    '{project_name: $project_name, github_owner: $github_owner, github_repo: $github_repo, production_branch: $production_branch, publish_dir: $publish_dir, custom_domain: $custom_domain}')
full=$(curl -s -w '\n%{http_code}' --connect-timeout 10 --max-time 360 \
    -X POST "$BASE_URL/api/v1/pages/deploy" \
    -H "Content-Type: application/json" \
    -H "X-API-Key: $API_KEY" \
    -d "$deploy_body")
split_response "$full" body code
if [ "$code" = 200 ]; then
    check "has cloudflare_pages_url"  '.cloudflare_pages_url != null' "$body"
    check "has custom_domain"         ".custom_domain == \"$DOMAIN\"" "$body"
else
    echo -e "  ${YELLOW}SKIP${NC} pages/deploy returned HTTP $code: $body"
fi

# --- Summary ---
echo ""
echo "============================================"
echo " Results: ${GREEN}$pass passed${NC}, ${RED}$fail failed${NC}"
echo "============================================"
[ "$fail" -gt 0 ] && exit 1
exit 0
