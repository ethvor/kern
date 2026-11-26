use std::io::{self, BufRead, Write};
use std::path::Path;

use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::{KernHome, ResolveOverrides, resolve_workspace};
use crate::doc::{Scope, Tier, compose, load_all_vault_docs, resolve_doc};
use crate::render::{render_bootstrap, render_list, render_read};
use crate::search::search_docs;
use crate::setup::{codex_hook_approval_notice, detect_unapproved_codex_kern_hooks};
use crate::update::bootstrap_update_notice;

#[derive(Deserialize)]
struct RpcRequest {
    #[allow(dead_code)]
    jsonrpc: Option<String>,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

pub fn serve_stdio(home: &KernHome) -> Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    let mut client_name = String::new();

    for line in stdin.lock().lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let req: RpcRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(err) => {
                write_error(
                    &mut out,
                    Value::Null,
                    -32700,
                    &format!("parse error: {err}"),
                )?;
                continue;
            }
        };

        let is_notification = req.id.is_none();
        let id = req.id.unwrap_or(Value::Null);

        match req.method.as_str() {
            "initialize" => {
                if let Some(name) = req
                    .params
                    .get("clientInfo")
                    .and_then(|c| c.get("name"))
                    .and_then(Value::as_str)
                {
                    client_name = name.to_string();
                }
                let version = req
                    .params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("2025-11-25");
                let result = json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "kern", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": "Call kern_bootstrap at session start (and after compaction). If a page ends with NEXT REQUIRED, fetch subsequent pages until COMPLETE. Use kern_read for on-demand documents and kern_search to locate standards."
                });
                if !is_notification {
                    write_result(&mut out, id, result)?;
                }
            }
            "notifications/initialized" => {}
            "ping" => {
                if !is_notification {
                    write_result(&mut out, id, json!({}))?;
                }
            }
            "tools/list" => {
                if !is_notification {
                    write_result(&mut out, id, json!({ "tools": tool_definitions() }))?;
                }
            }
            "tools/call" => {
                if !is_notification {
                    let result = dispatch_tool(home, &req.params, &client_name);
                    write_result(&mut out, id, result)?;
                }
            }
            _ => {
                if !is_notification {
                    write_error(
                        &mut out,
                        id,
                        -32601,
                        &format!("method not found: {}", req.method),
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn tool_definitions() -> Value {
    json!([
        {
            "name": "kern_bootstrap",
            "title": "Bootstrap project context",
            "description": "Returns the compact tiered routing index and bounded Must Read context pages (<= 7.5 KB each). If a page ends with NEXT REQUIRED, call again with page=2, etc. until COMPLETE.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "dir": { "type": "string", "description": "Optional repository directory path" },
                    "project": { "type": "string", "description": "Optional explicit project name" },
                    "mode": { "type": "string", "description": "Optional mode override" },
                    "norm": { "type": "string", "description": "Optional norm override" },
                    "user": { "type": "string", "description": "Optional user override" },
                    "harness": { "type": "string", "description": "Optional harness override (e.g. codex, claude, antigravity)" },
                    "provider": { "type": "string", "description": "Optional provider override (e.g. openai, anthropic, google)" },
                    "page": { "type": "integer", "minimum": 1, "default": 1 }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "kern_read",
            "title": "Read governed document(s)",
            "description": "Reads one or more documents by human name in bounded <= 7.5 KB pages. Supports --toc to inspect headings and --section to read a single heading.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Single document name" },
                    "names": { "type": "array", "items": { "type": "string" }, "description": "Multiple document names (e.g. for a subagent brief)" },
                    "dir": { "type": "string" },
                    "project": { "type": "string" },
                    "mode": { "type": "string" },
                    "norm": { "type": "string" },
                    "user": { "type": "string" },
                    "harness": { "type": "string" },
                    "provider": { "type": "string" },
                    "page": { "type": "integer", "minimum": 1, "default": 1 },
                    "toc": { "type": "boolean", "default": false },
                    "section": { "type": "string", "description": "Optional heading title to extract" }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "kern_search",
            "title": "Search governed documents",
            "description": "Searches composed project docs (or the whole vault when all=true) by keyword or regex.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "minLength": 1 },
                    "dir": { "type": "string" },
                    "project": { "type": "string" },
                    "harness": { "type": "string" },
                    "provider": { "type": "string" },
                    "regex": { "type": "boolean", "default": false },
                    "all": { "type": "boolean", "default": false }
                },
                "required": ["query"],
                "additionalProperties": false
            }
        },
        {
            "name": "kern_list",
            "title": "List available documents",
            "description": "Lists active composed documents grouped by tier, with optional scope or tier filter.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "dir": { "type": "string" },
                    "project": { "type": "string" },
                    "harness": { "type": "string" },
                    "provider": { "type": "string" },
                    "scope": { "type": "string", "enum": ["core", "mode", "norm", "user", "project"] },
                    "tier": { "type": "string", "enum": ["must_read", "on_demand", "reference"] }
                },
                "additionalProperties": false
            }
        }
    ])
}

fn dispatch_tool(home: &KernHome, params: &Value, client_name: &str) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let outcome = match name {
        "kern_bootstrap" => run_bootstrap(home, &args, client_name),
        "kern_read" => run_read(home, &args, client_name),
        "kern_search" => run_search(home, &args, client_name),
        "kern_list" => run_list(home, &args, client_name),
        other => Err(anyhow::anyhow!("unknown tool '{other}'")),
    };

    match outcome {
        Ok(text) => json!({
            "content": [{ "type": "text", "text": text }],
            "isError": false
        }),
        Err(err) => json!({
            "content": [{ "type": "text", "text": format!("kern error: {err:#}") }],
            "isError": true
        }),
    }
}

fn effective_mcp_harness<'a>(args: &'a Value, client_name: &'a str) -> Option<&'a str> {
    args.get("harness")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            let trimmed = client_name.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        })
}

fn run_bootstrap(home: &KernHome, args: &Value, client_name: &str) -> Result<String> {
    let cfg = home.load_config()?;
    let dir = args.get("dir").and_then(Value::as_str).map(Path::new);
    let project = args.get("project").and_then(Value::as_str);
    let mode = args.get("mode").and_then(Value::as_str);
    let norm = args.get("norm").and_then(Value::as_str);
    let user = args.get("user").and_then(Value::as_str);
    let harness = effective_mcp_harness(args, client_name);
    let provider = args.get("provider").and_then(Value::as_str);
    let page = args
        .get("page")
        .and_then(Value::as_u64)
        .map(|p| usize::try_from(p).unwrap_or(usize::MAX))
        .unwrap_or(1);
    let ws = resolve_workspace(
        home,
        &cfg,
        ResolveOverrides {
            dir,
            project,
            mode,
            norm,
            user,
            harness,
            provider,
        },
    )?;
    let is_codex_client = ws.harness.as_deref() == Some("codex");
    let corpus = compose(home, &cfg, ws)?;
    let mut notices = Vec::new();
    if is_codex_client
        && let Some(msg) = codex_hook_approval_notice(&detect_unapproved_codex_kern_hooks())
    {
        notices.push(msg);
    }
    if let Some(msg) = bootstrap_update_notice(home, &cfg) {
        notices.push(msg);
    }
    render_bootstrap(&corpus, page, false, cfg.page_limit(), &notices)
}

fn run_read(home: &KernHome, args: &Value, client_name: &str) -> Result<String> {
    let cfg = home.load_config()?;
    let dir = args.get("dir").and_then(Value::as_str).map(Path::new);
    let project = args.get("project").and_then(Value::as_str);
    let mode = args.get("mode").and_then(Value::as_str);
    let norm = args.get("norm").and_then(Value::as_str);
    let user = args.get("user").and_then(Value::as_str);
    let harness = effective_mcp_harness(args, client_name);
    let provider = args.get("provider").and_then(Value::as_str);
    let page = args
        .get("page")
        .and_then(Value::as_u64)
        .map(|p| usize::try_from(p).unwrap_or(usize::MAX))
        .unwrap_or(1);
    let toc = args.get("toc").and_then(Value::as_bool).unwrap_or(false);
    let section = args.get("section").and_then(Value::as_str);

    let mut names: Vec<String> = Vec::new();
    if let Some(single) = args.get("name").and_then(Value::as_str) {
        names.push(single.to_string());
    }
    if let Some(arr) = args.get("names").and_then(Value::as_array) {
        for v in arr {
            if let Some(s) = v.as_str() {
                names.push(s.to_string());
            }
        }
    }
    if names.is_empty() {
        anyhow::bail!("kern_read requires `name` or `names`");
    }

    let ws = resolve_workspace(
        home,
        &cfg,
        ResolveOverrides {
            dir,
            project,
            mode,
            norm,
            user,
            harness,
            provider,
        },
    )?;
    let corpus = compose(home, &cfg, ws)?;
    let mut resolved = Vec::with_capacity(names.len());
    for q in &names {
        resolved.push(resolve_doc(&corpus.docs, q)?);
    }
    render_read(&resolved, page, toc, section, false, cfg.page_limit())
}

fn run_search(home: &KernHome, args: &Value, client_name: &str) -> Result<String> {
    let cfg = home.load_config()?;
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("kern_search requires `query`"))?;
    let dir = args.get("dir").and_then(Value::as_str).map(Path::new);
    let project = args.get("project").and_then(Value::as_str);
    let harness = effective_mcp_harness(args, client_name);
    let provider = args.get("provider").and_then(Value::as_str);
    let use_regex = args.get("regex").and_then(Value::as_bool).unwrap_or(false);
    let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);

    if all {
        let docs = load_all_vault_docs(home)?;
        return search_docs(&docs, query, use_regex, false, cfg.page_limit());
    }
    let ws = resolve_workspace(
        home,
        &cfg,
        ResolveOverrides {
            dir,
            project,
            mode: None,
            norm: None,
            user: None,
            harness,
            provider,
        },
    )?;
    let corpus = compose(home, &cfg, ws)?;
    search_docs(&corpus.docs, query, use_regex, false, cfg.page_limit())
}

fn run_list(home: &KernHome, args: &Value, client_name: &str) -> Result<String> {
    let cfg = home.load_config()?;
    let dir = args.get("dir").and_then(Value::as_str).map(Path::new);
    let project = args.get("project").and_then(Value::as_str);
    let harness = effective_mcp_harness(args, client_name);
    let provider = args.get("provider").and_then(Value::as_str);
    let scope = args
        .get("scope")
        .and_then(Value::as_str)
        .map(Scope::parse)
        .transpose()?;
    let tier = args
        .get("tier")
        .and_then(Value::as_str)
        .map(Tier::parse)
        .transpose()?;

    let ws = resolve_workspace(
        home,
        &cfg,
        ResolveOverrides {
            dir,
            project,
            mode: None,
            norm: None,
            user: None,
            harness,
            provider,
        },
    )?;
    let corpus = compose(home, &cfg, ws)?;
    render_list(&corpus, scope, tier, false)
}

fn write_result(out: &mut impl Write, id: Value, result: Value) -> Result<()> {
    let resp = json!({ "jsonrpc": "2.0", "id": id, "result": result });
    serde_json::to_writer(&mut *out, &resp)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

fn write_error(out: &mut impl Write, id: Value, code: i64, message: &str) -> Result<()> {
    let resp = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    });
    serde_json::to_writer(&mut *out, &resp)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}
