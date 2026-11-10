#!/bin/sh
# A stand-in for puku-cli, and (as `node`) for the script relay reads puku's quota with, for relay's
# end-to-end tests. Sessions run the claude stub on the puku home ($PUKU_CONFIG_DIR), after doing
# what is puku's own. Files in the home:
#   usage          printed for the quota script, as it prints puku's chat quota
#   sign-ins-seen  written by every session: the sign-in files it can see under ~/.config
#   node-seen      written by every session: the `node` a `#!/usr/bin/env node` launcher would run
# As the quota script, it reads the sign-in under $HOME as the real one does: none there is `lapsed`.
# Every session also renews its sign-in, as puku does, by writing `renewed` next to it.
set -eu
if [ "${1:-}" = -e ]; then
  # node -e SCRIPT URL PUKU_CLI
  [ "${3:-}" = "https://api-cli.puku.sh/puku/v1/subscription" ] || exit 1
  [ -f "$HOME/.config/${APP_NAME:-pukucode}/session.json" ] || { echo '{"lapsed":"not signed in"}'; exit 0; }
  cat "$PUKU_CONFIG_DIR/usage" 2>/dev/null || true
  exit 0
fi
case " $* " in *" --dangerously-skip-permissions "*) echo "unknown option" >&2; exit 2 ;; esac
if [ "${1:-}" != --version ]; then
  command -v node >"$PUKU_CONFIG_DIR/node-seen" || true
  ls "$HOME"/.config/*/session.json >"$PUKU_CONFIG_DIR/sign-ins-seen" 2>/dev/null || true
  touch "$HOME/.config/${APP_NAME:-pukucode}/renewed"
fi
CLAUDE_CONFIG_DIR=$PUKU_CONFIG_DIR exec "$(dirname "$0")/claude" "$@"
