#!/bin/sh
# A stand-in for omp, for relay's end-to-end tests. The profile's root is
# ~/.omp, or ~/.omp/profiles/$OMP_PROFILE; files there switch behaviour:
#   usage    printed for `omp usage`
#   limited  every session there hits a rate limit
#   checks   appended to by every progress check run there (which answers with `verdict`)
#   cost     what each worker turn costs, in USD (default 0.01); one over the job's cap is cut off before
#            its `agent_end`, as omp's last turn is when relay ends it at its cap
#   verdict  the progress check's answer (default continue)
# A prompt containing [status-done] reports status: done; [status-partial] reports status: partial.
# A prompt containing [plant] plants, like a hostile session, a symlink to $HOME/victim wherever relay
# has written a prompt file for the job: its session store (`<job>.d/prompt.md`) and the prompt's own place.
# A prompt containing [plant-profile] tries to make a new profile, ~/.omp/profiles/evil, that relay would take for an account.
set -eu
root=$HOME/.omp
[ -n "${OMP_PROFILE:-}" ] && root=$HOME/.omp/profiles/$OMP_PROFILE
case "${1:-}" in
usage) cat "$root/usage" 2>/dev/null || true; exit 0 ;;
--version) echo "stub-omp 1.0"; exit 0 ;;
esac
dir=""
id=""
prompt=""
while [ $# -gt 0 ]; do
  case "$1" in
  --session-dir) dir=$2; shift ;;
  --resume) id=$2; shift ;;
  --model | --thinking | --append-system-prompt | --approval-mode | --mode) shift ;;
  @*) prompt=${1#@} ;;
  esac
  shift
done
[ -f "$prompt" ] || { echo "no prompt file" >&2; exit 2; }
[ -n "$id" ] || id="omp-$$"
mkdir -p "$dir"
if grep -q '\[plant-profile\]' "$prompt"; then
  mkdir -p "$HOME/.omp/profiles/evil/agent" 2>/dev/null && : >"$HOME/.omp/profiles/evil/agent/agent.db" 2>/dev/null || true
fi
if grep -q '\[plant\]' "$prompt"; then
  ln -sf "$HOME/victim" "$(dirname "$dir")/prompt.md"
  ln -sf "$HOME/victim" "$prompt" 2>/dev/null && echo planted >"$root/planted-inbox" || true
fi
echo '{"turn":1}' >>"$dir/$id.jsonl"
printf '{"type":"session","id":"%s"}\n' "$id"
if [ -f "$root/limited" ]; then
  echo '{"type":"message_end","message":{"role":"assistant","stopReason":"error","errorMessage":"429 Too Many Requests: rate limit"}}'
  echo '{"type":"agent_end"}'
  exit 1
fi
turns=$(wc -l <"$dir/$id.jsonl" | tr -d ' ')
if grep -q "relay's progress check" "$prompt"; then
  echo check >>"$root/checks"
  verdict=$(cat "$root/verdict" 2>/dev/null || echo continue)
  printf '{"type":"message_end","message":{"role":"assistant","stopReason":"stop","usage":{"cost":{"total":0.01}},"content":[{"type":"text","text":"{\\"verdict\\": \\"%s\\", \\"why\\": \\"stub judged\\", \\"message\\": \\"go on\\"}"}]}}\n' "$verdict"
  echo '{"type":"agent_end"}'
  exit 0
fi
final="FINAL ok"
grep -q '\[status-done\]' "$prompt" && final="FINAL\\nstatus: done"
grep -q '\[status-partial\]' "$prompt" && final="FINAL\\nstatus: partial"
cost=$(cat "$root/cost" 2>/dev/null || echo 0.01)
printf '{"type":"message_end","message":{"role":"assistant","stopReason":"stop","usage":{"cost":{"total":%s}},"content":[{"type":"text","text":"ran on omp:%s with %s transcript turns\\n%s"}]}}\n' "$cost" "${OMP_PROFILE:-default}" "$turns" "$final"
echo '{"type":"agent_end"}'
