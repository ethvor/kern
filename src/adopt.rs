use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::builtin::{
    DEFAULT_MANAGED_BOOTSTRAP, HARNESS_SHIM, LOCAL_END, LOCAL_START, MANAGED_END, MANAGED_START,
    ROOT_HARNESS_SHIM, starter_profile,
};
use crate::config::{
    DEFAULT_MAX_PAGE_BYTES, DEFAULT_MODE, GlobalConfig, KernHome, ProjectManifest, StealthBinding,
    canonicalize_lossy, default_harness_files, expand_tilde, save_toml,
};
use crate::doc::{Corpus, Scope, Tier, count_by_scope, resolve_doc, split_frontmatter};
use crate::sync::{cmd_sync, pull_quiet};

pub struct AddOptions<'a> {
    pub file: &'a Path,
    pub scope: Scope,
    pub target_owner: Option<&'a str>,
    pub rename: Option<&'a str>,
    pub tier: Option<Tier>,
    pub trigger: Option<&'a str>,
    pub auto_sync: bool,
}

pub fn cmd_init(
    home: &KernHome,
    user: Option<&str>,
    remote: Option<&str>,
    norm: Option<&str>,
    force: bool,
) -> Result<()> {
    let detected = user
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            ["USER", "USERNAME", "LOGNAME"]
                .iter()
                .find_map(|k| env::var(k).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .context("could not detect username from environment; pass --user <name>")?;
    let user = detected.as_str();
    let norm_name = norm
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(user);

    if home.config_path().exists() && !force {
        bail!(
            "{} already exists (pass --force to reinitialize config without deleting vault docs)",
            home.config_path().display()
        );
    }

    let vault = home.vault_dir();
    let dirs_to_create = [
        vault.join("core"),
        vault.join("modes").join(DEFAULT_MODE),
        vault.join("norms").join(norm_name),
        vault.join("users").join(user),
        vault.join("projects"),
    ];
    for d in &dirs_to_create {
        fs::create_dir_all(d)?;
    }

    let cfg = GlobalConfig {
        user: user.to_string(),
        default_mode: DEFAULT_MODE.to_string(),
        default_norm: Some(norm_name.to_string()),
        max_page_bytes: DEFAULT_MAX_PAGE_BYTES,
        sync_peers: Vec::new(),
        remote_cmd: None,
        remote_copy_cmd: None,
        managed_vaults: BTreeMap::new(),
        update_policy: None,
        update_repo: None,
        update_targets: Vec::new(),
        update_source_dir: None,
        project_roots: Vec::new(),
    };
    save_toml(&home.config_path(), &cfg)?;

    let profile_path = vault.join("users").join(user).join("PROFILE.md");
    if !profile_path.exists() {
        fs::write(&profile_path, starter_profile(user))?;
    }

    if !vault.join(".git").exists() {
        let status = Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&vault)
            .status()
            .context("failed running `git init` in vault")?;
        if !status.success() {
            bail!("`git init` failed in {}", vault.display());
        }
        let gitignore = vault.join(".gitignore");
        if !gitignore.exists() {
            fs::write(&gitignore, ".DS_Store\n*.swp\n")?;
        }
    }

    if let Some(url) = remote {
        let has_origin = Command::new("git")
            .args(["remote", "get-url", "origin"])
            .current_dir(&vault)
            .output()
            .is_ok_and(|o| o.status.success());
        let args = if has_origin {
            vec!["remote", "set-url", "origin", url]
        } else {
            vec!["remote", "add", "origin", url]
        };
        let status = Command::new("git")
            .args(&args)
            .current_dir(&vault)
            .status()?;
        if !status.success() {
            bail!("failed setting git remote origin to {url}");
        }
    }

    println!("Initialized kern at {}", home.root.display());
    println!("  user:  {user}");
    println!("  norm:  {norm_name}");
    println!("  vault: {}", vault.display());
    if let Some(url) = remote {
        println!("  origin: {url}");
    }
    println!("\nNext: run `kern bootstrap` or `cd <repo> && kern adopt --project <name>`");
    Ok(())
}

pub fn cmd_adopt(
    home: &KernHome,
    dir: &Path,
    project: Option<&str>,
    mode: Option<&str>,
    norm: Option<&str>,
    dry_run: bool,
) -> Result<()> {
    let cfg = home.load_config()?;
    let root = canonicalize_lossy(dir);
    let project_name = project
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            root.file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)
        })
        .context("could not infer project name from directory; pass --project")?;

    let manifest = ProjectManifest {
        project: project_name.clone(),
        mode: mode.map(str::to_string),
        norm: norm.map(str::to_string),
        harness_files: default_harness_files(),
        servers: Vec::new(),
    };

    let use_root_layout = !root.join(".kern").join("kern.toml").is_file()
        && (root.join("kern.toml").is_file() || root.join("BOOTSTRAP.md").is_file());
    let (manifest_path, bootstrap_path, shim, manifest_label, bootstrap_label) = if use_root_layout
    {
        (
            root.join("kern.toml"),
            root.join("BOOTSTRAP.md"),
            ROOT_HARNESS_SHIM,
            "kern.toml",
            "BOOTSTRAP.md",
        )
    } else {
        (
            root.join(".kern").join("kern.toml"),
            root.join(".kern").join("BOOTSTRAP.md"),
            HARNESS_SHIM,
            ".kern/kern.toml",
            ".kern/BOOTSTRAP.md",
        )
    };

    let preserved_local = if bootstrap_path.is_file() {
        extract_local_region(&fs::read_to_string(&bootstrap_path)?)
    } else {
        String::new()
    };
    let bootstrap_body = render_bootstrap_file(&preserved_local);

    if dry_run {
        println!(
            "[dry-run] Would adopt {} as project `{project_name}`:",
            root.display()
        );
        println!("  - write {manifest_label}");
        println!(
            "  - write {bootstrap_label} (preserving {} bytes of local section)",
            preserved_local.len()
        );
        for hf in &manifest.harness_files {
            println!("  - write {hf} ({}-byte shim)", shim.len());
        }
        return Ok(());
    }

    save_toml(&manifest_path, &manifest)?;
    if let Some(parent) = bootstrap_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&bootstrap_path, bootstrap_body)?;
    for hf in &manifest.harness_files {
        fs::write(root.join(hf), shim)?;
    }

    let proj_dir = home.vault_dir().join("projects").join(&project_name);
    fs::create_dir_all(&proj_dir)?;

    let eff_mode = manifest.mode.as_deref().unwrap_or(&cfg.default_mode);
    let eff_norm = manifest
        .norm
        .as_deref()
        .unwrap_or(cfg.effective_default_norm());

    println!(
        "Adopted {} as `{project_name}` (mode={eff_mode}, norm={eff_norm})",
        root.display()
    );
    println!(
        "  wrote: {manifest_label}, {bootstrap_label}, {}",
        manifest.harness_files.join(", ")
    );
    println!("  project docs: {}", proj_dir.display());
    Ok(())
}

pub fn cmd_bind(
    home: &KernHome,
    dir: &Path,
    project: Option<&str>,
    mode: Option<&str>,
    norm: Option<&str>,
    unbind: bool,
) -> Result<()> {
    let root = canonicalize_lossy(dir);
    let root_str = root.display().to_string();
    let mut bindings = home.load_bindings()?;

    if unbind {
        let before = bindings.binding.len();
        bindings
            .binding
            .retain(|b| canonicalize_lossy(&expand_tilde(&b.path)) != root);
        home.save_bindings(&bindings)?;
        if bindings.binding.len() < before {
            println!("Unbound stealth workspace {root_str}");
        } else {
            println!("No stealth binding existed for {root_str}");
        }
        return Ok(());
    }

    let project_name = project
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .context("--project is required when binding a stealth workspace")?
        .to_string();

    bindings
        .binding
        .retain(|b| canonicalize_lossy(&expand_tilde(&b.path)) != root);
    bindings.binding.push(StealthBinding {
        path: root_str.clone(),
        project: project_name.clone(),
        mode: mode.map(str::to_string),
        norm: norm.map(str::to_string),
    });
    home.save_bindings(&bindings)?;

    let proj_dir = home.vault_dir().join("projects").join(&project_name);
    fs::create_dir_all(&proj_dir)?;

    println!("Bound stealth workspace {root_str} -> project `{project_name}`");
    println!("  zero files written inside {root_str}");
    println!("  private project docs live at {}", proj_dir.display());
    Ok(())
}

pub fn cmd_add(home: &KernHome, opts: AddOptions<'_>) -> Result<()> {
    let cfg = home.load_config()?;
    let raw = fs::read_to_string(opts.file)
        .with_context(|| format!("failed reading {}", opts.file.display()))?;

    let dest_dir = scope_dir(home, &cfg, opts.scope, opts.target_owner)?;
    fs::create_dir_all(&dest_dir)?;

    let (parsed_fm, _) = split_frontmatter(&raw.replace("\r\n", "\n"))?;
    let file_name = match opts.rename {
        Some(n) => ensure_md(n),
        None => {
            if let Some(fm_name) = parsed_fm
                .name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                ensure_md(fm_name)
            } else {
                opts.file
                    .file_name()
                    .and_then(|n| n.to_str())
                    .context("invalid source filename")?
                    .to_string()
            }
        }
    };

    let content = if opts.tier.is_some() || opts.trigger.is_some() {
        rewrite_frontmatter(&raw, opts.tier, opts.trigger)?
    } else {
        raw
    };

    let dest = dest_dir.join(&file_name);
    fs::write(&dest, content)?;
    println!("Added {} -> {}", opts.file.display(), dest.display());

    if opts.auto_sync {
        cmd_sync(
            home,
            Some(&format!("docs: add {file_name}")),
            false,
            false,
            false,
        )?;
    }
    Ok(())
}

pub fn cmd_share(
    home: &KernHome,
    corpus: &Corpus,
    doc_query: &str,
    out_path: Option<&Path>,
    to_vault: Option<&str>,
    scope: Scope,
) -> Result<()> {
    let doc = resolve_doc(&corpus.docs, doc_query)?;
    let mut header = format!("---\nname: {:?}\ntier: {}\n", doc.name, doc.tier.label());
    if let Some(trig) = &doc.trigger {
        header.push_str(&format!("trigger: {:?}\n", trig));
    }
    if !doc.paths.is_empty() {
        let joined = doc
            .paths
            .iter()
            .map(|p| format!("{p:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        header.push_str(&format!("paths: [{joined}]\n"));
    }
    if !doc.harnesses.is_empty() {
        let joined = doc
            .harnesses
            .iter()
            .map(|h| format!("{h:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        header.push_str(&format!("harnesses: [{joined}]\n"));
    }
    if !doc.providers.is_empty() {
        let joined = doc
            .providers
            .iter()
            .map(|p| format!("{p:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        header.push_str(&format!("providers: [{joined}]\n"));
    }
    header.push_str("---\n\n");
    let full = format!("{header}{}\n", doc.body);

    if let Some(target_name) = to_vault {
        let target_home = home.with_managed_vault(target_name)?;
        let target_cfg = target_home.load_config()?;
        let dest_dir = scope_dir(&target_home, &target_cfg, scope, None)?;
        fs::create_dir_all(&dest_dir)?;
        let dest = dest_dir.join(ensure_md(&doc.name));
        fs::write(&dest, &full)?;
        println!(
            "Shared `{}` -> managed vault `{target_name}` ({})",
            doc.name,
            dest.display()
        );
        cmd_sync(
            &target_home,
            Some(&format!("docs: share {}", doc.name)),
            false,
            false,
            false,
        )?;
        return Ok(());
    }

    match out_path {
        Some(p) => {
            if let Some(parent) = p.parent()
                && !parent.as_os_str().is_empty()
            {
                fs::create_dir_all(parent)?;
            }
            fs::write(p, full)?;
            println!(
                "Exported `{}` to {} (recipient can add with `kern add {} --scope norm`)",
                doc.name,
                p.display(),
                p.display()
            );
        }
        None => print!("{full}"),
    }
    Ok(())
}

/// Pulls latest before opening `$EDITOR`, then auto-commits, pushes, and fans out on save.
pub fn cmd_edit(home: &KernHome, corpus: &Corpus, doc_query: &str, no_sync: bool) -> Result<()> {
    if !no_sync {
        pull_quiet(home)?;
    }
    let doc = resolve_doc(&corpus.docs, doc_query)?;
    if doc.source_path.starts_with("<builtin:") {
        bail!(
            "`{}` is a built-in doc; create `{}` to override it",
            doc.name,
            home.vault_dir()
                .join("core")
                .join(ensure_md(&doc.name))
                .display()
        );
    }
    let path = PathBuf::from(&doc.source_path);
    let before = fs::read(&path).with_context(|| format!("failed reading {}", path.display()))?;

    let editor = env::var("VISUAL")
        .or_else(|_| env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());
    let mut parts = editor.split_whitespace();
    let bin = parts.next().unwrap_or("vi");
    let status = Command::new(bin)
        .args(parts)
        .arg(&path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed launching editor '{editor}'"))?;
    if !status.success() {
        bail!("editor exited with non-zero status");
    }

    let after = fs::read(&path)?;
    if before == after {
        println!("No changes to `{}`.", doc.name);
        return Ok(());
    }

    if !no_sync && path.starts_with(home.vault_dir()) {
        cmd_sync(
            home,
            Some(&format!("docs: update {}", doc.name)),
            false,
            false,
            false,
        )?;
    } else if !path.starts_with(home.vault_dir()) {
        println!(
            "Saved `{}` (repo-local doc at {}).",
            doc.name,
            path.display()
        );
    } else {
        println!("Saved `{}` (skipped sync due to --no-sync).", doc.name);
    }
    Ok(())
}

pub fn cmd_path(corpus: &Corpus, doc_query: &str) -> Result<()> {
    let doc = resolve_doc(&corpus.docs, doc_query)?;
    println!("{}", doc.source_path);
    Ok(())
}

pub fn cmd_doctor(home: &KernHome, corpus: &Corpus, as_json: bool) -> Result<()> {
    let mut issues = Vec::new();
    let mut notices = Vec::new();

    for w in &corpus.warnings {
        issues.push(format!("compose: {w}"));
    }

    let must_bytes: usize = corpus.must_read().iter().map(|d| d.bytes).sum();
    if must_bytes > 15_000 {
        notices.push(format!(
            "Must Read total is {:.1} KB (recommended <= 15 KB for 1-2 page bootstrap)",
            must_bytes as f64 / 1024.0
        ));
    }

    let missing_triggers: Vec<&str> = corpus
        .on_demand()
        .into_iter()
        .filter(|d| d.trigger.is_none())
        .map(|d| d.name.as_str())
        .collect();
    if !missing_triggers.is_empty() {
        issues.push(format!(
            "on_demand docs missing `trigger:`: {}",
            missing_triggers.join(", ")
        ));
    }

    for b in &home.load_bindings()?.binding {
        let p = expand_tilde(&b.path);
        if !p.exists() {
            notices.push(format!("bindings.toml path does not exist: {}", b.path));
        }
    }

    let unapproved_codex = crate::setup::detect_unapproved_codex_kern_hooks();
    if !unapproved_codex.is_empty() {
        let rows = unapproved_codex
            .iter()
            .map(crate::setup::UnapprovedCodexHook::display_label)
            .collect::<Vec<_>>()
            .join(", ");
        notices.push(format!(
            "Codex has {} unapproved kern hook(s) ({rows}): approve in Codex Desktop -> Settings -> Hooks or `/hooks` in Codex CLI",
            unapproved_codex.len()
        ));
    }

    if let Ok(cfg) = home.load_config()
        && let Some(msg) = crate::update::doctor_update_notice(home, &cfg)
    {
        notices.push(msg);
    }

    let vault = home.vault_dir();
    if vault.join(".git").exists() {
        let out = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&vault)
            .output();
        if let Ok(o) = out {
            let dirty = String::from_utf8_lossy(&o.stdout).lines().count();
            if dirty > 0 {
                notices.push(format!(
                    "vault has {dirty} uncommitted change(s) (run `kern sync --commit \"...\"`)"
                ));
            }
        }
    } else {
        notices.push("vault is not a Git repository".to_string());
    }

    let counts = count_by_scope(&corpus.docs);
    if as_json {
        let payload = serde_json::json!({
            "ok": issues.is_empty(),
            "home": home.root,
            "vault": vault,
            "user": corpus.workspace.user,
            "project": corpus.workspace.project,
            "mode": corpus.workspace.mode,
            "norm": corpus.workspace.norm,
            "harness": corpus.workspace.harness,
            "provider": corpus.workspace.provider,
            "source": corpus.workspace.source.label(),
            "total_docs": corpus.docs.len(),
            "counts_by_scope": counts,
            "must_read_bytes": must_bytes,
            "notices": notices,
            "issues": issues,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        if !issues.is_empty() {
            bail!("doctor found {} issue(s)", issues.len());
        }
        return Ok(());
    }

    println!("kern doctor");
    println!("  home:    {}", home.root.display());
    println!("  vault:   {}", vault.display());
    println!("  user:    {}", corpus.workspace.user);
    println!(
        "  context: project={} mode={} norm={} harness={} (via {})",
        corpus.workspace.project.as_deref().unwrap_or("(global)"),
        corpus.workspace.mode,
        corpus.workspace.norm,
        corpus.workspace.harness.as_deref().unwrap_or("(any)"),
        corpus.workspace.source.label()
    );
    if let Some(prov) = corpus.workspace.provider.as_deref() {
        println!("  provider: {prov}");
    }
    let summary = counts
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(", ");
    println!("  docs:    {} total ({summary})", corpus.docs.len());

    if issues.is_empty() && notices.is_empty() {
        println!("\nStatus: OK");
        return Ok(());
    }
    if !notices.is_empty() {
        println!("\nNotices:");
        for n in &notices {
            println!("  [note] {n}");
        }
    }
    if !issues.is_empty() {
        println!("\nIssues:");
        for i in &issues {
            println!("  [fail] {i}");
        }
        bail!("doctor found {} issue(s)", issues.len());
    }
    println!("\nStatus: OK (with {} notice(s))", notices.len());
    Ok(())
}

fn ensure_md(name: &str) -> String {
    if name.ends_with(".md") || name.ends_with(".MD") {
        name.to_string()
    } else {
        format!("{name}.md")
    }
}

fn scope_dir(
    home: &KernHome,
    cfg: &GlobalConfig,
    scope: Scope,
    owner: Option<&str>,
) -> Result<PathBuf> {
    let vault = home.vault_dir();
    Ok(match scope {
        Scope::Core => vault.join("core"),
        Scope::Mode => vault.join("modes").join(owner.unwrap_or(&cfg.default_mode)),
        Scope::Norm => vault
            .join("norms")
            .join(owner.unwrap_or(cfg.effective_default_norm())),
        Scope::User => vault.join("users").join(owner.unwrap_or(&cfg.user)),
        Scope::Project => {
            let p = owner.context("--owner <project-name> is required for --scope project")?;
            vault.join("projects").join(p)
        }
    })
}

fn rewrite_frontmatter(raw: &str, tier: Option<Tier>, trigger: Option<&str>) -> Result<String> {
    let normalized = raw.replace("\r\n", "\n");
    let (mut fm, body) = split_frontmatter(&normalized)?;
    if let Some(t) = tier {
        fm.tier = Some(t.label().to_string());
    }
    if let Some(tr) = trigger {
        fm.trigger = Some(tr.to_string());
    }
    let mut header = String::from("---\n");
    if let Some(name) = &fm.name {
        header.push_str(&format!("name: {name:?}\n"));
    }
    if let Some(t) = &fm.tier {
        header.push_str(&format!("tier: {t}\n"));
    }
    if let Some(tr) = fm.trigger.as_deref().or(fm.description.as_deref()) {
        header.push_str(&format!("trigger: {tr:?}\n"));
    }
    if !fm.paths.is_empty() {
        let joined = fm
            .paths
            .iter()
            .map(|p| format!("{p:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        header.push_str(&format!("paths: [{joined}]\n"));
    }
    if !fm.harnesses.is_empty() {
        let joined = fm
            .harnesses
            .iter()
            .map(|h| format!("{h:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        header.push_str(&format!("harnesses: [{joined}]\n"));
    }
    if !fm.providers.is_empty() {
        let joined = fm
            .providers
            .iter()
            .map(|p| format!("{p:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        header.push_str(&format!("providers: [{joined}]\n"));
    }
    if let Some(p) = fm.priority {
        header.push_str(&format!("priority: {p}\n"));
    }
    header.push_str("---\n\n");
    Ok(format!("{header}{}\n", body.trim()))
}

fn extract_local_region(existing: &str) -> String {
    if let Some(s) = existing.find(LOCAL_START) {
        let after = &existing[s + LOCAL_START.len()..];
        if let Some(e) = after.find(LOCAL_END) {
            return after[..e].trim_matches('\n').to_string();
        }
    }
    String::new()
}

fn render_bootstrap_file(local_region: &str) -> String {
    let local_body = if local_region.is_empty() {
        "\n".to_string()
    } else {
        format!("\n{local_region}\n")
    };
    format!(
        "{MANAGED_START}\n{}\n{MANAGED_END}\n{LOCAL_START}{local_body}{LOCAL_END}\n",
        DEFAULT_MANAGED_BOOTSTRAP.trim_end()
    )
}
