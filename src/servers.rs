use std::collections::BTreeMap;
use std::fs;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::config::{
    GlobalConfig, KernHome, ProjectManifest, ResolveOverrides, ServerSpec, VaultProjectMeta,
    expand_tilde, find_manifest, load_optional_toml, resolve_workspace, save_toml,
};

const PROBE_TIMEOUT: Duration = Duration::from_millis(60);

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedServer {
    pub id: String,
    pub kind: &'static str,
    pub project: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub up: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port_conflict: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cmd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    pub source: String,
}

#[derive(Debug, Clone)]
struct CandidateServer {
    project: String,
    spec: ServerSpec,
    dir: Option<PathBuf>,
    source: &'static str,
}

pub struct ServerAddOptions<'a> {
    pub dir: Option<&'a Path>,
    pub project: Option<&'a str>,
    pub name: &'a str,
    pub port: u16,
    pub host: &'a str,
    pub path: Option<&'a str>,
    pub cmd: Option<&'a str>,
    pub description: Option<&'a str>,
    pub in_vault: bool,
}

pub fn cmd_servers_list(
    home: &KernHome,
    cfg: &GlobalConfig,
    dir: Option<&Path>,
    project: Option<&str>,
    all: bool,
    as_json: bool,
) -> Result<()> {
    let candidates = if all {
        collect_all_servers(home, cfg)?
    } else {
        let ws = resolve_workspace(
            home,
            cfg,
            ResolveOverrides {
                dir,
                project,
                mode: None,
                norm: None,
                user: None,
                harness: None,
                provider: None,
            },
        )?;
        let Some(proj) = ws.project else {
            if as_json {
                println!("[]");
            } else {
                println!("No active project resolved. Pass `--all` or `--project <name>`.");
            }
            return Ok(());
        };
        collect_project_servers(home, &proj, ws.root.as_deref())?
    };

    let resolved = probe_servers(candidates);

    if as_json {
        println!("{}", serde_json::to_string_pretty(&resolved)?);
        return Ok(());
    }

    if resolved.is_empty() {
        println!(
            "No official project servers declared{}.",
            if all { "" } else { " for this project" }
        );
        println!("Register one with `kern servers add --name <name> --port <port> [--private]`.");
        return Ok(());
    }

    let id_w = resolved
        .iter()
        .map(|s| s.id.len())
        .max()
        .unwrap_or(10)
        .max(7);
    println!(
        "{:<id_w$}  {:>5}  {:<8}  {:<8}  DESCRIPTION / CMD",
        "SERVER", "PORT", "STATUS", "SOURCE"
    );
    for s in &resolved {
        let status = if s.up {
            "up"
        } else if s.port_conflict.is_some() {
            "conflict"
        } else {
            "down"
        };
        let detail = match &s.port_conflict {
            Some(c) => c.as_str(),
            None => s.description.as_deref().or(s.cmd.as_deref()).unwrap_or("-"),
        };
        println!(
            "{:<id_w$}  {:>5}  {:<8}  {:<8}  {}",
            s.id, s.port, status, s.source, detail
        );
    }
    Ok(())
}

pub fn cmd_servers_add(
    home: &KernHome,
    cfg: &GlobalConfig,
    opts: ServerAddOptions<'_>,
) -> Result<()> {
    validate_server_name(opts.name)?;
    if opts.port == 0 {
        bail!("port must be between 1 and 65535");
    }

    let ws = resolve_workspace(
        home,
        cfg,
        ResolveOverrides {
            dir: opts.dir,
            project: opts.project,
            mode: None,
            norm: None,
            user: None,
            harness: None,
            provider: None,
        },
    )?;

    let project = ws.project.with_context(
        || "could not resolve project (run inside an adopted repo or pass --project <name>)",
    )?;

    let spec = ServerSpec {
        name: opts.name.to_string(),
        port: opts.port,
        host: opts.host.to_string(),
        path: opts.path.map(str::to_string),
        cmd: opts.cmd.map(str::to_string),
        description: opts.description.map(str::to_string),
    };

    let manifest_path = if opts.in_vault {
        None
    } else {
        ws.root.as_deref().and_then(existing_manifest_path)
    };

    if let Some(mpath) = manifest_path {
        let raw = fs::read_to_string(&mpath)
            .with_context(|| format!("failed reading {}", mpath.display()))?;
        let mut manifest: ProjectManifest =
            toml::from_str(&raw).with_context(|| format!("invalid TOML in {}", mpath.display()))?;
        upsert_server(&mut manifest.servers, spec);
        save_toml(&mpath, &manifest)?;
        println!(
            "Registered `{}:{}` (port {}) in {}",
            project,
            opts.name,
            opts.port,
            mpath.display()
        );
    } else {
        let vpath = vault_project_meta_path(home, &project);
        let mut meta: VaultProjectMeta = load_optional_toml(&vpath)?;
        if meta.dir.is_none()
            && let Some(root) = &ws.root
        {
            meta.dir = Some(root.display().to_string());
        }
        upsert_server(&mut meta.servers, spec);
        save_toml(&vpath, &meta)?;
        println!(
            "Registered `{}:{}` (port {}) in {}",
            project,
            opts.name,
            opts.port,
            vpath.display()
        );
    }

    Ok(())
}

pub fn cmd_servers_rm(
    home: &KernHome,
    cfg: &GlobalConfig,
    name: &str,
    dir: Option<&Path>,
    project: Option<&str>,
    in_vault: bool,
) -> Result<()> {
    let ws = resolve_workspace(
        home,
        cfg,
        ResolveOverrides {
            dir,
            project,
            mode: None,
            norm: None,
            user: None,
            harness: None,
            provider: None,
        },
    )?;

    let project = ws.project.with_context(
        || "could not resolve project (run inside an adopted repo or pass --project <name>)",
    )?;

    let mut removed = false;

    let vpath = vault_project_meta_path(home, &project);
    if vpath.is_file() {
        let mut meta: VaultProjectMeta = load_optional_toml(&vpath)?;
        let before = meta.servers.len();
        meta.servers.retain(|s| s.name != name);
        if meta.servers.len() != before {
            save_toml(&vpath, &meta)?;
            println!("Removed `{project}:{name}` from {}", vpath.display());
            removed = true;
        }
    }

    if !in_vault && let Some(mpath) = ws.root.as_deref().and_then(existing_manifest_path) {
        let raw = fs::read_to_string(&mpath)
            .with_context(|| format!("failed reading {}", mpath.display()))?;
        let mut manifest: ProjectManifest =
            toml::from_str(&raw).with_context(|| format!("invalid TOML in {}", mpath.display()))?;
        let before = manifest.servers.len();
        manifest.servers.retain(|s| s.name != name);
        if manifest.servers.len() != before {
            save_toml(&mpath, &manifest)?;
            println!("Removed `{project}:{name}` from {}", mpath.display());
            removed = true;
        }
    }

    if !removed {
        bail!("no declared server `{name}` found for project `{project}`");
    }
    Ok(())
}

fn collect_all_servers(home: &KernHome, cfg: &GlobalConfig) -> Result<Vec<CandidateServer>> {
    // Keyed by (project, server_name). Vault project.toml overrides repo kern.toml on name collision.
    let mut merged: BTreeMap<(String, String), CandidateServer> = BTreeMap::new();
    let mut project_dirs: BTreeMap<String, PathBuf> = BTreeMap::new();

    // 1. Adopted manifests directly under configured project_roots (depth 1 only, never recursive crawl)
    for root_raw in &cfg.project_roots {
        let root = expand_tilde(root_raw);
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if let Some(mpath) = existing_manifest_path(&path)
                && let Ok(raw) = fs::read_to_string(&mpath)
                && let Ok(manifest) = toml::from_str::<ProjectManifest>(&raw)
            {
                project_dirs
                    .entry(manifest.project.clone())
                    .or_insert_with(|| path.clone());
                for spec in manifest.servers {
                    merged.insert(
                        (manifest.project.clone(), spec.name.clone()),
                        CandidateServer {
                            project: manifest.project.clone(),
                            spec,
                            dir: Some(path.clone()),
                            source: "manifest",
                        },
                    );
                }
            }
        }
    }

    // 2. External stealth bindings in ~/.kern/bindings.toml
    if let Ok(bindings) = home.load_bindings() {
        for b in bindings.binding {
            let path = expand_tilde(&b.path);
            project_dirs.entry(b.project).or_insert(path);
        }
    }

    // 3. Vault project metadata files (~/.kern/vault/projects/<project>/project.toml)
    let projects_root = home.vault_dir().join("projects");
    if let Ok(entries) = fs::read_dir(&projects_root) {
        for entry in entries.flatten() {
            let pdir = entry.path();
            if !pdir.is_dir() {
                continue;
            }
            let Some(project) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if project.starts_with('.') || project.starts_with('_') {
                continue;
            }
            let meta_path = pdir.join("project.toml");
            if !meta_path.is_file() {
                continue;
            }
            let meta: VaultProjectMeta = load_optional_toml(&meta_path)?;
            let dir = meta
                .dir
                .as_deref()
                .map(expand_tilde)
                .or_else(|| project_dirs.get(&project).cloned())
                .or_else(|| {
                    cfg.project_roots.iter().find_map(|root_raw| {
                        let cand = expand_tilde(root_raw).join(&project);
                        cand.is_dir().then_some(cand)
                    })
                });
            for spec in meta.servers {
                merged.insert(
                    (project.clone(), spec.name.clone()),
                    CandidateServer {
                        project: project.clone(),
                        spec,
                        dir: dir.clone(),
                        source: "vault",
                    },
                );
            }
        }
    }

    Ok(merged.into_values().collect())
}

fn collect_project_servers(
    home: &KernHome,
    project: &str,
    repo_root: Option<&Path>,
) -> Result<Vec<CandidateServer>> {
    let mut merged: BTreeMap<String, CandidateServer> = BTreeMap::new();

    if let Some(root) = repo_root
        && let Ok(Some((_, manifest))) = find_manifest(root)
    {
        for spec in manifest.servers {
            merged.insert(
                spec.name.clone(),
                CandidateServer {
                    project: project.to_string(),
                    spec,
                    dir: Some(root.to_path_buf()),
                    source: "manifest",
                },
            );
        }
    }

    let vpath = vault_project_meta_path(home, project);
    if vpath.is_file() {
        let meta: VaultProjectMeta = load_optional_toml(&vpath)?;
        let dir = meta
            .dir
            .as_deref()
            .map(expand_tilde)
            .or_else(|| repo_root.map(Path::to_path_buf));
        for spec in meta.servers {
            merged.insert(
                spec.name.clone(),
                CandidateServer {
                    project: project.to_string(),
                    spec,
                    dir: dir.clone(),
                    source: "vault",
                },
            );
        }
    }

    Ok(merged.into_values().collect())
}

fn probe_servers(candidates: Vec<CandidateServer>) -> Vec<ResolvedServer> {
    let mut handles = Vec::with_capacity(candidates.len());
    for cand in candidates {
        handles.push(thread::spawn(move || {
            let tcp_up = is_port_listening(&cand.spec.host, cand.spec.port);
            let (up, port_conflict) = if tcp_up && is_loopback_host(&cand.spec.host) {
                verify_local_listener(&cand)
            } else {
                (tcp_up, None)
            };
            ResolvedServer {
                id: format!("{}:{}", cand.project, cand.spec.name),
                kind: "project",
                project: cand.project,
                name: cand.spec.name,
                host: cand.spec.host,
                port: cand.spec.port,
                path: cand.spec.path,
                up,
                port_conflict,
                cmd: cand.spec.cmd,
                description: cand.spec.description,
                dir: cand.dir.map(|d| d.display().to_string()),
                source: cand.source.to_string(),
            }
        }));
    }

    let mut out: Vec<ResolvedServer> = handles.into_iter().filter_map(|h| h.join().ok()).collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    matches!(
        h.as_str(),
        "127.0.0.1" | "localhost" | "::1" | "0.0.0.0" | "::"
    )
}

#[derive(Debug, Clone)]
struct ListenerProc {
    pid: u32,
    cmdline: String,
    cwd: Option<PathBuf>,
}

fn verify_local_listener(cand: &CandidateServer) -> (bool, Option<String>) {
    let listeners = inspect_local_port_listeners(cand.spec.port);
    if listeners.is_empty() {
        return (true, None);
    }

    let cand_dir_canon = cand
        .dir
        .as_deref()
        .map(|d| fs::canonicalize(d).unwrap_or_else(|_| d.to_path_buf()));
    let cand_dir_raw = cand.dir.as_deref().map(|d| d.to_string_lossy().to_string());
    let proj_lower = cand.project.to_ascii_lowercase();
    let proj_under = proj_lower.replace('-', "_");

    for lp in &listeners {
        if let Some(ref want_dir) = cand_dir_canon
            && let Some(ref cwd) = lp.cwd
        {
            let cwd_canon = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.clone());
            if cwd_canon.starts_with(want_dir) {
                return (true, None);
            }
        }
        let cmd_lower = lp.cmdline.to_ascii_lowercase();
        if let Some(ref raw) = cand_dir_raw
            && lp.cmdline.contains(raw.as_str())
        {
            return (true, None);
        }
        if let Some(ref want_dir) = cand_dir_canon
            && let Some(s) = want_dir.to_str()
            && lp.cmdline.contains(s)
        {
            return (true, None);
        }
        if cmdline_mentions_token(&cmd_lower, &proj_lower)
            || (proj_under != proj_lower && cmdline_mentions_token(&cmd_lower, &proj_under))
        {
            return (true, None);
        }
    }

    // If neither dir nor project name matched and we have a project dir or distinct command, report conflict.
    if cand_dir_canon.is_none() && cand.spec.cmd.is_none() {
        return (true, None);
    }

    let first = &listeners[0];
    let compact = compact_cmdline(&first.cmdline, 72);
    (
        false,
        Some(format!(
            "port {} in use by pid {} ({})",
            cand.spec.port, first.pid, compact
        )),
    )
}

fn cmdline_mentions_token(haystack: &str, token: &str) -> bool {
    if token.len() < 3 {
        return false;
    }
    let mut offset = 0usize;
    let bytes = haystack.as_bytes();
    while let Some(pos) = haystack[offset..].find(token) {
        let start = offset + pos;
        let end = start + token.len();
        let left_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        let right_ok = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if left_ok && right_ok {
            return true;
        }
        offset = start + 1;
    }
    false
}

fn compact_cmdline(raw: &str, max_len: usize) -> String {
    let trimmed = raw.trim();
    if trimmed.len() <= max_len {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max_len.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

fn find_lsof_bin() -> &'static str {
    if Path::new("/usr/sbin/lsof").is_file() {
        "/usr/sbin/lsof"
    } else if Path::new("/usr/bin/lsof").is_file() {
        "/usr/bin/lsof"
    } else {
        "lsof"
    }
}

fn inspect_local_port_listeners(port: u16) -> Vec<ListenerProc> {
    if cfg!(windows) {
        return Vec::new();
    }
    let mut pids: Vec<u32> = Vec::new();
    if let Ok(out) = Command::new(find_lsof_bin())
        .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fp"])
        .stdin(Stdio::null())
        .output()
        && out.status.success()
    {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some(rest) = line.strip_prefix('p')
                && let Ok(pid) = rest.trim().parse::<u32>()
                && !pids.contains(&pid)
            {
                pids.push(pid);
            }
        }
    }
    if pids.is_empty() && cfg!(target_os = "linux") {
        // Fallback for Linux hosts without lsof installed
        if let Ok(out) = Command::new("ss")
            .args(["-H", "-tlpn", &format!("sport = :{port}")])
            .stdin(Stdio::null())
            .output()
            && out.status.success()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            for chunk in text.split("pid=").skip(1) {
                let digits: String = chunk.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(pid) = digits.parse::<u32>()
                    && !pids.contains(&pid)
                {
                    pids.push(pid);
                }
            }
        }
    }

    let mut procs = Vec::new();
    for pid in pids {
        let cmdline = read_pid_cmdline(pid);
        let cwd = read_pid_cwd(pid);
        if !cmdline.is_empty() || cwd.is_some() {
            procs.push(ListenerProc { pid, cmdline, cwd });
        }
    }
    procs
}

fn read_pid_cmdline(pid: u32) -> String {
    if cfg!(target_os = "linux")
        && let Ok(raw) = fs::read(format!("/proc/{pid}/cmdline"))
    {
        let joined = raw
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        if !joined.is_empty() {
            return joined;
        }
    }
    Command::new("ps")
        .args(["-ww", "-p", &pid.to_string(), "-o", "command="])
        .stdin(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn read_pid_cwd(pid: u32) -> Option<PathBuf> {
    if cfg!(target_os = "linux")
        && let Ok(p) = fs::read_link(format!("/proc/{pid}/cwd"))
    {
        return Some(p);
    }
    let out = Command::new(find_lsof_bin())
        .args(["-a", "-d", "cwd", "-p", &pid.to_string(), "-Fn"])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(rest) = line.strip_prefix('n') {
            let p = rest.trim();
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
        }
    }
    None
}

fn is_port_listening(host: &str, port: u16) -> bool {
    let Ok(addrs) = (host, port).to_socket_addrs() else {
        return false;
    };
    let targets: Vec<SocketAddr> = addrs.collect();
    for addr in targets {
        if TcpStream::connect_timeout(&addr, PROBE_TIMEOUT).is_ok() {
            return true;
        }
    }
    false
}

fn existing_manifest_path(root: &Path) -> Option<PathBuf> {
    [root.join(".kern").join("kern.toml"), root.join("kern.toml")]
        .into_iter()
        .find(|candidate| candidate.is_file())
}

fn vault_project_meta_path(home: &KernHome, project: &str) -> PathBuf {
    home.vault_dir()
        .join("projects")
        .join(project)
        .join("project.toml")
}

fn upsert_server(list: &mut Vec<ServerSpec>, spec: ServerSpec) {
    if let Some(existing) = list.iter_mut().find(|s| s.name == spec.name) {
        *existing = spec;
    } else {
        list.push(spec);
    }
}

fn validate_server_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("server name '{name}' must contain only ASCII letters, digits, '-' or '_'");
    }
    Ok(())
}
