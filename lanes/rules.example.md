# Lane rules (template)

Prepend this file, edited for your project, to the brief of every lane, or have each brief point to it. A lane is a
non-interactive agent run: it cannot ask a question, and its session ends when its turn ends. Each rule below carries
its reason in one line.

## Finishing

1. **Run every long command in the foreground**, with the tool's timeout raised to its maximum, or poll with short
   foreground checks. Ending the turn ends the job, so anything still running is lost with its result.
2. **No background work at the end.** A subagent, monitor or background command cannot report after the session is gone.
   Wait for it, or do not start it.
3. **Stop what you start.** Servers, databases and watchers you launched are stopped before the final message, and `ps`
   shows none of yours left. A leftover process holds a port and memory for the next lane.
4. **Never end a turn with work or a check pending.** The final message is the report, written only when everything is
   done.

## Processes

5. **Never kill by pattern** (`pkill -f`, `killall`, `kill $(pgrep ...)`). A pattern matches other lanes' processes and
   the supervisor's own.
6. **Kill only your own PIDs**, recorded when you started the process, or a process group you created.

## Tests

7. **Targeted tests first**: the files and modules you changed, while you work.
8. **One full run, at the end**, of only the packages you changed. A full suite run repeatedly multiplies load on a
   machine shared with other lanes.
9. **A gate's result is its exit code.** Never report a gate green from `cmd | tail`, because the pipe's status is
   `tail`'s. Use `set -o pipefail` or read `${PIPESTATUS[0]}`, and state the exit code.

## Shared repository

10. **Work in your own worktree** and write every output file by absolute path.
11. **Commit early**, each reviewable step once its checks pass: a lane can be cut off at any moment, and committed work
    survives it.
12. **Commit with explicit paths** (`git commit -- <paths>`). Never `git add -A` or a bare `git commit`, which can take
    another lane's staged work.
13. **Touch only the files in your brief.** Report anything else that looks wrong instead of fixing it: another lane may
    be editing it.
14. **No `git stash`, `git reset`, `git clean` or branch switching in a shared checkout**, and never push unless the
    brief says so. Each can destroy work that exists nowhere else.
15. **Never print or write a credential, key or token**, in a file, a log or a commit message.

## Report

Your final message is the report. It states:

- **Done**: what changed, as a list of files.
- **Verified by execution**: each command you ran, its exit code and its counts.
- **Only read**: what you checked by reading and did not run.
- **Skipped**: every check you did not run, and why.
- **Found on the way**: defects outside your brief, one line each:
  `file:line | what is wrong | what it causes | the smallest fix`.

Do not rate your own work. The reader re-runs the commands you name.
