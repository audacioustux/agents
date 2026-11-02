//! ask: a one-shot, read-only call to any harness, run now in the caller's
//! own environment and account, under the safety contract in
//! [`crate::harness::bridge`]. The quick second opinion; `queue` is for work.
//! It needs no `relay.toml`.
//!
//! relay ask AGENT MODE [SUBJECT...] [--resume ID | --fresh] [--model M]
//!           [--base REF --head REF] [--extra TEXT] [--cwd DIR] [--dry-run]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::harness::bridge::{Call, delivery, session_name};
use crate::harness::prompt::{DIFF_LIMIT, Mode, Prompt, Subject, read_subject};
use crate::harness::{ALL, Harness, by_id};

pub const USAGE: &str = "usage: relay ask AGENT MODE [SUBJECT...] [--resume ID | --fresh] [--model M] \
[--base REF --head REF] [--extra TEXT] [--cwd DIR] [--dry-run]
modes: ask, plan, adversarial, review";

#[derive(Debug)]
struct Args {
    agent: &'static dyn Harness,
    mode: Mode,
    subject: String,
    resume: Option<String>,
    fresh: bool,
    model: Option<String>,
    base: String,
    head: String,
    extra: String,
    cwd: Option<PathBuf>,
    dry_run: bool,
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_owned())
}

fn parse(argv: &[String]) -> Result<Args, String> {
    // `cargo run --` and task runners can leave a leading `--`.
    let argv = argv.strip_prefix(&["--".to_owned()]).unwrap_or(argv);
    let agent = by_id(argv.first().ok_or("unknown agent: (missing)")?).map_err(|e| e.0)?;
    let mode: Mode = argv.get(1).ok_or("unknown mode: (missing)")?.parse()?;
    let rest = &argv[2..];
    // Checked before anything else: --continue resumes the latest session
    // anywhere, not one the caller named. After `--` it is only text.
    if rest.iter().take_while(|a| *a != "--").any(|a| a == "--continue" || a == "-c") {
        return Err("--continue/-c resumes the most recent session globally. Use --resume <id>.".into());
    }
    let mut a = Args {
        agent,
        mode,
        subject: String::new(),
        resume: None,
        fresh: false,
        model: None,
        base: "HEAD~1".into(),
        head: "HEAD".into(),
        extra: String::new(),
        cwd: None,
        dry_run: false,
    };
    let mut words = Vec::new();
    let mut it = rest.iter().peekable();
    while let Some(arg) = it.next() {
        if arg == "--" {
            words.extend(it.by_ref().cloned());
            break;
        }
        let Some(flag) = arg.strip_prefix("--") else {
            words.push(arg.clone());
            continue;
        };
        let (key, inline) = match flag.split_once('=') {
            Some((k, v)) => (k, Some(v.to_owned())),
            None => (flag, None),
        };
        // A value that looks like a flag is a missing value, not text: `--extra --dry-run`
        // must not start the CLI. A value starting with `-` goes inline: `--extra=-x`.
        let mut value = || {
            inline
                .clone()
                .or_else(|| it.next_if(|v| !v.starts_with('-')).cloned())
                .ok_or(format!("--{key} needs a value (for one starting with '-': --{key}=VALUE)"))
        };
        match key {
            "fresh" => a.fresh = true,
            "dry-run" => a.dry_run = true,
            "resume" => a.resume = non_empty(&value()?),
            "model" => a.model = non_empty(&value()?),
            "base" => a.base = non_empty(&value()?).unwrap_or_else(|| "HEAD~1".into()),
            "head" => a.head = non_empty(&value()?).unwrap_or_else(|| "HEAD".into()),
            "extra" => a.extra = value()?,
            "cwd" => a.cwd = non_empty(&value()?).map(PathBuf::from),
            _ => return Err(format!("unknown flag: --{key}")),
        }
    }
    if a.resume.is_some() && a.fresh {
        return Err("--resume and --fresh are mutually exclusive".into());
    }
    words.join(" ").trim().clone_into(&mut a.subject);
    Ok(a)
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
        let subj = if a.dry_run { Subject::default() } else { read_subject(&a.subject, cwd) };
        let display =
            subj.path.as_ref().map_or_else(|| a.subject.clone(), |p| format!("{} ({})", a.subject, p.display()));
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

fn run(a: &Args) -> Result<u8, String> {
    if a.mode == Mode::Review && !a.subject.is_empty() {
        return Err("review mode does not take a positional subject; use --base/--head".into());
    }
    if a.mode != Mode::Review && a.subject.is_empty() {
        return Err(format!("{} mode requires a question, file path, or description", a.mode));
    }
    let cwd = match &a.cwd {
        Some(c) => c.clone(),
        None => std::env::current_dir().map_err(|e| e.to_string())?,
    };
    let (prompt, meta) = build_prompt(a, &cwd)?;

    let new_session_id = if a.resume.is_some() { String::new() } else { uuid::Uuid::now_v7().to_string() };
    let stamp = jiff::Timestamp::now().strftime("%Y-%m-%dT%H-%M-%S").to_string();
    let name = session_name(a.agent, a.mode.as_str(), &stamp);
    // A private dir for a prompt file, removed when the call ends.
    let scratch = std::env::temp_dir().join(format!("relay-ask-{}-{}", std::process::id(), new_session_id));
    let call = Call {
        harness: a.agent,
        prompt: &prompt,
        model: a.model.as_deref(),
        resume: a.resume.as_deref(),
        new_session_id: &new_session_id,
        name: &name,
        scratch: &scratch,
    };
    let built = call.build().map_err(|e| e.0)?;

    if a.dry_run {
        let bytes = prompt.len();
        let mut prompt_info = serde_json::json!({ "redacted": true, "bytes": bytes, "delivery": delivery(&built) });
        prompt_info.as_object_mut().expect("object").extend(meta);
        // The prompt is never in argv; only the name of its file can be.
        let command: Vec<String> =
            std::iter::once(&built.program).chain(&built.args).map(|x| x.to_string_lossy().into_owned()).collect();
        let out = serde_json::json!({
            "agent": a.agent.id(),
            "model": a.model,
            "resume": a.resume,
            "fresh": a.fresh,
            "newSessionId": if a.resume.is_some() { None } else { Some(&new_session_id) },
            "prompt": prompt_info,
            "command": command,
        });
        println!("{}", serde_json::to_string_pretty(&out).expect("json"));
        return Ok(0);
    }

    if a.resume.is_none() {
        eprintln!("relay ask: new session id = {new_session_id}");
    }
    built.write_files().map_err(|e| format!("writing the prompt file: {e}"))?;
    let result = spawn_and_wait(&built, &cwd, a.agent.id());
    let _ = std::fs::remove_dir_all(&scratch);
    result
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
    let status = child.wait().map_err(|e| e.to_string())?;
    Ok(status.code().and_then(|c| u8::try_from(c).ok()).unwrap_or(1))
}

/// `argv` is everything after `relay ask`. Returns the child's exit code.
pub fn main(argv: &[String]) -> anyhow::Result<u8> {
    if argv.is_empty() || argv.len() == 1 && matches!(argv[0].as_str(), "--help" | "-h" | "help") {
        let ids: Vec<_> = ALL.iter().map(|h| h.id()).collect();
        println!("{USAGE}\nagents: {}", ids.join(", "));
        return Ok(0);
    }
    parse(argv).and_then(|a| run(&a)).map_err(anyhow::Error::msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Result<Args, String> {
        parse(&s.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn continue_is_refused_but_allowed_as_text_after_double_dash() {
        assert!(args(&["claude", "ask", "x", "--continue"]).unwrap_err().contains("--resume <id>"));
        assert!(args(&["claude", "ask", "-c", "x"]).is_err());
        assert_eq!(
            args(&["claude", "ask", "--", "what", "does", "--continue", "do"]).unwrap().subject,
            "what does --continue do"
        );
    }

    #[test]
    fn bad_combinations() {
        assert!(args(&["claude", "ask", "x", "--resume", "a", "--fresh"]).unwrap_err().contains("mutually exclusive"));
        assert!(args(&["codex", "ask", "x"]).unwrap_err().contains("unknown agent"));
        assert!(args(&["claude", "nope", "x"]).unwrap_err().contains("unknown mode"));
        assert!(args(&["claude", "ask", "x", "--yolo"]).unwrap_err().contains("unknown flag"));
        // A flag is never taken as the value before it.
        assert!(args(&["claude", "ask", "x", "--extra", "--dry-run"]).unwrap_err().contains("needs a value"));
        assert_eq!(args(&["claude", "ask", "x", "--extra=--dry-run"]).unwrap().extra, "--dry-run");
    }

    #[test]
    fn values_and_defaults() {
        let a = args(&["--", "omp", "review", "--model=opus", "--base", "main", "--resume", "  "]).unwrap();
        assert_eq!((a.agent.id(), a.mode), ("omp", Mode::Review));
        assert_eq!(a.model.as_deref(), Some("opus"));
        assert_eq!((a.base.as_str(), a.head.as_str()), ("main", "HEAD"));
        assert_eq!(a.resume, None, "a blank --resume is no resume");
    }
}
