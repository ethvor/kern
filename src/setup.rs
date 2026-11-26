use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};

use crate::config::KernHome;

const SERVER_NAME: &str = "kern";
const COMPACT_MSG: &str =
    "Context compacted: run kern bootstrap (or kern_bootstrap) before context-dependent work.";

/// Handles Claude Code / Codex lifecycle hook JSON on stdin.
/// Emits structured `hookSpecificOutput` ONLY on compaction (`PostCompact` or
/// `SessionStart` with `source` in `compact|clear`) and stays silent on normal
/// startup or when `kern` has not been initialized.
pub fn cmd_hook() -> Result<()> {
    let mut raw = String::new();
    let _ = io::stdin().take(1_048_576).read_to_string(&mut raw);
    let Ok(val) = serde_json::from_str::<Value>(&raw) else {
        return Ok(());
    };
    let event = val
        .get("hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let source = val.get("source").and_then(Value::as_str).unwrap_or("");
    let should_fire = match event {
        "PostCompact" => true,
        "SessionStart" => matches!(source, "compact" | "clear"),
        _ => false,
    };
    if !should_fire {
        return Ok(());
    }

    if KernHome::resolve().is_ok_and(|h| !h.config_path().is_file()) {
        return Ok(());
    }

    let out = json!({
        "hookSpecificOutput": {
            "hookEventName": event,
            "additionalContext": COMPACT_MSG
        }
    });
    println!("{out}");
    Ok(())
}

#[derive(Debug, Clone)]
pub struct UnapprovedCodexHook {
    pub event: String,
    pub row_num: usize,
    pub status_message: Option<String>,
    pub state_key: String,
    pub command: String,
}

impl UnapprovedCodexHook {
    pub fn display_label(&self) -> String {
        match self
            .status_message
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            Some(msg) => format!("{} → {} - {}", self.event, self.row_num, msg.trim()),
            None => format!("{} → Hook {}", self.event, self.row_num),
        }
    }
}

/// Read-only check for `kern` hooks in `~/.codex/hooks.json` that do not yet
/// have an active `trusted_hash` approval in `~/.codex/config.toml`.
pub fn detect_unapproved_codex_kern_hooks() -> Vec<UnapprovedCodexHook> {
    let codex_home = match env::var_os("CODEX_HOME").filter(|v| !v.is_empty()) {
        Some(v) => PathBuf::from(v),
        None => match dirs::home_dir() {
            Some(h) => h.join(".codex"),
            None => return Vec::new(),
        },
    };
    let hooks_path = codex_home.join("hooks.json");
    let Ok(hooks_raw) = fs::read_to_string(&hooks_path) else {
        return Vec::new();
    };
    let Ok(hooks_val) = serde_json::from_str::<Value>(&hooks_raw) else {
        return Vec::new();
    };
    let Some(events_map) = hooks_val.get("hooks").and_then(Value::as_object) else {
        return Vec::new();
    };

    let config_path = codex_home.join("config.toml");
    let state_table: Option<toml::map::Map<String, toml::Value>> = fs::read_to_string(&config_path)
        .ok()
        .and_then(|raw| toml::from_str::<toml::Value>(&raw).ok())
        .and_then(|v| v.get("hooks")?.get("state")?.as_table().cloned());
    let stale_hashes = load_stale_codex_hashes();

    let hooks_path_str = hooks_path.display().to_string();
    let mut unapproved = Vec::new();

    for (event_name, groups_val) in events_map {
        let Some(groups) = groups_val.as_array() else {
            continue;
        };
        let snake_event = pascal_to_snake(event_name);
        let mut display_row = 0usize;
        for (group_idx, group) in groups.iter().enumerate() {
            let Some(handlers) = group.get("hooks").and_then(Value::as_array) else {
                continue;
            };
            for (handler_idx, handler) in handlers.iter().enumerate() {
                display_row += 1;
                let Some(cmd) = handler.get("command").and_then(Value::as_str) else {
                    continue;
                };
                if !is_kern_hook_command(cmd) {
                    continue;
                }
                let state_key = format!("{hooks_path_str}:{snake_event}:{group_idx}:{handler_idx}");
                let approved = state_table
                    .as_ref()
                    .and_then(|t| t.get(&state_key))
                    .and_then(toml::Value::as_table)
                    .is_some_and(|entry| {
                        let hash = entry
                            .get("trusted_hash")
                            .and_then(toml::Value::as_str)
                            .map(str::trim)
                            .unwrap_or("");
                        let is_stale = stale_hashes.get(&state_key).is_some_and(|old| old == hash);
                        let disabled = entry.get("enabled").and_then(toml::Value::as_bool)
                            == Some(false)
                            || entry.get("disabled").and_then(toml::Value::as_bool) == Some(true);
                        !hash.is_empty() && !is_stale && !disabled
                    });
                if !approved {
                    let status_message = handler
                        .get("statusMessage")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    unapproved.push(UnapprovedCodexHook {
                        event: event_name.clone(),
                        row_num: display_row,
                        status_message,
                        state_key,
                        command: cmd.to_string(),
                    });
                }
            }
        }
    }

    unapproved.sort_by(|a, b| (&a.event, &a.state_key).cmp(&(&b.event, &b.state_key)));
    unapproved
}

pub fn codex_hook_approval_notice(unapproved: &[UnapprovedCodexHook]) -> Option<String> {
    if unapproved.is_empty() {
        return None;
    }
    let rows = unapproved
        .iter()
        .map(|h| format!("`{}`", h.display_label()))
        .collect::<Vec<_>>()
        .join(", ");
    let cmd = unapproved
        .first()
        .map(|h| h.command.as_str())
        .unwrap_or("kern hook");
    Some(format!(
        "> **REQUIRED ACTION: `kern` HOOKS UNAPPROVED ({} pending: {rows})**\n\
         > Codex is silently skipping required `kern` hooks. Agents cannot self-approve hooks (do not edit `~/.codex/config.toml`).\n\
         > **Mandatory reply lead:** Put the following callout at the very top of your response verbatim (do not soften, bury, or omit it):\n\
         >\n\
         > **REQUIRED: Accept `kern` hooks ({rows}):** Accepting these hooks (`{cmd}`) is required for the `kern` context system to function properly across context compactions.\n\
         > **How to accept:** Open **Codex Desktop -> Settings -> Hooks -> User config** (or run `/hooks` in Codex CLI) and click **Trust** on {rows}.",
        unapproved.len()
    ))
}

fn stale_hashes_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".kern").join("codex-stale-hook-hashes.json"))
}

fn load_stale_codex_hashes() -> std::collections::BTreeMap<String, String> {
    let Some(path) = stale_hashes_path() else {
        return std::collections::BTreeMap::new();
    };
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn record_stale_codex_hashes(hooks_path: &Path, codex_cfg: &Path) {
    let Ok(cfg_raw) = fs::read_to_string(codex_cfg) else {
        return;
    };
    let Ok(cfg_val) = toml::from_str::<toml::Value>(&cfg_raw) else {
        return;
    };
    let Some(state_table) = cfg_val
        .get("hooks")
        .and_then(|h| h.get("state"))
        .and_then(toml::Value::as_table)
    else {
        return;
    };
    let Ok(hooks_raw) = fs::read_to_string(hooks_path) else {
        return;
    };
    let Ok(hooks_val) = serde_json::from_str::<Value>(&hooks_raw) else {
        return;
    };
    let Some(events_map) = hooks_val.get("hooks").and_then(Value::as_object) else {
        return;
    };
    let hooks_path_str = hooks_path.display().to_string();
    let mut map = load_stale_codex_hashes();
    for (event_name, groups_val) in events_map {
        let Some(groups) = groups_val.as_array() else {
            continue;
        };
        let snake_event = pascal_to_snake(event_name);
        for (group_idx, group) in groups.iter().enumerate() {
            let Some(handlers) = group.get("hooks").and_then(Value::as_array) else {
                continue;
            };
            for (handler_idx, handler) in handlers.iter().enumerate() {
                let Some(cmd) = handler.get("command").and_then(Value::as_str) else {
                    continue;
                };
                if !is_kern_hook_command(cmd) {
                    continue;
                }
                let state_key = format!("{hooks_path_str}:{snake_event}:{group_idx}:{handler_idx}");
                if let Some(hash) = state_table
                    .get(&state_key)
                    .and_then(toml::Value::as_table)
                    .and_then(|e| e.get("trusted_hash"))
                    .and_then(toml::Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                {
                    map.insert(state_key, hash.trim().to_string());
                }
            }
        }
    }
    if let Some(out_path) = stale_hashes_path() {
        if let Some(parent) = out_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string_pretty(&map) {
            let _ = fs::write(out_path, format!("{text}\n"));
        }
    }
}

fn is_kern_hook_command(cmd: &str) -> bool {
    cmd.contains("run kern bootstrap")
        || cmd.ends_with("/kern\" hook")
        || cmd.ends_with("/kern' hook")
        || cmd.ends_with("\\kern\" hook")
        || cmd.ends_with("\\kern.exe\" hook")
        || cmd == "kern hook"
}

fn pascal_to_snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Registers `kern mcp` (and compaction-only reminder hooks where supported)
/// across detected local harnesses without per-turn hook overhead.
pub fn cmd_setup(dry_run: bool, uninstall: bool) -> Result<()> {
    let home = dirs::home_dir().context("could not determine user home directory")?;
    let kern_bin = resolve_kern_bin();
    let mut found = 0usize;

    // 1. Claude Code (~/.claude.json MCP + ~/.claude/settings.json compaction hook)
    if home.join(".claude").is_dir() || home.join(".claude.json").is_file() {
        found += 1;
        let mcp_path = home.join(".claude.json");
        update_json_mcp(&mcp_path, "mcpServers", &kern_bin, dry_run, uninstall)?;
        let hooks_path = home.join(".claude").join("settings.json");
        update_compact_hook(&hooks_path, "claude", &kern_bin, dry_run, uninstall)?;
    }

    // 2. Codex (~/.codex/config.toml MCP + ~/.codex/hooks.json compaction hook)
    if home.join(".codex").is_dir() {
        found += 1;
        let codex_cfg = home.join(".codex").join("config.toml");
        update_codex_mcp(&codex_cfg, &kern_bin, dry_run, uninstall)?;
        let hooks_path = home.join(".codex").join("hooks.json");
        let changed = update_compact_hook(&hooks_path, "codex", &kern_bin, dry_run, uninstall)?;
        if !dry_run && !uninstall {
            if changed {
                record_stale_codex_hashes(&hooks_path, &codex_cfg);
            }
            let unapproved = detect_unapproved_codex_kern_hooks();
            if !unapproved.is_empty() {
                let rows = unapproved
                    .iter()
                    .map(UnapprovedCodexHook::display_label)
                    .collect::<Vec<_>>()
                    .join(", ");
                println!(
                    "[note] Codex requires approving new hooks once ({rows}) in Codex Desktop -> Settings -> Hooks or `/hooks` in Codex CLI."
                );
            }
        }
    }

    // 3. Gemini CLI (~/.gemini/settings.json MCP)
    if home.join(".gemini").is_dir() {
        found += 1;
        let path = home.join(".gemini").join("settings.json");
        update_json_mcp(&path, "mcpServers", &kern_bin, dry_run, uninstall)?;
    }

    // 4. OpenCode (~/.config/opencode/opencode.json MCP)
    if home.join(".config/opencode").is_dir() {
        found += 1;
        let path = home.join(".config/opencode/opencode.json");
        update_opencode_mcp(&path, &kern_bin, dry_run, uninstall)?;
    }

    if found == 0 {
        println!(
            "No local harness directories detected (~/.claude, ~/.codex, ~/.gemini, ~/.config/opencode)."
        );
        println!("The `kern` CLI works directly in any shell harness without MCP.");
    } else if dry_run {
        println!("\n(Dry run: pass without --dry-run to apply changes.)");
    }
    Ok(())
}

fn resolve_kern_bin() -> String {
    if let Ok(exe) = env::current_exe()
        && let Some(s) = exe.to_str()
    {
        return s.to_string();
    }
    "kern".to_string()
}

fn update_json_mcp(
    path: &Path,
    servers_key: &str,
    kern_bin: &str,
    dry_run: bool,
    uninstall: bool,
) -> Result<()> {
    let mut root = read_json_object(path)?;
    let servers = root
        .entry(servers_key)
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .context("expected object for MCP servers map")?;

    let action = if uninstall {
        servers.remove(SERVER_NAME);
        "removed"
    } else {
        servers.insert(
            SERVER_NAME.to_string(),
            json!({
                "command": kern_bin,
                "args": ["mcp"]
            }),
        );
        "registered"
    };

    if dry_run {
        println!(
            "[dry-run] {action} `{SERVER_NAME}` MCP in {}",
            path.display()
        );
    } else {
        write_json_object(path, &root)?;
        println!("[ok] {action} `{SERVER_NAME}` MCP in {}", path.display());
    }
    Ok(())
}

fn update_opencode_mcp(path: &Path, kern_bin: &str, dry_run: bool, uninstall: bool) -> Result<()> {
    let mut root = read_json_object(path)?;
    let servers = root
        .entry("mcp")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .context("expected object for opencode `mcp` map")?;

    let action = if uninstall {
        servers.remove(SERVER_NAME);
        "removed"
    } else {
        servers.insert(
            SERVER_NAME.to_string(),
            json!({
                "type": "local",
                "command": [kern_bin, "mcp"],
                "enabled": true
            }),
        );
        "registered"
    };

    if dry_run {
        println!(
            "[dry-run] {action} `{SERVER_NAME}` MCP in {}",
            path.display()
        );
    } else {
        write_json_object(path, &root)?;
        println!("[ok] {action} `{SERVER_NAME}` MCP in {}", path.display());
    }
    Ok(())
}

fn update_codex_mcp(path: &Path, kern_bin: &str, dry_run: bool, uninstall: bool) -> Result<()> {
    let existing = if path.is_file() {
        fs::read_to_string(path)?
    } else {
        String::new()
    };

    let marker_start = "# kern:mcp:start";
    let marker_end = "# kern:mcp:end";
    let stripped = strip_marked_block(&existing, marker_start, marker_end);

    let (next, action) = if uninstall {
        (stripped, "removed")
    } else {
        let block = format!(
            "\n{marker_start}\n[mcp_servers.{SERVER_NAME}]\ncommand = {:?}\nargs = [\"mcp\"]\n{marker_end}\n",
            kern_bin
        );
        (format!("{}{block}", stripped.trim_end()), "registered")
    };

    if dry_run {
        println!(
            "[dry-run] {action} `{SERVER_NAME}` MCP in {}",
            path.display()
        );
    } else {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p)?;
        }
        fs::write(path, next)?;
        println!("[ok] {action} `{SERVER_NAME}` MCP in {}", path.display());
    }
    Ok(())
}

fn update_compact_hook(
    path: &Path,
    label: &str,
    kern_bin: &str,
    dry_run: bool,
    uninstall: bool,
) -> Result<bool> {
    let before = read_json_object(path)?;
    let mut root = before.clone();
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .context("expected object for hooks map")?;

    let hook_cmd = format!("\"{kern_bin}\" hook");
    let is_kern_hook = |entry: &Value| {
        entry
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|arr| {
                arr.iter().any(|h| {
                    h.get("command")
                        .and_then(Value::as_str)
                        .is_some_and(|c| c == hook_cmd || is_kern_hook_command(c))
                })
            })
    };

    for (event, status_msg) in [
        ("PostCompact", "kern PostCompact context check"),
        ("SessionStart", "kern SessionStart compaction check"),
    ] {
        let arr = hooks
            .entry(event)
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .with_context(|| format!("expected array for {event} hooks"))?;
        arr.retain(|entry| !is_kern_hook(entry));
        if !uninstall {
            arr.push(json!({
                "hooks": [{
                    "type": "command",
                    "command": hook_cmd,
                    "timeout": 5,
                    "statusMessage": status_msg
                }]
            }));
        }
    }

    let changed = root != before;
    let action = if uninstall { "removed" } else { "installed" };

    if dry_run {
        println!(
            "[dry-run] {action} compaction hook (SessionStart+PostCompact) for {label} in {}",
            path.display()
        );
    } else {
        write_json_object(path, &root)?;
        println!(
            "[ok] {action} compaction hook (SessionStart+PostCompact) for {label} in {}",
            path.display()
        );
    }
    Ok(changed)
}

fn read_json_object(path: &Path) -> Result<Map<String, Value>> {
    if !path.is_file() {
        return Ok(Map::new());
    }
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed reading {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(Map::new());
    }
    let val: Value = serde_json::from_str(&raw)
        .with_context(|| format!("invalid JSON in {}", path.display()))?;
    val.as_object()
        .cloned()
        .with_context(|| format!("expected top-level JSON object in {}", path.display()))
}

fn write_json_object(path: &Path, obj: &Map<String, Value>) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut text = serde_json::to_string_pretty(&Value::Object(obj.clone()))?;
    text.push('\n');
    let tmp = PathBuf::from(format!("{}.kern.tmp", path.display()));
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn strip_marked_block(input: &str, start: &str, end: &str) -> String {
    let Some(s) = input.find(start) else {
        return input.to_string();
    };
    let Some(e_rel) = input[s..].find(end) else {
        return input.to_string();
    };
    let after = s + e_rel + end.len();
    let mut out = String::new();
    out.push_str(input[..s].trim_end());
    let tail = input[after..].trim_start_matches('\n');
    if !tail.is_empty() {
        out.push('\n');
        out.push_str(tail);
    }
    out.push('\n');
    out
}
