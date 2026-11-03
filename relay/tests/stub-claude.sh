#!/bin/sh
# A stand-in for the claude CLI, for relay's end-to-end tests.
# It reads per-account switches from files in $CLAUDE_CONFIG_DIR:
#   usage    printed for `-p /usage`
#   limited  every session there hits its weekly limit
#   cost     what each session costs, in USD (default 0.02)
#   verdict  the progress check's answer (default continue)
#   checks   appended to by every progress check run there
#   hold     sessions wait, after starting, until the file is removed
#   fail     every session there ends in an error
#   decision a decision hook's answer, one line of JSON (default defer)
#   decisions appended to by every decision hook run there
#   argv     appended to with every session's arguments
#   cargo    appended to with every session's CARGO_INCREMENTAL (`unset` when it has none)
# A brief containing [idle] makes no commit; one containing [status-done] reports status: done.
set -eu
acct=$CLAUDE_CONFIG_DIR
case " $* " in
*" /usage "*) cat "$acct/usage" 2>/dev/null || true; exit 0 ;;
*" --version "*) echo "stub 1.0"; exit 0 ;;
esac
echo "$*" >>"$acct/argv"
echo "${CARGO_INCREMENTAL-unset}" >>"$acct/cargo"
sid=""
resume=""
fork=""
while [ $# -gt 0 ]; do
  case "$1" in
  --session-id) sid=$2; shift ;;
  --resume) sid=$2; resume=1; shift ;;
  --fork-session) fork=1 ;;
  esac
  shift
done
prompt=$(cat)
cost=$(cat "$acct/cost" 2>/dev/null || echo 0.02)
key=$(pwd | sed 's/[^A-Za-z0-9]/-/g')
transcript="$acct/projects/$key/$sid.jsonl"
if [ -n "$resume" ] && [ ! -f "$transcript" ]; then
  echo '{"type":"result","subtype":"error_during_execution","is_error":true,"result":"No conversation found"}'
  exit 1
fi
if [ -n "$fork" ]; then
  sid="$sid-fork$$"
  cp "$transcript" "$acct/projects/$key/$sid.jsonl"
  transcript="$acct/projects/$key/$sid.jsonl"
fi
mkdir -p "$(dirname "$transcript")"
echo '{"turn":1}' >>"$transcript"
echo '{"type":"system","subtype":"init"}'
while [ -f "$acct/hold" ]; do sleep 0.1; done
if [ -f "$acct/fail" ]; then
  echo '{"type":"result","subtype":"error_during_execution","is_error":true,"result":"boom","num_turns":1,"total_cost_usd":0.01}'
  exit 1
fi
if [ -f "$acct/limited" ]; then
  echo '{"type":"rate_limit_event","rate_limit_info":{"unifiedWindows":{"seven_day":{"utilization":1.0,"resetsAt":4102444800}}}}'
  echo '{"type":"result","subtype":"success","is_error":true,"result":"You'"'"'ve hit your weekly limit · resets Jan 1, 12am (UTC)","num_turns":1,"total_cost_usd":0.01}'
  exit 1
fi
echo '{"type":"rate_limit_event","rate_limit_info":{"unifiedWindows":{"seven_day":{"utilization":0.3,"resetsAt":4102444800}}}}'
turns=$(wc -l <"$transcript" | tr -d ' ')
case "$prompt" in
*"relay's progress check"*)
  echo check >>"$acct/checks"
  verdict=$(cat "$acct/verdict" 2>/dev/null || echo continue)
  printf '{"type":"result","subtype":"success","is_error":false,"result":"{\\"verdict\\": \\"%s\\", \\"why\\": \\"stub judged\\", \\"message\\": \\"run the formatter\\"}","num_turns":1,"total_cost_usd":0.01}\n' "$verdict"
  exit 0
  ;;
*"relay's decision hook"*)
  echo decide >>"$acct/decisions"
  answer=$(cat "$acct/decision" 2>/dev/null || echo '{"action": "defer"}')
  esc=$(printf '%s' "$answer" | sed 's/\\/\\\\/g; s/"/\\"/g')
  printf '{"type":"result","subtype":"success","is_error":false,"result":"%s","num_turns":1,"total_cost_usd":0.01}\n' "$esc"
  exit 0
  ;;
esac
final="FINAL ok"
case "$prompt" in *"[status-done]"*) final="FINAL\\nstatus: done" ;; esac
case "$prompt" in *"[idle]"*) ;; *)
if [ -w . ] && [ -e .git ]; then
  echo "work $sid $turns" >relay-output.txt
  git add relay-output.txt
  git commit -q -m "stub work"
fi ;;
esac
# printf, not echo: dash's echo turns \n into a real newline and splits the JSON line.
printf '{"type":"result","subtype":"success","is_error":false,"session_id":"%s","result":"ran on %s with %s transcript turns\\n%s","num_turns":1,"total_cost_usd":%s}\n' "$sid" "$(basename "$acct")" "$turns" "$final" "$cost"
