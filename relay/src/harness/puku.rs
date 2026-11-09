//! puku-cli, a Claude Code derivative: the same flags, stream-json events,
//! session ids and transcripts (`projects/<cwd key>/<session>.jsonl` in the
//! home), run by the shared [`ClaudeCode`]. What differs is here.
//!
//! - Homes are `~/.puku-cli` (`default`) and `~/.puku-cli.<label>`, selected
//!   by `PUKU_CONFIG_DIR`; each holds `puku-cli.json`, settings and transcripts.
//! - The sign-in is not in the home: `puku-cli auth login` writes
//!   `~/.config/$APP_NAME/session.json` (`APP_NAME` defaults to `pukucode`),
//!   whatever `PUKU_CONFIG_DIR` says. A labelled home is signed in under
//!   `APP_NAME=pukucode.<label>`, so each account keeps its own sign-in.
//! - Permission checks are turned off with `--god-mode`.
//! - Quota is the chat quota of `api-cli.puku.sh/puku/v1/subscription`, which
//!   puku shows only in its interactive Usage tab. A short node script reads
//!   it with the home's own access token and prints only the quota; when the
//!   token has expired it lets `puku-cli` renew it first, so relay never
//!   writes the sign-in itself. `/usage` reads Claude's oauth endpoint, which
//!   a puku sign-in does not reach, and puku's `rate_limit_event`s carry no
//!   windows.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::harness::claude::{Account, ClaudeCode, digest};
use crate::harness::{Home, Identity, Invocation, Meter, Meters, SignIn, read_json};

#[allow(non_upper_case_globals)]
pub const Puku: ClaudeCode = ClaudeCode {
    id: "puku",
    program: "puku-cli",
    prefix: ".puku-cli",
    config_env: "PUKU_CONFIG_DIR",
    identity: "You are Puku",
    skip_permissions: "--god-mode",
    config_files: &["settings.json", "settings.local.json", "PUKU.md"],
    config_dirs: &["hooks", "plugins", "skills", "commands", "agents", "output-styles", "rules", "workflows"],
    account: &PukuSession,
};

/// The sign-in directory's name under `~/.config` for the default home.
const APP: &str = "pukucode";

/// Where the usage script asks. A constant, so no environment can send the token elsewhere.
pub const USAGE_URL: &str = "https://api-cli.puku.sh/puku/v1/subscription";

/// Prints one JSON line: `{"chat": …}` (the chat quota), `{"lapsed": why}`
/// (not signed in, or the API refused the sign-in), `{"stale": why}` (the
/// token expired and puku-cli did not renew it) or `{"error": why}`. Never
/// the token: it goes only to its first argument, [`USAGE_URL`]. Its second
/// is puku-cli, run (output discarded) when the token has expired, since every
/// puku-cli start renews an expired sign-in.
const USAGE_SCRIPT: &str = r#"const fs=require("fs"),path=require("path"),os=require("os"),cp=require("child_process");
const [url,cli]=process.argv.slice(1);
const file=path.join(os.homedir(),".config",process.env.APP_NAME||"pukucode","session.json");
const say=(o)=>process.stdout.write(JSON.stringify(o)+"\n");
const read=()=>{try{return JSON.parse(fs.readFileSync(file,"utf8"))}catch{return null}};
const fresh=(s)=>typeof s.expiresAt==="number"&&Date.now()<s.expiresAt-60000;
let s=read();
if(s&&s.accessToken&&!fresh(s)){cp.spawnSync(cli,["auth","status","--json"],{stdio:"ignore",timeout:60000});s=read();}
if(!s||!s.accessToken){say({lapsed:"not signed in"});}
else if(!fresh(s)){say({stale:"the sign-in has expired and puku-cli did not renew it"});}
else{fetch(url,{headers:{Authorization:"Bearer "+s.accessToken},signal:AbortSignal.timeout(20000)})
.then(async(r)=>{if(r.status===401||r.status===403)return say({lapsed:"HTTP "+r.status});if(!r.ok)return say({error:"HTTP "+r.status});
const b=await r.json();say({chat:(b&&b.quotas&&b.quotas.chat)||null});})
.catch((e)=>say({error:String((e&&e.name)||e)}));}
"#;

/// The user's home a puku home lives in: homes are its direct children.
fn user_home(home: &Home) -> &Path {
    home.dir.parent().unwrap_or(Path::new("/"))
}

/// `pukucode` for `default`, `pukucode.<label>` for the others.
fn app_name(home: &Home) -> Option<String> {
    (home.label != "default").then(|| format!("{APP}.{}", home.label))
}

fn session_dir(home: &Home) -> PathBuf {
    user_home(home).join(".config").join(app_name(home).as_deref().unwrap_or(APP))
}

/// The last line of `text` that is a JSON object.
fn last_object(text: &str) -> Option<Value> {
    text.lines().rev().find_map(|l| serde_json::from_str::<Value>(l.trim()).ok().filter(Value::is_object))
}

/// `{"limit": 100, "used": 25, "resetsAt": "2026-11-09T05:00:00Z"}`. A window
/// with no limit says nothing about room, so it is not a meter.
fn window(w: &Value) -> Option<Meter> {
    let limit = w.get("limit").and_then(Value::as_f64).filter(|l| *l > 0.0)?;
    let used = w.get("used").and_then(Value::as_f64)?;
    let resets_at = match w.get("resetsAt") {
        Some(Value::String(t)) => t.parse::<jiff::Timestamp>().ok().map(jiff::Timestamp::as_second),
        Some(Value::Number(n)) => n.as_i64().map(|n| if n > 100_000_000_000 { n / 1000 } else { n }),
        _ => None,
    };
    Some(Meter { utilization: used / limit, resets_at })
}

/// puku's own sign-in, `~/.config/<app>/session.json`: `accessToken`,
/// `refreshToken`, `expiresAt` (unix ms), `createdAt`, `validatedAt` and
/// `user` (`id`, `email`, `name`, `picture`, `provider`). No plan is kept.
#[derive(Debug)]
pub struct PukuSession;

impl Account for PukuSession {
    fn can_refresh(&self, home: &Home) -> bool {
        read_json(&session_dir(home).join("session.json"))
            .and_then(|v| v.get("refreshToken").and_then(Value::as_str).map(|t| !t.is_empty()))
            .unwrap_or(false)
    }

    fn sign_in(&self, home: &Home) -> SignIn {
        let file = read_json(&session_dir(home).join("session.json"));
        let user = file.as_ref().and_then(|v| v.get("user"));
        let field = |k: &str| user.and_then(|u| u.get(k)).and_then(Value::as_str).filter(|s| !s.is_empty());
        let Some(id) = field("id") else { return SignIn::Out };
        // A refresh keeps `createdAt`; a new login sets it.
        let created = file.as_ref().and_then(|v| v.get("createdAt")).map(Value::to_string).unwrap_or_default();
        let stamp = digest(&[id, field("provider").unwrap_or(""), &created].join("\n"));
        SignIn::As(Identity { plan: String::new(), id: digest(id)[..6].to_owned(), stamp: stamp[..12].to_owned() })
    }

    fn lapsed(&self, text: &str) -> bool {
        last_object(text).is_some_and(|v| v.get("lapsed").is_some())
    }

    fn env(&self, home: &Home) -> Vec<(OsString, Option<OsString>)> {
        vec![("APP_NAME".into(), app_name(home).map(Into::into))]
    }

    fn sign_in_paths(&self, user_home: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(user_home.join(".config"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(APP))
            .map(|e| e.path())
            .collect()
    }

    fn sign_in_dir(&self, home: &Home) -> Option<PathBuf> {
        Some(session_dir(home))
    }

    fn quota(&self, cli: &ClaudeCode, home: &Home) -> Invocation {
        let mut i = Invocation::script("node", &["-e", USAGE_SCRIPT, USAGE_URL], cli.program);
        i.env.push((cli.config_env.into(), Some(home.dir.clone().into())));
        i.env.extend(self.env(home));
        i
    }

    /// `quotas.chat` of the subscription: `windows` `fiveHour`, `weekly` and
    /// `monthly`, or, with none, the request count `used_req` of `req_limit`.
    fn parse_quota(&self, text: &str, _now_s: i64) -> Meters {
        let mut out = Meters::new();
        let Some(chat) = last_object(text).and_then(|v| v.get("chat").cloned()) else { return out };
        for (key, name) in [("fiveHour", "five_hour"), ("weekly", "seven_day"), ("monthly", "monthly")] {
            if let Some(m) = chat.pointer(&format!("/windows/{key}")).and_then(window) {
                out.insert(name.into(), m);
            }
        }
        if out.is_empty() {
            let limit = chat.get("req_limit").and_then(Value::as_f64).filter(|l| *l > 0.0);
            if let (Some(limit), Some(used)) = (limit, chat.get("used_req").and_then(Value::as_f64)) {
                out.insert("requests".into(), Meter { utilization: used / limit, resets_at: None });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::testing::{argv, home, task};
    use crate::harness::{Harness, Permissions, Session};

    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn tmp(tag: &str) -> Dir {
        let d = std::env::temp_dir().join(format!("relay-puku-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }

    /// A session file as `puku-cli auth login` writes it, with fake tokens.
    fn session(user_home: &Path, app: &str, refresh: &str, id: &str, created: u64) {
        let d = user_home.join(".config").join(app);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("session.json"),
            format!(
                r#"{{"accessToken":"fake-access","refreshToken":"{refresh}","expiresAt":1,"createdAt":{created},"validatedAt":5,"user":{{"id":"{id}","email":"x@y.invalid","name":"X","picture":null,"provider":"github"}}}}"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn argv_is_claudes_with_puku_names() {
        let s = Path::new("/s");
        let fresh = Puku.invocation(&home("puku", "w"), &task("p", Session::New("id"), Permissions::Unrestricted, s));
        let fresh = fresh.unwrap();
        assert_eq!(fresh.program, "puku-cli");
        assert_eq!(
            argv(&fresh),
            "-p --output-format stream-json --verbose --strict-mcp-config --setting-sources project,local \
             --disable-slash-commands --god-mode --disallowedTools ScheduleWakeup,CronCreate,CronDelete,RemoteTrigger \
             --max-budget-usd 2.5 --append-system-prompt b --model m --effort high --name n --session-id id"
        );
        assert_eq!(fresh.stdin.as_deref(), Some("p"));
        assert!(fresh.env.contains(&("PUKU_CONFIG_DIR".into(), Some("/h/w".into()))));
        assert!(fresh.env.contains(&("APP_NAME".into(), Some("pukucode.w".into()))), "its own sign-in");
        let d = Puku.invocation(&home("puku", "default"), &task("p", Session::Fork("o"), Permissions::ReadOnly, s));
        let d = d.unwrap();
        assert!(d.env.contains(&("APP_NAME".into(), None)), "the default sign-in: APP_NAME unset");
        let a = argv(&d);
        assert!(a.contains("--resume o --fork-session") && a.contains("--permission-mode plan"), "{a}");
        assert!(!a.contains("--god-mode") && !a.contains("--dangerously"));
    }

    #[test]
    fn homes_are_config_dirs_whose_sign_in_can_renew() {
        let d = tmp("homes");
        for name in [".puku-cli", ".puku-cli.work", ".puku-cli.gone", ".puku-cli.short"] {
            std::fs::create_dir_all(d.0.join(name)).unwrap();
        }
        std::fs::write(d.0.join(".puku-cli.json"), "{}").unwrap();
        session(&d.0, "pukucode", "fake-refresh", "u1", 1);
        session(&d.0, "pukucode.work", "fake-refresh", "u2", 1);
        session(&d.0, "pukucode.short", "", "u3", 1);
        let homes = Puku.homes(&d.0);
        let labels: Vec<_> = homes.iter().map(|h| (h.harness, h.label.as_str())).collect();
        assert_eq!(labels, [("puku", "default"), ("puku", "work")], "no session, or no refresh token: not offered");
        let work = &homes[1];
        assert_eq!(Puku.writable(work, &d.0), [d.0.join(".puku-cli.work"), d.0.join(".config/pukucode.work")]);
        let paths = Puku.home_paths(&d.0);
        for p in [".puku-cli", ".puku-cli.gone", ".puku-cli.json", ".config/pukucode", ".config/pukucode.short"] {
            assert!(paths.contains(&d.0.join(p)), "{p} is hidden from other sessions: {paths:?}");
        }
        assert!(Puku.protected(work).contains(&work.dir.join("settings.json")));
    }

    #[test]
    fn sign_in_is_a_hash_of_the_user_id_and_never_carries_the_email() {
        let d = tmp("signin");
        std::fs::create_dir_all(d.0.join(".puku-cli")).unwrap();
        let h = Home { harness: "puku", label: "default".into(), dir: d.0.join(".puku-cli") };
        assert_eq!(Puku.sign_in(&h), SignIn::Out, "no session file");
        session(&d.0, "pukucode", "fake-refresh", "user-76de9d00", 1);
        let SignIn::As(id) = Puku.sign_in(&h) else { panic!("signed in") };
        assert_eq!((id.plan.as_str(), id.id.len()), ("", 6));
        let shown = format!("{id:?} {}", id.label());
        assert!(!shown.contains("x@y.invalid") && !shown.contains("user-76de9d00") && !shown.contains("fake"));
        session(&d.0, "pukucode", "fake-refresh", "user-76de9d00", 2);
        let SignIn::As(later) = Puku.sign_in(&h) else { panic!("signed in") };
        assert_eq!(later.id, id.id);
        assert_ne!(later.stamp, id.stamp, "a new login is a changed sign-in");
    }

    #[test]
    fn usage_is_the_chat_quota() {
        let reply = r#"{"chat":{"used_req":10,"req_limit":1000,"windows":{"fiveHour":{"limit":200,"used":50,"resetsAt":"2026-11-09T05:00:00Z"},"weekly":{"limit":1000,"used":900,"resetsAt":"2026-11-12T00:00:00.000Z"},"monthly":{"limit":0,"used":0,"resetsAt":"2026-12-01T00:00:00Z"}}}}"#;
        let m = Puku.parse_quota(&format!("noise\n{reply}\n"), 0);
        assert_eq!(m.keys().collect::<Vec<_>>(), ["five_hour", "seven_day"], "a window with no limit is no meter");
        assert!((m["five_hour"].utilization - 0.25).abs() < 1e-9 && (m["seven_day"].utilization - 0.9).abs() < 1e-9);
        assert_eq!(m["five_hour"].resets_at, Some(1_794_200_400));
        assert_eq!(Puku.meters_for(Some("puku-ai-2.8"), &m).unwrap().len(), 2, "one allowance, whatever the model");
        let counts = Puku.parse_quota(r#"{"chat":{"used_req":250,"req_limit":1000}}"#, 0);
        assert!((counts["requests"].utilization - 0.25).abs() < 1e-9 && counts["requests"].resets_at.is_none());
        for unread in [r#"{"chat":null}"#, r#"{"stale":"expired"}"#, r#"{"error":"HTTP 500"}"#, ""] {
            assert!(Puku.parse_quota(unread, 0).is_empty() && !Puku.lapsed(unread), "{unread}");
        }
        assert!(Puku.lapsed(r#"{"lapsed":"HTTP 401"}"#) && Puku.lapsed(r#"{"lapsed":"not signed in"}"#));
    }

    #[test]
    fn quota_is_read_by_a_script_handing_over_to_puku() {
        let q = Puku.quota(&home("puku", "w")).unwrap();
        assert_eq!(q.program, "node");
        assert_eq!(q.command_arg, Some(3));
        let a: Vec<_> = q.args.iter().map(|a| a.to_string_lossy()).collect();
        assert_eq!((a[0].as_ref(), a[2].as_ref(), a[3].as_ref()), ("-e", USAGE_URL, "puku-cli"));
        assert!(q.env.contains(&("APP_NAME".into(), Some("pukucode.w".into()))));
        assert!(!a[1].contains("process.env.PUKU"), "no variable can send the token elsewhere");
    }

    /// One HTTP reply on a local port; the request it answered comes back from the handle.
    fn serve(status: &str, body: &str) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/puku/v1/subscription", l.local_addr().unwrap());
        let reply = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let h = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut buf = [0u8; 8192];
            let n = c.read(&mut buf).unwrap();
            c.write_all(reply.as_bytes()).unwrap();
            String::from_utf8_lossy(&buf[..n]).into_owned()
        });
        (url, h)
    }

    /// The script under the installed node, where there is one, against a local server.
    #[test]
    fn the_usage_script_asks_with_the_homes_token_and_prints_only_the_quota() {
        if std::process::Command::new("node").arg("--version").output().is_err() {
            eprintln!("skipped: node is not installed");
            return;
        }
        let d = tmp("script");
        let run = |url: &str, cli: &Path, app: Option<&str>| {
            let mut cmd = std::process::Command::new("node");
            cmd.args(["-e", USAGE_SCRIPT, url]).arg(cli).env("HOME", &d.0).env_remove("APP_NAME");
            if let Some(a) = app {
                cmd.env("APP_NAME", a);
            }
            String::from_utf8_lossy(&cmd.output().unwrap().stdout).into_owned()
        };
        let nowhere = Path::new("/nonexistent/puku-cli");
        let none = run("http://127.0.0.1:9/", nowhere, None);
        assert!(Puku.lapsed(&none), "{none}");
        // Expired long ago, and no puku-cli to renew it: unread, not signed out. No request is made.
        session(&d.0, "pukucode", "fake-refresh", "u1", 1);
        let stale = run("http://127.0.0.1:9/", nowhere, None);
        assert!(stale.contains("stale") && !Puku.lapsed(&stale) && !stale.contains("fake"), "{stale}");
        // A puku-cli that renews the sign-in as puku does, rewriting its own file.
        let cli = d.0.join("puku-cli");
        let file = d.0.join(".config/pukucode.w/session.json");
        session(&d.0, "pukucode.w", "fake-refresh", "u2", 1);
        std::fs::write(
            &cli,
            format!(
                "#!/bin/sh\n[ \"$*\" = 'auth status --json' ] || exit 1\nsed -i 's/\"expiresAt\":1,/\"expiresAt\":99999999999999,/' {}\necho '{{\"email\":\"x@y.invalid\"}}'\n",
                file.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&cli, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let (url, asked) = serve(
            "200 OK",
            r#"{"plan":"pro","email":"x@y.invalid","quotas":{"chat":{"windows":{"weekly":{"limit":10,"used":4,"resetsAt":"2026-11-12T00:00:00Z"}}}}}"#,
        );
        let out = run(&url, &cli, Some("pukucode.w"));
        let request = asked.join().unwrap().to_ascii_lowercase();
        assert!(
            request.starts_with("get /puku/v1/subscription") && request.contains("authorization: bearer fake-access")
        );
        assert!(!out.contains("x@y.invalid") && !out.contains("fake"), "only the quota is printed: {out}");
        assert!((Puku.parse_quota(&out, 0)["seven_day"].utilization - 0.4).abs() < 1e-9, "{out}");
        // A refused sign-in is a lapsed one.
        let (url, asked) = serve("401 Unauthorized", "{}");
        let out = run(&url, &cli, Some("pukucode.w"));
        asked.join().unwrap();
        assert!(Puku.lapsed(&out), "{out}");
    }
}
