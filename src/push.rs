//! The one place the app runs the installed `git` command line, and only for
//! `git push`. Everything else uses libgit2.
use crate::flow_finish::PushSpec;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const PUSH_TIMEOUT: Duration = Duration::from_secs(120);

/// `git` from PATH, else /usr/bin/git; None unless `git --version` succeeds.
pub fn find_git() -> Option<PathBuf> {
    ["git", "/usr/bin/git"].into_iter().map(PathBuf::from).find(|p| {
        Command::new(p)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// Arguments after `git`. Atomic, never forced: the only lease flag guards the
/// remote branch delete. Branches are pushed as the commits the finish made.
/// User config (`push.followTags`, `push.recurseSubmodules`) adds nothing.
pub fn push_args(spec: &PushSpec) -> Vec<String> {
    let mut args: Vec<String> = ["push", "--atomic", "--porcelain", "--no-follow-tags", "--recurse-submodules=no"]
        .into_iter()
        .map(String::from)
        .collect();
    if let Some((branch, oid)) = &spec.delete {
        args.push(format!("--force-with-lease=refs/heads/{branch}:{oid}"));
    }
    args.push("origin".into());
    for (b, oid) in &spec.branches {
        args.push(format!("{oid}:refs/heads/{b}"));
    }
    args.push(format!("refs/tags/{}", spec.tag));
    if let Some((branch, _)) = &spec.delete {
        args.push(format!(":refs/heads/{branch}"));
    }
    args
}

/// Ok when origin accepted everything; Err carries Git's reason for the user.
pub fn push(git: &Path, workdir: &Path, spec: &PushSpec) -> std::result::Result<(), String> {
    let mut child = Command::new(git)
        .arg("-C")
        .arg(workdir)
        .args(["-c", "advice.pushUpdateRejected=false"])
        .args(push_args(spec))
        .env("GIT_TERMINAL_PROMPT", "0")
        // `failure_reason` matches Git's English output.
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not run git: {e}"))?;
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let deadline = Instant::now() + PUSH_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Push timed out after 120 seconds.".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Could not wait for git: {e}"));
            }
        }
    };
    if status.success() {
        return Ok(());
    }
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    Err(failure_reason(&stdout, &stderr))
}

/// Read a pipe on its own thread so a full pipe never blocks git.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    })
}

/// The rejected references from `--porcelain` stdout; only when there are none,
/// the tail of stderr without the generic `hint:` lines.
fn failure_reason(stdout: &str, stderr: &str) -> String {
    let rejected: Vec<(String, bool)> = stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let (flag, refs, summary) = (parts.next()?, parts.next()?, parts.next()?);
            if flag != "!" {
                return None;
            }
            let to = refs.split_once(':').map_or(refs, |(_, to)| to);
            let to = to
                .strip_prefix("refs/heads/")
                .or_else(|| to.strip_prefix("refs/tags/"))
                .unwrap_or(to);
            Some((format!("{to} {}", summary.trim()), summary.contains("(atomic push failed)")))
        })
        .collect();
    let mut shown: Vec<&str> = rejected.iter().filter(|(_, atomic)| !atomic).map(|(s, _)| s.as_str()).collect();
    if shown.is_empty() {
        shown = rejected.iter().map(|(s, _)| s.as_str()).collect();
    }
    let reason = if shown.is_empty() {
        let lines: Vec<&str> = stderr
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("hint:"))
            .collect();
        lines[lines.len().saturating_sub(3)..].join(" ")
    } else {
        shown.join("; ")
    };
    if reason.is_empty() {
        "git push failed.".into()
    } else {
        reason
    }
}
