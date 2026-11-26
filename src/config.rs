use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub const DEFAULT_MAX_PAGE_BYTES: usize = 7500;
pub const DEFAULT_MODE: &str = "standard";

#[derive(Debug, Clone)]
pub struct KernHome {
    pub root: PathBuf,
    pub vault_override: Option<PathBuf>,
    pub user_override: Option<String>,
    pub sync_peers_override: Option<Vec<String>>,
}

impl KernHome {
    pub fn resolve() -> Result<Self> {
        let root = if let Ok(val) = env::var("KERN_HOME") {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                expand_tilde(trimmed)
            } else {
                dirs::home_dir()
                    .context("could not determine user home directory")?
                    .join(".kern")
            }
        } else {
            dirs::home_dir()
                .context("could not determine user home directory")?
                .join(".kern")
        };
        Ok(Self {
            root,
            vault_override: None,
            user_override: None,
            sync_peers_override: None,
        })
    }

    /// Switches vault context to a named managed vault (e.g. `--vault team`).
    pub fn with_managed_vault(&self, name: &str) -> Result<Self> {
        let cfg = self.load_config()?;
        let mv = cfg.managed_vaults.get(name).with_context(|| {
            let avail: Vec<&str> = cfg.managed_vaults.keys().map(String::as_str).collect();
            format!(
                "unknown managed vault '{name}' in {} (configured: {})",
                self.config_path().display(),
                if avail.is_empty() {
                    "none".to_string()
                } else {
                    avail.join(", ")
                }
            )
        })?;
        let mut next = self.clone();
        next.vault_override = Some(expand_tilde(&mv.path));
        next.user_override = Some(mv.user.clone().unwrap_or_else(|| name.to_string()));
        next.sync_peers_override = Some(mv.peer.clone().into_iter().collect());
        Ok(next)
    }

    pub fn config_path(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    pub fn bindings_path(&self) -> PathBuf {
        self.root.join("bindings.toml")
    }

    pub fn guards_dir(&self) -> PathBuf {
        self.root.join("guards")
    }

    pub fn vault_dir(&self) -> PathBuf {
        self.vault_override
            .clone()
            .unwrap_or_else(|| self.root.join("vault"))
    }

    pub fn load_config(&self) -> Result<GlobalConfig> {
        let path = self.config_path();
        if !path.exists() {
            bail!(
                "kern is not initialized ({} not found). Run `kern init --user <name>` first.",
                path.display()
            );
        }
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed reading {}", path.display()))?;
        let mut cfg: GlobalConfig =
            toml::from_str(&raw).with_context(|| format!("invalid TOML in {}", path.display()))?;
        if cfg.user.trim().is_empty() {
            bail!("`user` must be non-empty in {}", path.display());
        }
        if let Some(u) = &self.user_override {
            cfg.user = u.clone();
            cfg.default_norm = Some(u.clone());
        }
        if let Some(peers) = &self.sync_peers_override {
            cfg.sync_peers = peers.clone();
        }
        Ok(cfg)
    }

    pub fn load_bindings(&self) -> Result<BindingsFile> {
        load_optional_toml(&self.bindings_path())
    }

    pub fn save_bindings(&self, bindings: &BindingsFile) -> Result<()> {
        save_toml(&self.bindings_path(), bindings)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalConfig {
    pub user: String,
    #[serde(default = "default_mode")]
    pub default_mode: String,
    #[serde(default)]
    pub default_norm: Option<String>,
    #[serde(default = "default_max_page_bytes")]
    pub max_page_bytes: usize,
    /// Awake peer aliases to nudge with `kern sync --pull-only` right after push
    #[serde(default)]
    pub sync_peers: Vec<String>,
    /// Command prefix for remote peer commands (defaults to `ssh`)
    #[serde(default)]
    pub remote_cmd: Option<String>,
    /// Command prefix for copying files to remote peers (defaults to `rsync` over `ssh`)
    #[serde(default)]
    pub remote_copy_cmd: Option<String>,
    /// Additional vaults you collaborate on or maintain locally
    #[serde(default)]
    pub managed_vaults: BTreeMap<String, ManagedVault>,
    /// Update behavior on bootstrap: "auto", "prompt", "off", or omitted/unset (asks once)
    #[serde(default)]
    pub update_policy: Option<String>,
    /// Git repository URL checked/built by `kern update`
    #[serde(default)]
    pub update_repo: Option<String>,
    /// Peer aliases pushed/notified automatically when running `kern update`
    #[serde(default)]
    pub update_targets: Vec<String>,
    /// Optional local kern source checkout directory for `kern update`
    #[serde(default)]
    pub update_source_dir: Option<String>,
    /// Optional directories containing adopted project checkouts for `kern servers --all`
    #[serde(default)]
    pub project_roots: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedVault {
    pub path: String,
    #[serde(default)]
    pub user: Option<String>,
    /// Optional SSH host alias to nudge after pushing
    #[serde(default)]
    pub peer: Option<String>,
}

impl GlobalConfig {
    pub fn effective_default_norm(&self) -> &str {
        self.default_norm
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&self.user)
    }

    pub fn page_limit(&self) -> usize {
        self.max_page_bytes.clamp(2048, 32768)
    }

    pub fn effective_update_policy(&self) -> &str {
        match self
            .update_policy
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some("auto") => "auto",
            Some("prompt") => "prompt",
            Some("off") => "off",
            _ => "unset",
        }
    }

    pub fn effective_update_repo(&self) -> &str {
        self.update_repo
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(crate::update::DEFAULT_UPDATE_REPO)
    }
}

fn default_mode() -> String {
    DEFAULT_MODE.to_string()
}

fn default_max_page_bytes() -> usize {
    DEFAULT_MAX_PAGE_BYTES
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BindingsFile {
    #[serde(default)]
    pub binding: Vec<StealthBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StealthBinding {
    pub path: String,
    pub project: String,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub norm: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerSpec {
    /// Short name unique within the project (e.g. "preview", "api", "web")
    pub name: String,
    /// Localhost TCP port
    pub port: u16,
    /// Localhost bind address (defaults to "127.0.0.1")
    #[serde(default = "default_server_host")]
    pub host: String,
    /// Optional URL path (e.g. "/docs")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Optional local start command
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd: Option<String>,
    /// Optional human description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

fn default_server_host() -> String {
    "127.0.0.1".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VaultProjectMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<ServerSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectManifest {
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub norm: Option<String>,
    #[serde(default = "default_harness_files")]
    pub harness_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<ServerSpec>,
}

pub fn default_harness_files() -> Vec<String> {
    vec![
        "AGENTS.md".to_string(),
        "CLAUDE.md".to_string(),
        "GEMINI.md".to_string(),
    ]
}

#[derive(Debug, Clone)]
pub struct ResolvedWorkspace {
    pub root: Option<PathBuf>,
    pub project: Option<String>,
    pub mode: String,
    pub norm: String,
    pub user: String,
    pub harness: Option<String>,
    pub provider: Option<String>,
    pub source: WorkspaceSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceSource {
    Manifest,
    StealthBinding,
    ExplicitFlags,
    GlobalDefaults,
}

impl WorkspaceSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Manifest => "kern.toml",
            Self::StealthBinding => "stealth-binding",
            Self::ExplicitFlags => "flags",
            Self::GlobalDefaults => "defaults",
        }
    }
}

pub struct ResolveOverrides<'a> {
    pub dir: Option<&'a Path>,
    pub project: Option<&'a str>,
    pub mode: Option<&'a str>,
    pub norm: Option<&'a str>,
    pub user: Option<&'a str>,
    pub harness: Option<&'a str>,
    pub provider: Option<&'a str>,
}

pub fn normalize_harness(raw: &str) -> String {
    let lc = raw.trim().to_ascii_lowercase();
    if lc.contains("claude") {
        "claude".to_string()
    } else if lc.contains("codex") {
        "codex".to_string()
    } else if lc.contains("antigravity") || lc == "agy" {
        "antigravity".to_string()
    } else if lc.contains("gemini") {
        "gemini".to_string()
    } else if lc.contains("opencode") {
        "opencode".to_string()
    } else {
        lc
    }
}

pub fn normalize_provider(raw: &str) -> String {
    raw.trim().to_ascii_lowercase()
}

/// Detects the active coding harness from runtime environment variables.
/// Checks innermost CLI harness markers before outer IDE markers so nested
/// invocations (e.g., an IDE spawning `claude` or `codex`) detect accurately.
pub fn detect_harness_env() -> Option<String> {
    if let Ok(v) = env::var("KERN_HARNESS") {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            return Some(normalize_harness(trimmed));
        }
    }
    let env_nonempty = |k: &str| env::var_os(k).is_some_and(|v| !v.is_empty());
    if env_nonempty("CLAUDECODE")
        || env_nonempty("CLAUDE_CODE_ENTRYPOINT")
        || env_nonempty("CLAUDE_CODE_SESSION_ID")
    {
        return Some("claude".to_string());
    }
    if env_nonempty("CODEX_THREAD_ID")
        || env_nonempty("CODEX_SANDBOX")
        || env_nonempty("CODEX_CI")
        || env_nonempty("CODEX_MANAGED_BY_NPM")
        || env_nonempty("CODEX_MANAGED_BY_BUN")
    {
        return Some("codex".to_string());
    }
    if env_nonempty("GEMINI_CLI") {
        return Some("gemini".to_string());
    }
    if env_nonempty("OPENCODE") {
        return Some("opencode".to_string());
    }
    if env_nonempty("ANTIGRAVITY_AGENT")
        || env_nonempty("ANTIGRAVITY_CONVERSATION_ID")
        || env_nonempty("ANTIGRAVITY_LS_ADDRESS")
    {
        return Some("antigravity".to_string());
    }
    if env_nonempty("CODEX_HOME") {
        return Some("codex".to_string());
    }
    None
}

pub fn detect_provider_env() -> Option<String> {
    let v = env::var("KERN_PROVIDER").ok()?;
    let trimmed = v.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(normalize_provider(trimmed))
    }
}

pub fn resolve_workspace(
    home: &KernHome,
    cfg: &GlobalConfig,
    ov: ResolveOverrides<'_>,
) -> Result<ResolvedWorkspace> {
    let start_dir = match ov.dir {
        Some(p) => canonicalize_lossy(p),
        None => env::current_dir()
            .map(|p| canonicalize_lossy(&p))
            .context("failed to get current directory")?,
    };

    let user = ov
        .user
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&cfg.user)
        .to_string();

    let harness = ov
        .harness
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(normalize_harness)
        .or_else(detect_harness_env);

    let provider = ov
        .provider
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(normalize_provider)
        .or_else(detect_provider_env);

    if let Some((repo_root, manifest)) = find_manifest(&start_dir)? {
        return Ok(ResolvedWorkspace {
            root: Some(repo_root),
            project: Some(ov.project.map(str::to_string).unwrap_or(manifest.project)),
            mode: ov
                .mode
                .map(str::to_string)
                .or(manifest.mode)
                .unwrap_or_else(|| cfg.default_mode.clone()),
            norm: ov
                .norm
                .map(str::to_string)
                .or(manifest.norm)
                .unwrap_or_else(|| cfg.effective_default_norm().to_string()),
            user,
            harness,
            provider,
            source: WorkspaceSource::Manifest,
        });
    }

    let bindings = home.load_bindings()?;
    if let Some((bound_root, binding)) = match_binding(&start_dir, &bindings) {
        return Ok(ResolvedWorkspace {
            root: Some(bound_root),
            project: Some(ov.project.map(str::to_string).unwrap_or(binding.project)),
            mode: ov
                .mode
                .map(str::to_string)
                .or(binding.mode)
                .unwrap_or_else(|| cfg.default_mode.clone()),
            norm: ov
                .norm
                .map(str::to_string)
                .or(binding.norm)
                .unwrap_or_else(|| cfg.effective_default_norm().to_string()),
            user,
            harness,
            provider,
            source: WorkspaceSource::StealthBinding,
        });
    }

    let has_explicit = ov.project.is_some() || ov.mode.is_some() || ov.norm.is_some();
    Ok(ResolvedWorkspace {
        root: find_git_root(&start_dir),
        project: ov.project.map(str::to_string),
        mode: ov
            .mode
            .map(str::to_string)
            .unwrap_or_else(|| cfg.default_mode.clone()),
        norm: ov
            .norm
            .map(str::to_string)
            .unwrap_or_else(|| cfg.effective_default_norm().to_string()),
        user,
        harness,
        provider,
        source: if has_explicit {
            WorkspaceSource::ExplicitFlags
        } else {
            WorkspaceSource::GlobalDefaults
        },
    })
}

pub fn find_manifest(start: &Path) -> Result<Option<(PathBuf, ProjectManifest)>> {
    let mut cur = start;
    loop {
        for candidate in [cur.join(".kern").join("kern.toml"), cur.join("kern.toml")] {
            if candidate.is_file() {
                let raw = fs::read_to_string(&candidate)
                    .with_context(|| format!("failed reading {}", candidate.display()))?;
                let manifest: ProjectManifest = toml::from_str(&raw)
                    .with_context(|| format!("invalid TOML in {}", candidate.display()))?;
                if manifest.project.trim().is_empty() {
                    bail!("`project` must be non-empty in {}", candidate.display());
                }
                return Ok(Some((cur.to_path_buf(), manifest)));
            }
        }
        if cur.join(".git").exists() {
            return Ok(None);
        }
        match cur.parent() {
            Some(parent) => cur = parent,
            None => return Ok(None),
        }
    }
}

pub fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut cur = start;
    loop {
        if cur.join(".git").exists() {
            return Some(cur.to_path_buf());
        }
        cur = cur.parent()?;
    }
}

fn match_binding(start: &Path, bindings: &BindingsFile) -> Option<(PathBuf, StealthBinding)> {
    let mut best: Option<(usize, PathBuf, StealthBinding)> = None;
    for b in &bindings.binding {
        let bpath = canonicalize_lossy(&expand_tilde(&b.path));
        if start.starts_with(&bpath) {
            let depth = bpath.components().count();
            let replace = best.as_ref().is_none_or(|(d, _, _)| depth > *d);
            if replace {
                best = Some((depth, bpath, b.clone()));
            }
        }
    }
    best.map(|(_, p, b)| (p, b))
}

pub fn expand_tilde(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    } else if raw == "~"
        && let Some(home) = dirs::home_dir()
    {
        return home;
    }
    PathBuf::from(raw)
}

pub fn canonicalize_lossy(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub fn load_optional_toml<T: Default + for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    if !path.exists() {
        return Ok(T::default());
    }
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed reading {}", path.display()))?;
    toml::from_str(&raw).with_context(|| format!("invalid TOML in {}", path.display()))
}

pub fn save_toml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(value)?;
    fs::write(path, text).with_context(|| format!("failed writing {}", path.display()))
}
