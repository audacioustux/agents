#!/usr/bin/env bash
# smoke.sh — end-to-end check that puku-proxy is deployed and reachable.
#
# Usage:
#   HOST=https://puku.example.com PORT=8787 TOKEN=pk_live_xxx ./smoke.sh
#
# Exit codes:
#   0  every probe passed
#   1  usage error
#   2  /healthz failed
#   3  /v1/models failed (auth/network)
#   4  /v1/chat/completions returned a *proxy-level* error (auth/parse)
#   5  /v1/chat/completions returned 500 with "Not logged in" — upstream
#      puku-cli rejected env-var auth; proxy is healthy but useless until
#      that's resolved (see README "Upstream auth" section).

set -uo pipefail

: "${HOST:?HOST is required, e.g. https://puku.example.com}"
: "${PORT:=8787}"
: "${TOKEN:?TOKEN is required, the bearer token from PUKU_PROXY_AUTH_KEYS}"
: "${MODEL:=puku-default}"

base="${HOST%/}"
url="${base}:${PORT}"

red()    { printf '\033[31m%s\033[0m\n' "$*"; }
green()  { printf '\033[32m%s\033[0m\n' "$*"; }
yellow() { printf '\033[33m%s\033[0m\n' "$*"; }
bold()   { printf '\033[1m%s\033[0m\n' "$*"; }

section() { printf '\n%s\n' "── $* ──"; }

# ── 1. /healthz (no auth) ─────────────────────────────────────────────
section "1/3 GET /healthz"
hz_body="$(curl -fsS --max-time 10 "${url}/healthz" || true)"
if [[ -n "$hz_body" ]]; then
  green "  ok  $hz_body"
else
  red   "  FAIL /healthz did not return 2xx — proxy isn't up yet"
  exit 2
fi

# ── 2. /v1/models (auth required) ─────────────────────────────────────
section "2/3 GET /v1/models"
models_out="$(mktemp)"
models_code="$(curl -sS --max-time 10 -o "$models_out" -w '%{http_code}' \
  -H "authorization: Bearer ${TOKEN}" \
  "${url}/v1/models" || true)"
case "$models_code" in
  200) green "  ok  $(head -c 200 "$models_out")…" ;;
  401) red   "  FAIL 401 — token rejected. Check PUKU_PROXY_AUTH_KEYS on Dokploy."
        exit 3 ;;
  *)   red   "  FAIL HTTP $models_code — $(head -c 200 "$models_out")"
        exit 3 ;;
esac
rm -f "$models_out"

# ── 3. /v1/chat/completions (the actual chat path) ─────────────────────
section "3/3 POST /v1/chat/completions"
chat_out="$(mktemp)"
chat_code="$(curl -sS --max-time 60 -o "$chat_out" -w '%{http_code}' \
  -H "authorization: Bearer ${TOKEN}" \
  -H 'content-type: application/json' \
  -d "{\"model\":\"${MODEL}\",\"messages\":[{\"role\":\"user\",\"content\":\"ping\"}],\"stream\":false}" \
  "${url}/v1/chat/completions" || true)"

case "$chat_code" in
  200)
    green "  ok  upstream chat call succeeded"
    head -c 400 "$chat_out"
    printf '\n'
    ;;
  401|403|400)
    red   "  FAIL HTTP $chat_code — proxy-level error"
    cat "$chat_out"
    rm -f "$chat_out"
    exit 4
    ;;
  500)
    body="$(cat "$chat_out")"
    if grep -qi 'not logged in' <<<"$body"; then
      yellow "  WARN 500 'Not logged in' — proxy is healthy, but puku-cli"
      yellow "        rejected PUKU_AI_API_KEY. See README 'Upstream auth'."
      cat "$chat_out"
      rm -f "$chat_out"
      exit 5
    fi
    red   "  FAIL HTTP 500 — $(head -c 400 "$chat_out")"
    rm -f "$chat_out"
    exit 4
    ;;
  *)
    red   "  FAIL HTTP $chat_code — $(head -c 400 "$chat_out")"
    rm -f "$chat_out"
    exit 4
    ;;
esac

rm -f "$chat_out"
green "\nAll probes passed. Proxy is healthy and chat is working."
