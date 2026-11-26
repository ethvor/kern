use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::{KernHome, canonicalize_lossy, load_optional_toml, save_toml};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardPolicy {
    pub project: String,
    #[serde(default)]
    pub allowed_names: Vec<String>,
    #[serde(default)]
    pub allowed_emails: Vec<String>,
    /// Case-insensitive substrings forbidden in paths or staged diffs (never echoed on hit)
    #[serde(default)]
    pub forbidden_terms: Vec<String>,
    #[serde(default = "default_true")]
    pub check_nul: bool,
}

impl Default for GuardPolicy {
    fn default() -> Self {
        Self {
            project: String::new(),
            allowed_names: Vec::new(),
            allowed_emails: Vec::new(),
            forbidden_terms: Vec::new(),
            check_nul: true,
        }
    }
}

fn default_true() -> bool {
    true
}

pub fn policy_path(home: &KernHome, project: &str) -> PathBuf {
    home.guards_dir().join(format!("{project}.toml"))
}

pub fn cmd_guard_init(home: &KernHome, project: &str, confidential: bool) -> Result<()> {
    let path = policy_path(home, project);
    if path.exists() {
        bail!("guard policy already exists at {}", path.display());
    }
    let policy = if confidential {
        GuardPolicy {
            project: project.to_string(),
            allowed_names: vec!["Local Development".to_string()],
            allowed_emails: vec!["private@localhost.invalid".to_string()],
            forbidden_terms: Vec::new(),
            check_nul: true,
        }
    } else {
        GuardPolicy {
            project: project.to_string(),
            allowed_names: Vec::new(),
            allowed_emails: Vec::new(),
            forbidden_terms: Vec::new(),
            check_nul: true,
        }
    };
    save_toml(&path, &policy)?;
    println!("Created guard policy at {}", path.display());
    println!("Edit allowed_names, allowed_emails, and forbidden_terms, then run:");
    println!("  kern guard install --dir <repo> --project {project}");
    Ok(())
}

pub fn cmd_guard_install(home: &KernHome, dir: &Path, project: &str, force: bool) -> Result<()> {
    let repo = canonicalize_lossy(dir);
    let hooks_dir = resolve_git_hooks_dir(&repo)?;
    let policy = policy_path(home, project);
    if !policy.is_file() {
        bail!(
            "guard policy not found at {} (run `kern guard init --project {project}` first)",
            policy.display()
        );
    }

    let kern_bin = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "kern".to_string());

    fs::create_dir_all(&hooks_dir)?;
    for stage in ["pre-commit", "pre-push"] {
        let hook_path = hooks_dir.join(stage);
        if hook_path.exists() {
            let existing = fs::read_to_string(&hook_path).unwrap_or_default();
            let is_kern =
                existing.contains("# kern-managed-guard") || existing.contains("kern guard check");
            if !is_kern {
                if !force {
                    bail!(
                        "refusing to overwrite existing non-kern hook at {} (re-run with --force to back it up to {stage}.kern-orig)",
                        hook_path.display()
                    );
                }
                let backup = hooks_dir.join(format!("{stage}.kern-orig"));
                fs::rename(&hook_path, &backup)?;
                println!("Backed up existing hook -> {}", backup.display());
            }
        }
        let script = format!(
            "#!/usr/bin/env sh\n# kern-managed-guard\nset -e\nKERN_BIN={}\nif [ ! -x \"$KERN_BIN\" ]; then KERN_BIN=\"kern\"; fi\nexec \"$KERN_BIN\" guard check --dir {} --project {} --stage {stage}\n",
            shell_single_quote(&kern_bin),
            shell_single_quote(&repo.display().to_string()),
            shell_single_quote(project),
        );
        fs::write(&hook_path, script)?;
        #[cfg(unix)]
        {
            let mut perms = fs::metadata(&hook_path)?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&hook_path, perms)?;
        }
    }

    println!(
        "Installed fail-closed pre-commit and pre-push guards in {}",
        hooks_dir.display()
    );
    println!("  zero tracked files written inside {}", repo.display());
    Ok(())
}

pub fn cmd_guard_uninstall(dir: &Path) -> Result<()> {
    let repo = canonicalize_lossy(dir);
    let hooks_dir = resolve_git_hooks_dir(&repo)?;
    let mut removed = 0usize;

    for stage in ["pre-commit", "pre-push"] {
        let hook_path = hooks_dir.join(stage);
        if !hook_path.exists() {
            continue;
        }
        let existing = fs::read_to_string(&hook_path).unwrap_or_default();
        let is_kern =
            existing.contains("# kern-managed-guard") || existing.contains("kern guard check");
        if !is_kern {
            println!("Skipped non-kern hook at {}", hook_path.display());
            continue;
        }
        fs::remove_file(&hook_path)?;
        removed += 1;
        let backup = hooks_dir.join(format!("{stage}.kern-orig"));
        if backup.exists() {
            fs::rename(&backup, &hook_path)?;
            println!(
                "Removed kern {stage} guard and restored {}",
                hook_path.display()
            );
        } else {
            println!("Removed kern {stage} guard from {}", hooks_dir.display());
        }
    }

    if removed == 0 {
        println!("No kern-managed guards found in {}", hooks_dir.display());
    }
    Ok(())
}

pub fn cmd_guard_check(home: &KernHome, dir: &Path, project: &str, stage: &str) -> Result<()> {
    let repo = canonicalize_lossy(dir);
    let path = policy_path(home, project);
    if !path.is_file() {
        bail!(
            "guard FAIL (fail-closed): missing policy {}",
            path.display()
        );
    }
    let policy: GuardPolicy = load_optional_toml(&path)?;
    let mut violations: Vec<String> = Vec::new();

    // 1. Check effective Git author & committer identity
    if !policy.allowed_names.is_empty() || !policy.allowed_emails.is_empty() {
        for var in ["GIT_AUTHOR_IDENT", "GIT_COMMITTER_IDENT"] {
            let ident = git_capture(&repo, &["var", var])?;
            let (name, email) = parse_git_ident(&ident);
            if !policy.allowed_names.is_empty() && !policy.allowed_names.iter().any(|a| a == &name)
            {
                violations.push(format!("{var}: disallowed author/committer name"));
            }
            if !policy.allowed_emails.is_empty()
                && !policy
                    .allowed_emails
                    .iter()
                    .any(|a| a.eq_ignore_ascii_case(&email))
            {
                violations.push(format!("{var}: disallowed author/committer email"));
            }
        }
    }

    // 2. Check staged files (pre-commit) or outgoing commits (pre-push)
    if stage == "pre-push" {
        check_outgoing_commits(&repo, &policy, &mut violations)?;
    } else {
        check_staged(&repo, &policy, &mut violations)?;
    }

    if !violations.is_empty() {
        eprintln!("kern guard FAILED ({project}, stage={stage}):");
        for v in &violations {
            eprintln!("  - {v}");
        }
        bail!("{} guard violation(s) blocked {stage}", violations.len());
    }

    println!("kern guard OK ({project}, stage={stage})");
    Ok(())
}

fn check_staged(repo: &Path, policy: &GuardPolicy, violations: &mut Vec<String>) -> Result<()> {
    let lowered_terms: Vec<(usize, String)> = policy
        .forbidden_terms
        .iter()
        .enumerate()
        .filter(|(_, t)| !t.is_empty())
        .map(|(idx, t)| (idx + 1, t.to_ascii_lowercase()))
        .collect();

    let files = git_capture(repo, &["diff", "--cached", "--name-only"])?;
    for rel in files.lines().filter(|l| !l.trim().is_empty()) {
        let base = Path::new(rel)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if policy.check_nul && base.eq_ignore_ascii_case("nul") {
            violations.push(format!("Windows NUL artifact staged: {rel}"));
        }
        if !lowered_terms.is_empty() {
            let rel_lower = rel.to_ascii_lowercase();
            for (rule_num, term) in &lowered_terms {
                if rel_lower.contains(term.as_str()) {
                    violations.push(format!(
                        "staged path matched forbidden rule #{rule_num}: {rel}"
                    ));
                }
            }
        }
    }

    if !lowered_terms.is_empty() {
        let diff = git_capture(repo, &["diff", "--cached", "--no-color", "-U0"])?;
        let mut current_file = String::new();
        for line in diff.lines() {
            if let Some(rest) = line.strip_prefix("+++ b/") {
                current_file = rest.to_string();
                continue;
            }
            if line.starts_with('+') && !line.starts_with("+++") {
                let lower = line.to_ascii_lowercase();
                for (rule_num, term) in &lowered_terms {
                    if lower.contains(term.as_str()) {
                        violations.push(format!(
                            "staged content matched forbidden rule #{rule_num} in {}",
                            if current_file.is_empty() {
                                "(diff)"
                            } else {
                                &current_file
                            }
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn check_outgoing_commits(
    repo: &Path,
    policy: &GuardPolicy,
    violations: &mut Vec<String>,
) -> Result<()> {
    let has_head = Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(repo)
        .output()
        .is_ok_and(|o| o.status.success());
    if !has_head {
        return Ok(());
    }

    let lowered_terms: Vec<(usize, String)> = policy
        .forbidden_terms
        .iter()
        .enumerate()
        .filter(|(_, t)| !t.is_empty())
        .map(|(idx, t)| (idx + 1, t.to_ascii_lowercase()))
        .collect();

    let range = "@{u}..HEAD";
    let has_upstream = Command::new("git")
        .args(["rev-parse", "--verify", "@{u}"])
        .current_dir(repo)
        .output()
        .is_ok_and(|o| o.status.success());

    let log_args = if has_upstream {
        vec!["log", range, "--format=%H|%an|%ae|%cn|%ce|%s"]
    } else {
        vec!["log", "-n", "10", "--format=%H|%an|%ae|%cn|%ce|%s"]
    };
    let out = git_capture(repo, &log_args)?;
    for line in out.lines().filter(|l| !l.trim().is_empty()) {
        let parts: Vec<&str> = line.splitn(6, '|').collect();
        if parts.len() < 6 {
            continue;
        }
        let sha: String = parts[0].chars().take(8).collect();
        let (an, ae, cn, ce, subj) = (parts[1], parts[2], parts[3], parts[4], parts[5]);
        if !policy.allowed_names.is_empty()
            && (!policy.allowed_names.iter().any(|a| a == an)
                || !policy.allowed_names.iter().any(|a| a == cn))
        {
            violations.push(format!("commit {sha}: disallowed author/committer name"));
        }
        if !policy.allowed_emails.is_empty()
            && (!policy
                .allowed_emails
                .iter()
                .any(|a| a.eq_ignore_ascii_case(ae))
                || !policy
                    .allowed_emails
                    .iter()
                    .any(|a| a.eq_ignore_ascii_case(ce)))
        {
            violations.push(format!("commit {sha}: disallowed author/committer email"));
        }
        if !lowered_terms.is_empty() {
            let lower_subj = subj.to_ascii_lowercase();
            for (rule_num, term) in &lowered_terms {
                if lower_subj.contains(term.as_str()) {
                    violations.push(format!(
                        "commit {sha}: subject matched forbidden rule #{rule_num}"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn resolve_git_hooks_dir(repo: &Path) -> Result<PathBuf> {
    let out = git_capture(repo, &["rev-parse", "--git-path", "hooks"])?;
    let p = PathBuf::from(out.trim());
    Ok(if p.is_absolute() { p } else { repo.join(p) })
}

fn parse_git_ident(ident: &str) -> (String, String) {
    let trimmed = ident.trim();
    if let Some(lt) = trimmed.find('<')
        && let Some(gt) = trimmed[lt + 1..].find('>')
    {
        let name = trimmed[..lt].trim().to_string();
        let email = trimmed[lt + 1..lt + 1 + gt].trim().to_string();
        return (name, email);
    }
    (trimmed.to_string(), String::new())
}

fn git_capture(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .with_context(|| format!("failed running `git {}`", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
