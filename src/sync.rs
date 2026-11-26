use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

use anyhow::{Context, Result, bail};

use crate::config::{GlobalConfig, KernHome};

pub fn cmd_sync(
    home: &KernHome,
    commit_msg: Option<&str>,
    pull_only: bool,
    no_fanout: bool,
    dry_run: bool,
) -> Result<()> {
    let cfg = home.load_config()?;
    let repo = home.vault_dir();
    if !repo.join(".git").exists() {
        println!("[vault] skipped (not a git repo: {})", repo.display());
        return Ok(());
    }

    let status_out = git_capture(&repo, &["status", "--porcelain"])?;
    let is_dirty = !status_out.trim().is_empty();

    if is_dirty {
        if let Some(msg) = commit_msg {
            if dry_run {
                println!("[vault] dry-run: would commit changes with message {msg:?}");
            } else {
                git_run(&repo, &["add", "-A"])?;
                git_run(&repo, &["commit", "-m", msg])?;
                println!("[vault] committed local changes");
            }
        } else if !pull_only {
            println!(
                "[vault] note: {} uncommitted file(s) (pass --commit \"msg\" to commit)",
                status_out.lines().count()
            );
        }
    }

    let has_origin = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(&repo)
        .output()
        .is_ok_and(|o| o.status.success());

    if !has_origin {
        println!("[vault] local only (no `origin` remote configured)");
        return Ok(());
    }

    let branch = git_capture(&repo, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "main".to_string());

    if dry_run {
        println!("[vault] dry-run: would pull --rebase and push origin/{branch}");
        if !pull_only && !no_fanout && !cfg.sync_peers.is_empty() {
            println!(
                "[vault] dry-run: would nudge awake peers: {}",
                cfg.sync_peers.join(", ")
            );
        }
        return Ok(());
    }

    let remote_has_branch = Command::new("git")
        .args(["ls-remote", "--exit-code", "--heads", "origin", &branch])
        .current_dir(&repo)
        .output()
        .is_ok_and(|o| o.status.success());

    if remote_has_branch {
        git_run(
            &repo,
            &["pull", "--rebase", "--autostash", "origin", &branch],
        )?;
    }

    if pull_only {
        println!("[vault] pulled origin/{branch}");
        return Ok(());
    }

    git_run(&repo, &["push", "-u", "origin", &branch])?;
    println!("[vault] synced with origin/{branch}");

    if !no_fanout && !cfg.sync_peers.is_empty() {
        fanout_pull(&cfg);
    }
    Ok(())
}

/// Fast, non-blocking pull before editing so edits always start from latest HEAD.
pub fn pull_quiet(home: &KernHome) -> Result<()> {
    let repo = home.vault_dir();
    if !repo.join(".git").exists() {
        return Ok(());
    }
    let has_origin = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(&repo)
        .output()
        .is_ok_and(|o| o.status.success());
    if !has_origin {
        return Ok(());
    }
    let branch = git_capture(&repo, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "main".to_string());
    let _ = git_run(
        &repo,
        &["pull", "--rebase", "--autostash", "origin", &branch],
    );
    Ok(())
}

/// Event-driven instant push notification: runs `kern sync --pull-only` in parallel
/// on every awake peer in `cfg.sync_peers`. Asleep/offline peers are skipped cleanly.
pub fn fanout_pull(cfg: &GlobalConfig) {
    let runner = resolve_remote_runner(cfg);
    let mut handles = Vec::with_capacity(cfg.sync_peers.len());

    for peer in cfg.sync_peers.clone() {
        let runner_parts = runner.clone();
        handles.push(thread::spawn(move || {
            let ok = nudge_peer(&runner_parts, &peer);
            (peer, ok)
        }));
    }

    let mut synced = Vec::new();
    let mut skipped = Vec::new();
    for h in handles {
        if let Ok((peer, ok)) = h.join() {
            if ok {
                synced.push(peer);
            } else {
                skipped.push(peer);
            }
        }
    }
    if !synced.is_empty() {
        println!("[fanout] pulled on awake peers: {}", synced.join(", "));
    }
    if !skipped.is_empty() {
        println!(
            "[fanout] asleep/unreachable (will pull on wake): {}",
            skipped.join(", ")
        );
    }
}

fn nudge_peer(runner: &[String], peer: &str) -> bool {
    let Some((bin, prefix)) = runner.split_first() else {
        return false;
    };
    let mut cmd = Command::new(bin);
    cmd.args(prefix);
    let remote_pull = "PATH=\"$HOME/.local/bin:$PATH\" kern sync --pull-only";
    if bin == "ssh" {
        cmd.args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=2",
            "-o",
            "StrictHostKeyChecking=accept-new",
            peer,
            remote_pull,
        ]);
    } else {
        cmd.args([peer, "--", remote_pull]);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn resolve_remote_runner(cfg: &GlobalConfig) -> Vec<String> {
    if let Some(custom) = &cfg.remote_cmd
        && !custom.trim().is_empty()
    {
        return custom.split_whitespace().map(str::to_string).collect();
    }
    vec!["ssh".to_string()]
}

fn command_exists(bin: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path_var) {
        if dir.join(bin).is_file() {
            return true;
        }
        #[cfg(windows)]
        for ext in [".exe", ".cmd", ".bat"] {
            if dir.join(format!("{bin}{ext}")).is_file() {
                return true;
            }
        }
    }
    false
}

pub fn cmd_install_wake_hook() -> Result<()> {
    let home = dirs::home_dir().context("could not resolve home directory")?;
    let kern_bin = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "kern".to_string());

    if cfg!(target_os = "macos") {
        let wakeup = home.join(".wakeup");
        let line = format!("{kern_bin} sync --pull-only >/dev/null 2>&1 &");
        let existing = fs::read_to_string(&wakeup).unwrap_or_default();
        if !existing.contains("kern sync --pull-only") {
            let mut next = if existing.is_empty() {
                "#!/usr/bin/env sh\n".to_string()
            } else {
                existing
            };
            if !next.ends_with('\n') {
                next.push('\n');
            }
            next.push_str(&line);
            next.push('\n');
            fs::write(&wakeup, next)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = fs::metadata(&wakeup)?.permissions();
                perms.set_mode(0o755);
                fs::set_permissions(&wakeup, perms)?;
            }
        }
        println!("Installed macOS wake hook in {}", wakeup.display());
        if !command_exists("sleepwatcher") {
            println!("Install & start sleepwatcher to trigger ~/.wakeup on every lid-open:");
            println!("  brew install sleepwatcher && brew services start sleepwatcher");
        } else {
            println!(
                "sleepwatcher detected (ensure `brew services start sleepwatcher` is running)."
            );
        }
    } else if cfg!(target_os = "windows") {
        println!("Windows wake-from-sleep task (run once in PowerShell):");
        println!(
            "  schtasks /Create /TN \"KernWakeSync\" /TR \"\\\"{kern_bin}\\\" sync --pull-only\" /SC ONEVENT /EC System /MO \"*[System[Provider[@Name='Microsoft-Windows-Power-Troubleshooter'] and EventID=1]]\" /F"
        );
    } else {
        println!(
            "Linux systemd-sleep wake hook (install as /usr/lib/systemd/system-sleep/kern-sync):"
        );
        println!("#!/bin/sh");
        println!("if [ \"$1\" = \"post\" ]; then");
        println!(
            "  su - {} -c '{kern_bin} sync --pull-only' >/dev/null 2>&1 &",
            std::env::var("USER")
                .or_else(|_| std::env::var("LOGNAME"))
                .unwrap_or_else(|_| "<user>".to_string())
        );
        println!("fi");
    }
    Ok(())
}

fn git_capture(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .with_context(|| format!("failed running `git {}`", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "git {} failed in {}: {}",
            args.join(" "),
            repo.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn git_run(repo: &Path, args: &[&str]) -> Result<()> {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .with_context(|| format!("failed running `git {}`", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "git {} failed in {}: {}",
            args.join(" "),
            repo.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}
