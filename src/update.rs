use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::{GlobalConfig, KernHome, expand_tilde, save_toml};

pub const CURRENT_SHA: &str = env!("KERN_GIT_SHA");
pub const DEFAULT_UPDATE_REPO: &str = "https://github.com/ethvor/kern.git";
const CHECK_TTL_SECS: u64 = 6 * 3600;
const LOCK_STALE_SECS: u64 = 600;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCache {
    pub checked_at: u64,
    pub remote_sha: String,
    pub remote_short: String,
}

#[derive(Debug, Default)]
pub struct UpdateOptions {
    pub check_only: bool,
    pub status: bool,
    pub self_only: bool,
    pub push_only: bool,
    pub no_push: bool,
    pub quiet: bool,
    pub policy: Option<String>,
    pub target_add: Option<String>,
    pub target_rm: Option<String>,
    pub targets: Vec<String>,
}

/// Fast, non-blocking update check called during `kern bootstrap`.
/// Reads cached `~/.kern/update-check.json` (~0.1 ms), refreshes asynchronously if stale,
/// and enforces `update_policy` (`unset` | `prompt` | `auto` | `off`).
pub fn bootstrap_update_notice(home: &KernHome, cfg: &GlobalConfig) -> Option<String> {
    let policy = cfg.effective_update_policy();
    if policy == "off" || CURRENT_SHA == "unknown" {
        return None;
    }

    let now = unix_now();
    let cache = load_cache(home);
    let cache_stale = cache
        .as_ref()
        .is_none_or(|c| now.saturating_sub(c.checked_at) >= CHECK_TTL_SECS);
    if cache_stale {
        spawn_detached(&["update", "--check-only", "--quiet"]);
    }

    let cached = cache?;
    if cached.remote_short.is_empty() || cached.remote_short == CURRENT_SHA {
        return None;
    }

    match policy {
        "auto" => {
            if !lock_is_active(home) {
                spawn_detached(&["update", "--self-only", "--quiet"]);
            }
            Some(format!(
                "> **kern update:** Newer commit `{}` detected (current `{CURRENT_SHA}`). Auto-updating in background (`update_policy = \"auto\"`).",
                cached.remote_short
            ))
        }
        "prompt" => Some(format!(
            "> **kern update available:** `{CURRENT_SHA}` → `{}` (`update_policy = \"prompt\"`).\n\
             > Tell the user a `kern` update is available and offer to run `kern update` (or switch policy with `kern update --policy auto|off`).",
            cached.remote_short
        )),
        _ => Some(format!(
            "> **kern update available:** `{CURRENT_SHA}` → `{}` (`update_policy` is not set yet).\n\
             > Ask the user:\n\
             > 1. Would you like me to run `kern update` now?\n\
             > 2. Going forward, should `kern` **auto-update** (`kern update --policy auto`) or **tell you every time** (`kern update --policy prompt`)? (Or `kern update --policy off` to disable checks.)",
            cached.remote_short
        )),
    }
}

pub fn doctor_update_notice(home: &KernHome, cfg: &GlobalConfig) -> Option<String> {
    if cfg.effective_update_policy() == "off" || CURRENT_SHA == "unknown" {
        return None;
    }
    let cached = load_cache(home)?;
    if !cached.remote_short.is_empty() && cached.remote_short != CURRENT_SHA {
        return Some(format!(
            "kern update available: {CURRENT_SHA} -> {} (policy={}; run `kern update`)",
            cached.remote_short,
            cfg.effective_update_policy()
        ));
    }
    None
}

pub fn cmd_update(home: &KernHome, opts: UpdateOptions) -> Result<()> {
    let mut cfg = home.load_config()?;

    if let Some(pol) = opts.policy {
        let p = pol.trim().to_ascii_lowercase();
        match p.as_str() {
            "auto" | "prompt" | "off" => cfg.update_policy = Some(p.clone()),
            "unset" => cfg.update_policy = None,
            other => bail!("invalid --policy '{other}' (expected: auto, prompt, off, or unset)"),
        }
        save_toml(&home.config_path(), &cfg)?;
        println!(
            "Saved update_policy = {:?} in {}",
            cfg.effective_update_policy(),
            home.config_path().display()
        );
        return Ok(());
    }

    if let Some(dev) = opts.target_add {
        let dev = dev.trim().to_string();
        if dev.is_empty() {
            bail!("--target-add cannot be empty");
        }
        if !cfg.update_targets.iter().any(|t| t == &dev) {
            cfg.update_targets.push(dev.clone());
            save_toml(&home.config_path(), &cfg)?;
        }
        println!("update_targets: {}", cfg.update_targets.join(", "));
        return Ok(());
    }

    if let Some(dev) = opts.target_rm {
        let dev = dev.trim();
        cfg.update_targets.retain(|t| t != dev);
        save_toml(&home.config_path(), &cfg)?;
        println!(
            "update_targets: {}",
            if cfg.update_targets.is_empty() {
                "(none)".to_string()
            } else {
                cfg.update_targets.join(", ")
            }
        );
        return Ok(());
    }

    if opts.status {
        let cache = load_cache(home);
        println!("kern update status");
        println!(
            "  version:        {} ({CURRENT_SHA})",
            env!("CARGO_PKG_VERSION")
        );
        println!("  policy:         {}", cfg.effective_update_policy());
        println!("  repo:           {}", cfg.effective_update_repo());
        println!(
            "  update_targets: {}",
            if cfg.update_targets.is_empty() {
                "(none)".to_string()
            } else {
                cfg.update_targets.join(", ")
            }
        );
        match cache {
            Some(c) => {
                let age = unix_now().saturating_sub(c.checked_at);
                let state = if c.remote_short == CURRENT_SHA {
                    "up to date"
                } else {
                    "update available"
                };
                println!(
                    "  remote_sha:     {} ({state}, checked {age}s ago)",
                    c.remote_short
                );
            }
            None => println!("  remote_sha:     (not checked yet; run `kern update --check-only`)"),
        }
        return Ok(());
    }

    if opts.check_only {
        let cache = refresh_remote_check(home, &cfg)?;
        if !opts.quiet {
            if cache.remote_short == CURRENT_SHA {
                println!("kern is up to date ({CURRENT_SHA})");
            } else {
                println!(
                    "kern update available: {CURRENT_SHA} -> {} (run `kern update`)",
                    cache.remote_short
                );
            }
        }
        return Ok(());
    }

    let bin_dir = resolve_bin_dir()?;
    let mut installed_sha = CURRENT_SHA.to_string();

    if !opts.push_only {
        let _lock = acquire_lock(home)?;
        let src_dir = prepare_source_repo(home, &cfg, opts.quiet)?;
        if !opts.quiet {
            println!("==> Building kern from {}...", src_dir.display());
        }
        let mut build = Command::new("cargo");
        build.args(["build", "--release"]).current_dir(&src_dir);
        if opts.quiet {
            build.stdout(Stdio::null()).stderr(Stdio::null());
        }
        let status = build
            .status()
            .context("failed running `cargo build --release`")?;
        if !status.success() {
            bail!("cargo build failed in {}", src_dir.display());
        }

        let target_dir = src_dir.join("target").join("release");
        fs::create_dir_all(&bin_dir)?;
        let src = target_dir.join(bin_filename("kern"));
        if !src.is_file() {
            bail!("expected built binary missing: {}", src.display());
        }
        atomic_install(&src, &bin_dir.join(bin_filename("kern")))?;

        if let Some(sha) = git_head_short(&src_dir) {
            installed_sha = sha.clone();
            let full = git_head_full(&src_dir).unwrap_or_else(|| sha.clone());
            save_cache(
                home,
                &UpdateCache {
                    checked_at: unix_now(),
                    remote_sha: full,
                    remote_short: sha,
                },
            )?;
        }
        if !opts.quiet {
            let ver = Command::new(bin_dir.join(bin_filename("kern")))
                .arg("--version")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_else(|| installed_sha.clone());
            println!(
                "[ok] updated {} ({ver})",
                bin_dir.join(bin_filename("kern")).display()
            );
        }
    }

    if !opts.self_only && !opts.no_push {
        let targets = if !opts.targets.is_empty() {
            opts.targets
        } else {
            cfg.update_targets.clone()
        };
        if !targets.is_empty() {
            push_to_targets(&cfg, &targets, &bin_dir, &installed_sha, opts.quiet);
        }
    }

    Ok(())
}

fn push_to_targets(
    cfg: &GlobalConfig,
    targets: &[String],
    bin_dir: &Path,
    local_sha: &str,
    quiet: bool,
) {
    let local_os = local_uname("-s").unwrap_or_default();
    let local_arch = local_uname("-m").unwrap_or_default();
    let runner = resolve_remote_runner(cfg);
    let copy_runner = resolve_remote_copy_runner(cfg);

    let mut handles = Vec::with_capacity(targets.len());
    for peer in targets.iter().cloned() {
        let bin_dir = bin_dir.to_path_buf();
        let local_os = local_os.clone();
        let local_arch = local_arch.clone();
        let sha = local_sha.to_string();
        let runner = runner.clone();
        let copy_runner = copy_runner.clone();
        handles.push(thread::spawn(move || {
            let outcome = push_one_target(
                &peer,
                &bin_dir,
                &local_os,
                &local_arch,
                &sha,
                &runner,
                copy_runner.as_deref(),
            );
            (peer, outcome)
        }));
    }

    for h in handles {
        if let Ok((peer, res)) = h.join()
            && !quiet
        {
            match res {
                Ok(msg) => println!("[update:{peer}] {msg}"),
                Err(err) => println!("[update:{peer}] skipped ({err:#})"),
            }
        }
    }
}

fn push_one_target(
    peer: &str,
    bin_dir: &Path,
    local_os: &str,
    local_arch: &str,
    sha: &str,
    runner: &[String],
    copy_runner: Option<&[String]>,
) -> Result<String> {
    let probe = run_remote_capture(peer, "uname -s && uname -m", runner)?;
    let mut lines = probe.lines();
    let remote_os = lines.next().unwrap_or("").trim();
    let remote_arch = lines.next().unwrap_or("").trim();
    if remote_os.is_empty() || remote_arch.is_empty() {
        bail!("asleep or unreachable");
    }

    if remote_os == local_os && remote_arch == local_arch {
        run_remote_capture(
            peer,
            "mkdir -p \"$HOME/.local/bin\" \"$HOME/.kern\"",
            runner,
        )?;
        let local_bin = bin_dir.join(bin_filename("kern"));
        if !local_bin.is_file() {
            bail!("local binary missing: {}", local_bin.display());
        }
        copy_remote(&local_bin, peer, ".local/bin/kern.new", copy_runner)?;
        let safe_sha: String = sha
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        let now = unix_now();
        let cache_json = format!(
            "{{\"checked_at\":{now},\"remote_sha\":\"{safe_sha}\",\"remote_short\":\"{safe_sha}\"}}"
        );
        let finalize = format!(
            "chmod 755 \"$HOME/.local/bin/kern.new\" && mv -f \"$HOME/.local/bin/kern.new\" \"$HOME/.local/bin/kern\" && \
             printf '%s\\n' '{cache_json}' > \"$HOME/.kern/update-check.json\" && \
             \"$HOME/.local/bin/kern\" --version"
        );
        let ver = run_remote_capture(peer, &finalize, runner)?;
        return Ok(format!("pushed binary ({})", ver.trim()));
    }

    // Different OS/arch: notify peer to run its own self-update
    let out = run_remote_capture(
        peer,
        "PATH=\"$HOME/.local/bin:$PATH\" kern update --self-only",
        runner,
    )?;
    let last = out.lines().last().unwrap_or("updated").trim();
    Ok(format!(
        "notified remote self-update ({remote_os}/{remote_arch}: {last})"
    ))
}

fn resolve_remote_runner(cfg: &GlobalConfig) -> Vec<String> {
    if let Some(custom) = &cfg.remote_cmd
        && !custom.trim().is_empty()
    {
        return custom.split_whitespace().map(str::to_string).collect();
    }
    vec!["ssh".to_string()]
}

fn resolve_remote_copy_runner(cfg: &GlobalConfig) -> Option<Vec<String>> {
    let custom = cfg.remote_copy_cmd.as_deref()?.trim();
    if custom.is_empty() {
        return None;
    }
    Some(custom.split_whitespace().map(str::to_string).collect())
}

fn run_remote_capture(peer: &str, remote_cmd: &str, runner: &[String]) -> Result<String> {
    let Some((bin, prefix)) = runner.split_first() else {
        bail!("empty remote_cmd");
    };
    let mut cmd = Command::new(bin);
    cmd.args(prefix);
    if bin == "ssh" {
        cmd.args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=4",
            "-o",
            "StrictHostKeyChecking=accept-new",
            peer,
            remote_cmd,
        ]);
    } else {
        cmd.args([peer, "--", remote_cmd]);
    }
    let out = cmd
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("failed invoking remote command on {peer}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        bail!(
            "{}",
            if err.is_empty() {
                "unreachable".to_string()
            } else {
                err
            }
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn copy_remote(
    local: &Path,
    peer: &str,
    remote_rel: &str,
    copy_runner: Option<&[String]>,
) -> Result<()> {
    let dst = format!("{peer}:{remote_rel}");
    let mut cmd = if let Some(runner) = copy_runner
        && let Some((bin, prefix)) = runner.split_first()
    {
        let mut c = Command::new(bin);
        c.args(prefix).arg(local).arg(&dst);
        c
    } else {
        let mut c = Command::new("rsync");
        c.args([
            "-az",
            "-e",
            "ssh -o BatchMode=yes -o ConnectTimeout=5 -o StrictHostKeyChecking=accept-new",
        ])
        .arg(local)
        .arg(&dst);
        c
    };
    let out = cmd
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("failed copying {} to {dst}", local.display()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        bail!("copy failed: {err}");
    }
    Ok(())
}

fn prepare_source_repo(home: &KernHome, cfg: &GlobalConfig, quiet: bool) -> Result<PathBuf> {
    let repo_url = cfg.effective_update_repo();

    if let Some(custom) = &cfg.update_source_dir {
        let p = expand_tilde(custom);
        if p.join("Cargo.toml").is_file() && p.join(".git").exists() && git_is_clean(&p) {
            git_pull_ff(&p, quiet)?;
            return Ok(p);
        }
    }

    let managed = home.root.join("src").join("kern");
    if managed.join(".git").exists() {
        git_pull_ff(&managed, quiet)?;
        return Ok(managed);
    }
    if let Some(parent) = managed.parent() {
        fs::create_dir_all(parent)?;
    }
    if !quiet {
        println!("==> Cloning {repo_url} into {}...", managed.display());
    }
    let mut cmd = Command::new("git");
    cmd.args(["clone", "--depth", "1", repo_url])
        .arg(&managed)
        .env(
            "GIT_SSH_COMMAND",
            "ssh -o BatchMode=yes -o ConnectTimeout=6 -o StrictHostKeyChecking=accept-new",
        );
    if quiet {
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
    }
    let status = cmd.status().context("failed running `git clone`")?;
    if !status.success() {
        bail!("git clone failed for {repo_url}");
    }
    Ok(managed)
}

fn git_pull_ff(repo: &Path, quiet: bool) -> Result<()> {
    let mut cmd = Command::new("git");
    cmd.args(["pull", "--ff-only", "origin", "main"])
        .current_dir(repo)
        .env(
            "GIT_SSH_COMMAND",
            "ssh -o BatchMode=yes -o ConnectTimeout=6 -o StrictHostKeyChecking=accept-new",
        );
    if quiet {
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
    }
    let status = cmd
        .status()
        .context("failed running `git pull --ff-only`")?;
    if !status.success() {
        bail!("git pull --ff-only failed in {}", repo.display());
    }
    Ok(())
}

fn git_is_clean(repo: &Path) -> bool {
    Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo)
        .output()
        .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim().is_empty())
}

fn git_head_short(repo: &Path) -> Option<String> {
    Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .current_dir(repo)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn git_head_full(repo: &Path) -> Option<String> {
    Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn refresh_remote_check(home: &KernHome, cfg: &GlobalConfig) -> Result<UpdateCache> {
    let repo = cfg.effective_update_repo();
    let out = Command::new("git")
        .args(["ls-remote", repo, "HEAD"])
        .env(
            "GIT_SSH_COMMAND",
            "ssh -o BatchMode=yes -o ConnectTimeout=5 -o StrictHostKeyChecking=accept-new",
        )
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("failed running `git ls-remote {repo} HEAD`"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        bail!("git ls-remote failed: {err}");
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let full = stdout
        .split_whitespace()
        .next()
        .filter(|s| s.len() >= 7)
        .context("empty output from git ls-remote")?
        .to_string();
    let short = full.chars().take(7).collect();
    let cache = UpdateCache {
        checked_at: unix_now(),
        remote_sha: full,
        remote_short: short,
    };
    save_cache(home, &cache)?;
    Ok(cache)
}

fn cache_path(home: &KernHome) -> PathBuf {
    home.root.join("update-check.json")
}

fn load_cache(home: &KernHome) -> Option<UpdateCache> {
    let raw = fs::read_to_string(cache_path(home)).ok()?;
    serde_json::from_str(&raw).ok()
}

fn save_cache(home: &KernHome, cache: &UpdateCache) -> Result<()> {
    fs::create_dir_all(&home.root)?;
    let tmp = home.root.join("update-check.json.tmp");
    fs::write(&tmp, format!("{}\n", serde_json::to_string_pretty(cache)?))?;
    fs::rename(&tmp, cache_path(home))?;
    Ok(())
}

struct LockGuard(PathBuf);
impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn lock_path(home: &KernHome) -> PathBuf {
    home.root.join(".update.lock")
}

fn lock_is_active(home: &KernHome) -> bool {
    let p = lock_path(home);
    let Ok(meta) = fs::metadata(&p) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|d| d < Duration::from_secs(LOCK_STALE_SECS))
}

fn acquire_lock(home: &KernHome) -> Result<LockGuard> {
    if lock_is_active(home) {
        bail!(
            "another kern update is already in progress ({})",
            lock_path(home).display()
        );
    }
    fs::create_dir_all(&home.root)?;
    let p = lock_path(home);
    fs::write(&p, format!("{}\n", std::process::id()))?;
    Ok(LockGuard(p))
}

fn spawn_detached(args: &[&str]) {
    let exe = env::current_exe().unwrap_or_else(|_| PathBuf::from("kern"));
    let _ = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn resolve_bin_dir() -> Result<PathBuf> {
    if let Ok(exe) = env::current_exe()
        && let Some(parent) = exe.parent()
    {
        return Ok(parent.to_path_buf());
    }
    Ok(dirs::home_dir()
        .context("could not resolve home directory")?
        .join(".local")
        .join("bin"))
}

fn bin_filename(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

fn atomic_install(src: &Path, dst: &Path) -> Result<()> {
    let mut tmp_os = dst.as_os_str().to_os_string();
    tmp_os.push(".new");
    let tmp = PathBuf::from(tmp_os);
    fs::copy(src, &tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&tmp)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&tmp, perms)?;
    }
    #[cfg(windows)]
    {
        let mut old_os = dst.as_os_str().to_os_string();
        old_os.push(".old");
        let old = PathBuf::from(old_os);
        let _ = fs::remove_file(&old);
        if dst.exists() {
            fs::rename(dst, &old)?;
        }
    }
    fs::rename(&tmp, dst)?;
    Ok(())
}

fn local_uname(flag: &str) -> Option<String> {
    Command::new("uname")
        .arg(flag)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
