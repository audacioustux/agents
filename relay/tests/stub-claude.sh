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
#   prompts  appended to with every worker prompt (and the resume messages it is sent)
#   tmp-fs, hidden-write, shm-write, shm-over  written by a brief containing [tmpcheck]: /tmp's filesystem type, whether a write into a hidden directory worked, and whether /dev/shm takes a small write and refuses 5 MiB
#   rereview  the verdict (accept|reject) of a review that relay resumed after its reject (default accept)
#   cargo    appended to with every session's CARGO_INCREMENTAL (`unset` when it has none)
# A brief containing [idle] makes no commit; one containing [status-done] reports status: done;
# [verdict-accept] and [verdict-reject] also give a review's verdict; [uncommitted] leaves a changed tracked
# file and an untracked one behind after its commit; [filter] leaves a clean filter in the clone's own configuration
# plus diff.external and a textconv driver (and a .gitattributes using them) that touch $acct/filter-ran if anything on the host ever runs it.
# [fsmonitor] leaves a core.fsmonitor in the clone's own configuration that touches $acct/fsmonitor-ran if anything ever runs it.
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
case "$prompt" in *"[tmpcheck]"*)
  # 8 MB under /tmp, the filesystem /tmp is on, and whether a hidden sibling of the repository is writable.
  dd if=/dev/zero of=/tmp/relay-fill bs=1M count=8 2>/dev/null
  stat -f -c %T /tmp >"$acct/tmp-fs"
  if touch /dev/shm/relay-probe 2>/dev/null; then echo wrote >"$acct/shm-write"; else echo refused >"$acct/shm-write"; fi
  if dd if=/dev/zero of=/dev/shm/relay-fill bs=1M count=5 2>/dev/null; then echo wrote >"$acct/shm-over"; else echo refused >"$acct/shm-over"; fi
  if touch "$(dirname "$PWD")/state/probe-write" 2>/dev/null; then echo wrote >"$acct/hidden-write"; else echo refused >"$acct/hidden-write"; fi ;;
esac
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
case "$prompt" in
*"[verdict-reject]"*) final="FINAL\\nstatus: done\\nverdict: reject\\nreason: test-failure\\nnotes: parse panics on empty input" ;;
*"[verdict-accept]"*) final="FINAL\\nstatus: done\\nverdict: accept" ;;
*"Judge its branch again"*) final="FINAL\\nstatus: done\\nverdict: $(cat "$acct/rereview" 2>/dev/null || echo accept)" ;;
esac
printf '%s\n--\n' "$prompt" >>"$acct/prompts"
case "$prompt" in *"[idle]"*) ;; *)
if [ -w . ] && [ -e .git ]; then
  echo "work $sid $turns" >relay-output.txt
  git add relay-output.txt
  git commit -q -m "stub work"
fi ;;
esac
case "$prompt" in *"[uncommitted]"*)
  if [ -w . ] && [ -e .git ]; then echo late >>relay-output.txt; echo x >untracked.txt; fi ;;
esac
case "$prompt" in *"[filter]"*)
  if [ -w . ] && [ -e .git ]; then
    git config filter.evil.clean "touch $acct/filter-ran; cat"
    git config diff.external "touch $acct/filter-ran; true"
    git config diff.x.textconv "sh -c 'touch $acct/filter-ran; cat \"\$1\"' sh"
    git config diff.x.command "touch $acct/filter-ran; true"
    printf '*.txt filter=evil diff=x\n' >.gitattributes
    echo y >extra.txt
  fi ;;
esac
case "$prompt" in *"[fsmonitor]"*)
  if [ -w . ] && [ -e .git ]; then git config core.fsmonitor "touch $acct/fsmonitor-ran; false"; fi ;;
esac
# printf, not echo: dash's echo turns \n into a real newline and splits the JSON line.
printf '{"type":"result","subtype":"success","is_error":false,"session_id":"%s","result":"ran on %s with %s transcript turns\\n%s","num_turns":1,"total_cost_usd":%s}\n' "$sid" "$(basename "$acct")" "$turns" "$final" "$cost"
