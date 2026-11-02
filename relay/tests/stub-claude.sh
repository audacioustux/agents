#!/bin/sh
# A stand-in for the claude CLI, for relay's end-to-end tests.
# It reads per-account switches from files in $CLAUDE_CONFIG_DIR:
#   usage    printed for `-p /usage`
#   limited  every session there hits its weekly limit
set -eu
acct=$CLAUDE_CONFIG_DIR
case " $* " in
*" /usage "*) cat "$acct/usage" 2>/dev/null || true; exit 0 ;;
*" --version "*) echo "stub 1.0"; exit 0 ;;
esac
sid=""
resume=""
while [ $# -gt 0 ]; do
  case "$1" in
  --session-id) sid=$2; shift ;;
  --resume) sid=$2; resume=1; shift ;;
  esac
  shift
done
cat >/dev/null # the prompt
key=$(pwd | sed 's/[^A-Za-z0-9]/-/g')
transcript="$acct/projects/$key/$sid.jsonl"
if [ -n "$resume" ] && [ ! -f "$transcript" ]; then
  echo '{"type":"result","subtype":"error_during_execution","is_error":true,"result":"No conversation found"}'
  exit 1
fi
mkdir -p "$(dirname "$transcript")"
echo '{"turn":1}' >>"$transcript"
echo '{"type":"system","subtype":"init"}'
if [ -f "$acct/limited" ]; then
  echo '{"type":"rate_limit_event","rate_limit_info":{"unifiedWindows":{"seven_day":{"utilization":1.0,"resetsAt":4102444800}}}}'
  echo '{"type":"result","subtype":"success","is_error":true,"result":"You'"'"'ve hit your weekly limit · resets Jan 1, 12am (UTC)","num_turns":1,"total_cost_usd":0.01}'
  exit 1
fi
echo '{"type":"rate_limit_event","rate_limit_info":{"unifiedWindows":{"seven_day":{"utilization":0.3,"resetsAt":4102444800}}}}'
if [ -w . ] && [ -e .git ]; then
  echo work >relay-output.txt
  git add relay-output.txt
  git commit -q -m "stub work"
fi
turns=$(wc -l <"$transcript" | tr -d ' ')
# printf, not echo: dash's echo turns \n into a real newline and splits the JSON line.
printf '{"type":"result","subtype":"success","is_error":false,"result":"ran on %s with %s transcript turns\\nFINAL ok","num_turns":1,"total_cost_usd":0.02}\n' "$(basename "$acct")" "$turns"
