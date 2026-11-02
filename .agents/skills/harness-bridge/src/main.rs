//! harness-bridge AGENT MODE [SUBJECT...] [--resume ID | --fresh] [--model M]
//!                [--base REF --head REF] [--extra TEXT] [--cwd DIR] [--dry-run]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

use harness_bridge::bridge::{Call, session_name};
use harness_bridge::prompt::{DIFF_LIMIT, Mode, Prompt, Subject, read_subject};
use harness_bridge::{Agent, Delivery};

const USAGE: &str = "usage: harness-bridge AGENT MODE [SUBJECT...] [--resume ID | --fresh] [--model M] \
[--base REF --head REF] [--extra TEXT] [--cwd DIR] [--dry-run]
agents: claude, puku-cli, omp    modes: ask, plan, adversarial, review";

#[derive(Debug)]
struct Args {
    agent: Agent,
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
    let agent: Agent = argv.first().ok_or("unknown agent: (missing)")?.parse()?;
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
    let mut it = rest.iter();
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
        let mut value = || inline.clone().or_else(|| it.next().cloned()).ok_or(format!("--{key} needs a value"));
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
        text.push_str("\n[harness-bridge: diff truncated]");
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
            format!("{}\n\n[harness-bridge: target file content truncated]", subj.text)
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

    let new_session_id = if a.resume.is_some() { String::new() } else { uuid::Uuid::new_v4().to_string() };
    let stamp = jiff::Timestamp::now().strftime("%Y-%m-%dT%H-%M-%S").to_string();
    let name = session_name(a.agent, a.mode.as_str(), &stamp);
    let call = Call {
        agent: a.agent,
        prompt: &prompt,
        model: a.model.as_deref(),
        resume: a.resume.as_deref(),
        new_session_id: &new_session_id,
        name: &name,
    };
    let built = call.build()?;

    if a.dry_run {
        let bytes = prompt.len();
        let mut prompt_info =
            serde_json::json!({ "redacted": true, "bytes": bytes, "delivery": call.delivery().as_str() });
        prompt_info.as_object_mut().expect("object").extend(meta);
        let command: Vec<String> = std::iter::once(built.bin.to_owned())
            .chain(
                built
                    .args
                    .iter()
                    .map(|x| if *x == prompt { format!("[prompt redacted: {bytes} bytes]") } else { x.clone() }),
            )
            .collect();
        let out = serde_json::json!({
            "agent": a.agent.name(),
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
        eprintln!("harness-bridge: new session id = {new_session_id}");
    }
    let mut child = Command::new(built.bin)
        .args(&built.args)
        .current_dir(&cwd)
        .stdin(if built.envelope.is_some() { Stdio::piped() } else { Stdio::inherit() })
        .spawn()
        .map_err(|e| format!("starting {}: {e}", built.bin))?;
    if let (Some(envelope), Some(mut stdin)) = (&built.envelope, child.stdin.take()) {
        debug_assert_eq!(call.delivery(), Delivery::Stdin);
        stdin.write_all(envelope.as_bytes()).map_err(|e| format!("writing the prompt: {e}"))?;
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    Ok(status.code().and_then(|c| u8::try_from(c).ok()).unwrap_or(1))
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.is_empty() || argv.iter().any(|a| a == "--help" || a == "-h") && argv.len() <= 1 {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match parse(&argv).and_then(|a| run(&a)) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("harness-bridge: {e}");
            ExitCode::FAILURE
        }
    }
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
    }

    #[test]
    fn values_and_defaults() {
        let a = args(&["--", "omp", "review", "--model=opus", "--base", "main", "--resume", "  "]).unwrap();
        assert_eq!((a.agent, a.mode), (Agent::Omp, Mode::Review));
        assert_eq!(a.model.as_deref(), Some("opus"));
        assert_eq!((a.base.as_str(), a.head.as_str()), ("main", "HEAD"));
        assert_eq!(a.resume, None, "a blank --resume is no resume");
    }
}
