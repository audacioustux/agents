//! ask: a one-shot, read-only call to any harness, run now in the caller's
//! own environment and account, under the safety contract in
//! [`crate::harness::bridge`]. The quick second opinion; `queue` is for work.
//! It needs no `relay.toml`.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::Result;

use crate::harness::bridge::{Call, delivery, session_name};
use crate::harness::prompt::{DIFF_LIMIT, Mode, Prompt, Subject, read_subject};
use crate::harness::{Harness, by_id};

#[derive(Debug, clap::Args)]
#[command(
    after_help = "Text after `--` is all subject, so `relay ask claude ask -- what does --continue do` asks about the flag."
)]
pub struct Args {
    /// The harness: claude or omp.
    #[arg(value_parser = harness)]
    agent: &'static dyn Harness,
    /// How the prompt is framed: ask, plan, adversarial, or review (of `git diff BASE..HEAD`).
    #[arg(value_parser = clap::value_parser!(Mode))]
    mode: Mode,
    /// The question, a file to read, or a description. Not for review.
    subject: Vec<String>,
    /// Fork this session (never extend it).
    #[arg(long, conflicts_with = "fresh")]
    resume: Option<String>,
    /// Start a new session (the default; for callers that say so).
    #[arg(long)]
    fresh: bool,
    #[arg(long)]
    model: Option<String>,
    #[arg(long, default_value = "HEAD~1")]
    base: String,
    #[arg(long, default_value = "HEAD")]
    head: String,
    /// Further instructions, appended to the prompt.
    #[arg(long, default_value = "")]
    extra: String,
    /// Where to run, and what relative subject paths and the diff are read from.
    #[arg(long)]
    cwd: Option<PathBuf>,
    /// Print the command (prompt redacted) as JSON; start nothing.
    #[arg(long)]
    dry_run: bool,
    /// Refused: it picks the latest session anywhere, not one you named.
    #[arg(long = "continue", short = 'c', hide = true)]
    continue_: bool,
}

fn harness(id: &str) -> Result<&'static dyn Harness, String> {
    by_id(id).map_err(|e| e.0)
}

impl Args {
    fn subject(&self) -> String {
        self.subject.join(" ").trim().to_owned()
    }

    /// A blank `--resume` is no resume.
    fn resume(&self) -> Option<&str> {
        self.resume.as_deref().map(str::trim).filter(|r| !r.is_empty())
    }
}

/// `git <args>` output, cut at [`DIFF_LIMIT`] with the cut marked.
fn git_diff(args: &[&str], cwd: &std::path::Path) -> Result<(String, bool), String> {
    let out = Command::new("git").args(args).current_dir(cwd).output().map_err(|e| format!("git: {e}"))?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let truncated = text.len() > DIFF_LIMIT;
    if truncated {
        let mut cut = DIFF_LIMIT;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("\n[relay ask: diff truncated]");
    } else if !out.status.success() {
        return Err(format!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok((text, truncated))
}

/// The prompt for this mode, and what the dry run reports about how it was cut.
fn build_prompt(
    a: &Args,
    cwd: &std::path::Path,
) -> Result<(String, serde_json::Map<String, serde_json::Value>), String> {
    let identity = a.agent.identity();
    let mut meta = serde_json::Map::new();
    let prompt = if a.mode == Mode::Review {
        let range = format!("{}..{}", a.base, a.head);
        let (stat, stat_cut) = git_diff(&["diff", "--stat", &range, "--"], cwd)?;
        let (diff, diff_cut) =
            git_diff(&["diff", "--no-ext-diff", "--find-renames", "--function-context", &range, "--"], cwd)?;
        meta.insert("diffTruncated".into(), diff_cut.into());
        meta.insert("diffStatTruncated".into(), stat_cut.into());
        let diff = format!("## Diff stat\n{stat}\n\n## Diff\n{diff}");
        Prompt {
            mode: a.mode,
            subject: &range,
            subject_text: "",
            extra: &a.extra,
            base: &a.base,
            head: &a.head,
            diff: &diff,
            identity,
        }
        .render()
    } else {
        // A dry run reads nothing, so it cannot leak a file into its output.
        let subject = a.subject();
        let subj = if a.dry_run { Subject::default() } else { read_subject(&subject, cwd) };
        let display = subj.path.as_ref().map_or_else(|| subject.clone(), |p| format!("{subject} ({})", p.display()));
        let text = if subj.truncated {
            format!("{}\n\n[relay ask: target file content truncated]", subj.text)
        } else {
            subj.text
        };
        meta.insert("subjectTruncated".into(), subj.truncated.into());
        if let Some(p) = &subj.path {
            meta.insert("targetResolvedPath".into(), p.display().to_string().into());
        }
        Prompt {
            mode: a.mode,
            subject: &display,
            subject_text: &text,
            extra: &a.extra,
            base: &a.base,
            head: &a.head,
            diff: "",
            identity,
        }
        .render()
    };
    Ok((prompt, meta))
}

/// Runs the call and returns the harness's exit code.
pub fn run(a: &Args, json: bool) -> Result<u8> {
    if a.continue_ {
        fail!(Refused, "--continue/-c resumes the most recent session globally. Use --resume <id>.");
    }
    match (a.mode, a.subject().is_empty()) {
        (Mode::Review, false) => fail!(Usage, "review mode does not take a positional subject; use --base/--head"),
        (Mode::Review, true) | (_, false) => {}
        (_, true) => fail!(Usage, "{} mode requires a question, file path, or description", a.mode),
    }
    let cwd = match &a.cwd {
        Some(c) => c.clone(),
        None => std::env::current_dir()?,
    };
    let (prompt, meta) = build_prompt(a, &cwd).map_err(anyhow::Error::msg)?;

    let resume = a.resume();
    let new_session_id = if resume.is_some() { String::new() } else { uuid::Uuid::now_v7().to_string() };
    let stamp = crate::os::local(jiff::Timestamp::now()).strftime("%Y-%m-%dT%H-%M-%S%z").to_string();
    let name = session_name(a.agent, a.mode.as_str(), &stamp);
    // A private dir for a prompt file, removed when the call ends.
    let scratch = std::env::temp_dir().join(format!("relay-ask-{}-{}", std::process::id(), new_session_id));
    let call = Call {
        harness: a.agent,
        prompt: &prompt,
        model: a.model.as_deref(),
        resume,
        new_session_id: &new_session_id,
        name: &name,
        scratch: &scratch,
    };
    let built = match call.build() {
        Ok(b) => b,
        Err(refusal) => fail!(Refused, "{}", refusal.0),
    };

    if a.dry_run {
        let mut prompt_info =
            serde_json::json!({ "redacted": true, "bytes": prompt.len(), "delivery": delivery(&built) });
        prompt_info.as_object_mut().expect("object").extend(meta);
        // The prompt is never in argv; only the name of its file can be.
        let command: Vec<String> =
            std::iter::once(&built.program).chain(&built.args).map(|x| x.to_string_lossy().into_owned()).collect();
        let out = serde_json::json!({
            "agent": a.agent.id(),
            "model": a.model,
            "resume": resume,
            "fresh": a.fresh,
            "newSessionId": if resume.is_some() { None } else { Some(&new_session_id) },
            "prompt": prompt_info,
            "command": command,
        });
        crate::reply::reply(true, &out, "")?;
        return Ok(0);
    }

    if resume.is_none() {
        let note = serde_json::json!({ "newSessionId": new_session_id });
        if json { eprintln!("{note}") } else { eprintln!("relay ask: new session id = {new_session_id}") }
    }
    // Made new (nothing already there, a symlink included), and written through its handle.
    if !built.files.is_empty() {
        crate::hostdir::HostDir::create_new(&scratch)
            .and_then(|inbox| built.write_files(&inbox))
            .map_err(|e| anyhow::anyhow!("writing the prompt file: {e}"))?;
    }
    let result = spawn_and_wait(&built, &cwd, a.agent.id());
    let _ = std::fs::remove_dir_all(&scratch);
    result.map_err(anyhow::Error::msg)
}

fn spawn_and_wait(built: &crate::harness::Invocation, cwd: &std::path::Path, id: &str) -> Result<u8, String> {
    let mut child = Command::new(&built.program)
        .args(&built.args)
        .current_dir(cwd)
        .stdin(if built.stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .spawn()
        .map_err(|e| format!("starting {id}: {e}"))?;
    if let (Some(text), Some(mut stdin)) = (&built.stdin, child.stdin.take()) {
        stdin.write_all(text.as_bytes()).map_err(|e| format!("writing the prompt: {e}"))?;
    }
    #[allow(
        clippy::disallowed_methods,
        reason = "`relay ask` runs the harness in the caller's own foreground, under their terminal and Ctrl-C"
    )]
    let status = child.wait().map_err(|e| e.to_string())?;
    Ok(status.code().and_then(|c| u8::try_from(c).ok()).unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Debug, Parser)]
    struct Cli {
        #[command(flatten)]
        a: Args,
    }

    fn args(s: &[&str]) -> Result<Args, clap::Error> {
        Cli::try_parse_from(std::iter::once("ask").chain(s.iter().copied())).map(|c| c.a)
    }

    fn refusal(s: &[&str]) -> String {
        run(&args(s).unwrap(), false).unwrap_err().to_string()
    }

    #[test]
    fn continue_is_refused_but_allowed_as_text_after_double_dash() {
        assert!(refusal(&["claude", "ask", "x", "--continue"]).contains("--resume <id>"));
        assert!(refusal(&["claude", "ask", "-c", "x"]).contains("--resume <id>"));
        let a = args(&["claude", "ask", "--", "what", "does", "--continue", "do"]).unwrap();
        assert_eq!((a.subject(), a.continue_), ("what does --continue do".into(), false));
    }

    #[test]
    fn bad_combinations() {
        assert!(args(&["claude", "ask", "x", "--resume", "a", "--fresh"]).is_err());
        assert!(args(&["codex", "ask", "x"]).unwrap_err().to_string().contains("Supported: claude, omp"));
        assert!(args(&["claude", "nope", "x"]).unwrap_err().to_string().contains("unknown mode"));
        assert!(args(&["claude", "ask", "x", "--yolo"]).is_err());
        // A flag is never taken as the value before it.
        assert!(args(&["claude", "ask", "x", "--extra", "--dry-run"]).is_err());
        assert_eq!(args(&["claude", "ask", "x", "--extra=--dry-run"]).unwrap().extra, "--dry-run");
        assert!(refusal(&["claude", "review", "x"]).contains("does not take a positional subject"));
    }

    #[test]
    fn values_and_defaults() {
        let a = args(&["omp", "review", "--model=opus", "--base", "main", "--resume", "  "]).unwrap();
        assert_eq!((a.agent.id(), a.mode), ("omp", Mode::Review));
        assert_eq!(a.model.as_deref(), Some("opus"));
        assert_eq!((a.base.as_str(), a.head.as_str()), ("main", "HEAD"));
        assert_eq!(a.resume(), None, "a blank --resume is no resume");
    }
}
