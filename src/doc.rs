use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use globset::{Glob, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use crate::builtin::{BUILTIN_ORIENTATION, BUILTIN_ORIENTATION_NAME};
use crate::config::{
    GlobalConfig, KernHome, ResolvedWorkspace, normalize_harness, normalize_provider,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Core,
    Mode,
    Norm,
    User,
    Project,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Mode => "mode",
            Self::Norm => "norm",
            Self::User => "user",
            Self::Project => "project",
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "core" => Ok(Self::Core),
            "mode" => Ok(Self::Mode),
            "norm" => Ok(Self::Norm),
            "user" => Ok(Self::User),
            "project" => Ok(Self::Project),
            other => bail!("unknown scope '{other}' (expected core|mode|norm|user|project)"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    MustRead,
    OnDemand,
    Reference,
}

impl Tier {
    pub fn label(self) -> &'static str {
        match self {
            Self::MustRead => "must_read",
            Self::OnDemand => "on_demand",
            Self::Reference => "reference",
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "must_read" | "must-read" | "immediate" | "read_now" | "read-now" => Ok(Self::MustRead),
            "on_demand" | "on-demand" | "read_when_needed" | "read-when-needed" => {
                Ok(Self::OnDemand)
            }
            "reference" | "reference_only" | "reference-only" => Ok(Self::Reference),
            other => bail!("unknown tier '{other}' (expected must_read|on_demand|reference)"),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawFrontmatter {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub trigger: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub harnesses: Vec<String>,
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub priority: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Document {
    pub name: String,
    pub key: String,
    pub scope: Scope,
    pub tier: Tier,
    pub trigger: Option<String>,
    pub paths: Vec<String>,
    pub harnesses: Vec<String>,
    pub providers: Vec<String>,
    pub matched_paths: bool,
    pub priority: i32,
    pub source_path: String,
    pub bytes: usize,
    #[serde(skip_serializing)]
    pub body: String,
}

impl Document {
    pub fn matches_audience(&self, harness: Option<&str>, provider: Option<&str>) -> bool {
        if !self.harnesses.is_empty() {
            let Some(active) = harness else {
                return false;
            };
            if !self.harnesses.iter().any(|h| h == active) {
                return false;
            }
        }
        if !self.providers.is_empty() {
            let Some(active) = provider else {
                return false;
            };
            if !self.providers.iter().any(|p| p == active) {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Clone)]
pub struct Corpus {
    pub workspace: ResolvedWorkspace,
    pub docs: Vec<Document>,
    pub warnings: Vec<String>,
}

impl Corpus {
    pub fn must_read(&self) -> Vec<&Document> {
        let mut out: Vec<_> = self
            .docs
            .iter()
            .filter(|d| d.tier == Tier::MustRead)
            .collect();
        out.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.scope.cmp(&b.scope))
                .then_with(|| a.name.cmp(&b.name))
        });
        out
    }

    pub fn on_demand(&self) -> Vec<&Document> {
        let mut out: Vec<_> = self
            .docs
            .iter()
            .filter(|d| d.tier == Tier::OnDemand)
            .collect();
        out.sort_by(|a, b| {
            b.matched_paths
                .cmp(&a.matched_paths)
                .then_with(|| a.name.cmp(&b.name))
        });
        out
    }

    pub fn reference(&self) -> Vec<&Document> {
        let mut out: Vec<_> = self
            .docs
            .iter()
            .filter(|d| d.tier == Tier::Reference)
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

pub fn normalize_key(name: &str) -> String {
    let stripped = name
        .trim()
        .strip_suffix(".md")
        .or_else(|| name.trim().strip_suffix(".MD"))
        .unwrap_or(name.trim());
    stripped
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

pub fn compose(
    home: &KernHome,
    _cfg: &GlobalConfig,
    workspace: ResolvedWorkspace,
) -> Result<Corpus> {
    let vault = home.vault_dir();
    let mut by_key: HashMap<String, Document> = HashMap::new();
    let mut warnings = Vec::new();

    // 1. Built-in universal orientation
    let builtin_doc = parse_markdown_str(
        BUILTIN_ORIENTATION_NAME,
        Scope::Core,
        "<builtin:KERN_ORIENTATION.md>",
        BUILTIN_ORIENTATION,
    )?;
    by_key.insert(builtin_doc.key.clone(), builtin_doc);

    // 2. Scope layers in strict precedence order:
    //    core -> mode -> norm -> user -> repo-local .kern/docs -> vault project
    let mut layers: Vec<(Scope, PathBuf)> = vec![
        (Scope::Core, vault.join("core")),
        (Scope::Mode, vault.join("modes").join(&workspace.mode)),
        (Scope::Norm, vault.join("norms").join(&workspace.norm)),
        (Scope::User, vault.join("users").join(&workspace.user)),
    ];
    if let Some(root) = &workspace.root {
        let local_docs = root.join(".kern").join("docs");
        if local_docs.is_dir() {
            layers.push((Scope::Project, local_docs));
        }
    }
    if let Some(proj) = &workspace.project {
        layers.push((Scope::Project, vault.join("projects").join(proj)));
    }

    for (scope, dir) in layers {
        if !dir.is_dir() {
            continue;
        }
        load_scope_dir(
            scope,
            &dir,
            workspace.harness.as_deref(),
            workspace.provider.as_deref(),
            true,
            &mut by_key,
            &mut warnings,
        )?;
    }

    // 3. Match optional `paths:` globs against current worktree files (only if any doc uses `paths:`)
    let mut docs: Vec<Document> = by_key.into_values().collect();
    if docs.iter().any(|d| !d.paths.is_empty())
        && let Some(root) = &workspace.root
    {
        let repo_files = collect_worktree_files(root, 4000);
        if !repo_files.is_empty() {
            for doc in &mut docs {
                if !doc.paths.is_empty() {
                    doc.matched_paths = matches_any_glob(&doc.paths, &repo_files);
                }
            }
        }
    }

    docs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Corpus {
        workspace,
        docs,
        warnings,
    })
}

pub fn load_all_vault_docs(home: &KernHome) -> Result<Vec<Document>> {
    let vault = home.vault_dir();
    let mut out = Vec::new();
    let mut warnings = Vec::new();

    let scope_roots = [
        (Scope::Core, vault.join("core")),
        (Scope::Mode, vault.join("modes")),
        (Scope::Norm, vault.join("norms")),
        (Scope::User, vault.join("users")),
        (Scope::Project, vault.join("projects")),
    ];

    for (scope, root) in scope_roots {
        if !root.is_dir() {
            continue;
        }
        let mut map = HashMap::new();
        load_scope_dir(scope, &root, None, None, false, &mut map, &mut warnings)?;
        out.extend(map.into_values());
    }
    out.sort_by(|a, b| a.scope.cmp(&b.scope).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

fn load_scope_dir(
    scope: Scope,
    dir: &Path,
    harness: Option<&str>,
    provider: Option<&str>,
    filter_audience: bool,
    by_key: &mut HashMap<String, Document>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let mut files = Vec::new();
    collect_markdown_files(dir, &mut files)?;
    files.sort();
    for path in files {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unnamed");
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed reading {}", path.display()))?;
        match parse_markdown_str(stem, scope, &path.display().to_string(), &raw) {
            Ok(doc) => {
                if !filter_audience || doc.matches_audience(harness, provider) {
                    by_key.insert(doc.key.clone(), doc);
                }
            }
            Err(err) => {
                warnings.push(format!("{}: {err}", path.display()));
            }
        }
    }
    Ok(())
}

fn collect_markdown_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("reading dir {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') || name_str.starts_with('_') {
            continue;
        }
        let ft = entry.file_type()?;
        if ft.is_dir() {
            collect_markdown_files(&path, out)?;
        } else if ft.is_file()
            && path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        {
            out.push(path);
        }
    }
    Ok(())
}

pub fn parse_markdown_str(
    default_name: &str,
    scope: Scope,
    source_path: &str,
    raw: &str,
) -> Result<Document> {
    let normalized: Cow<'_, str> = if raw.contains('\r') {
        Cow::Owned(raw.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(raw)
    };
    let (fm, body) = split_frontmatter(&normalized)?;

    let name = fm
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default_name)
        .to_string();

    let tier = match fm.tier.as_deref() {
        Some(t) if !t.trim().is_empty() => Tier::parse(t)?,
        _ => Tier::Reference,
    };

    let trigger = fm
        .trigger
        .or(fm.description)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let priority = fm.priority.unwrap_or(match scope {
        Scope::Core => 10,
        Scope::Mode => 20,
        Scope::Norm => 30,
        Scope::User => 40,
        Scope::Project => 50,
    });

    let harnesses: Vec<String> = fm
        .harnesses
        .into_iter()
        .map(|h| normalize_harness(&h))
        .filter(|h| !h.is_empty())
        .collect();
    let providers: Vec<String> = fm
        .providers
        .into_iter()
        .map(|p| normalize_provider(&p))
        .filter(|p| !p.is_empty())
        .collect();

    let clean_body = body.trim().to_string();
    let bytes = clean_body.len();
    Ok(Document {
        key: normalize_key(&name),
        name,
        scope,
        tier,
        trigger,
        paths: fm.paths,
        harnesses,
        providers,
        matched_paths: false,
        priority,
        source_path: source_path.to_string(),
        bytes,
        body: clean_body,
    })
}

pub fn split_frontmatter(input: &str) -> Result<(RawFrontmatter, &str)> {
    let Some(first_nl) = input.find('\n') else {
        return Ok((RawFrontmatter::default(), input));
    };
    if input[..first_nl].trim() != "---" {
        return Ok((RawFrontmatter::default(), input));
    }
    let after_open = &input[first_nl + 1..];
    let mut offset = 0usize;
    let mut close_span: Option<(usize, usize)> = None;
    for line in after_open.split_inclusive('\n') {
        let trimmed = line.trim_end_matches('\n').trim();
        if trimmed == "---" {
            close_span = Some((offset, offset + line.len()));
            break;
        }
        offset += line.len();
    }
    let Some((yaml_end, body_start)) = close_span else {
        return Ok((RawFrontmatter::default(), input));
    };
    let yaml_slice = &after_open[..yaml_end];
    let body = &after_open[body_start..];
    let fm = parse_simple_frontmatter(yaml_slice)?;
    Ok((fm, body))
}

fn parse_simple_frontmatter(yaml: &str) -> Result<RawFrontmatter> {
    let mut fm = RawFrontmatter::default();
    let mut active_list: Option<&str> = None;

    for raw_line in yaml.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(item) = trimmed.strip_prefix("- ") {
            match active_list {
                Some("paths") => fm.paths.push(unquote_scalar(item)),
                Some("harnesses") => fm.harnesses.push(unquote_scalar(item)),
                Some("providers") => fm.providers.push(unquote_scalar(item)),
                _ => {}
            }
            continue;
        }
        active_list = None;
        let Some((key, val_raw)) = trimmed.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let val = val_raw.trim();
        match key {
            "name" if !val.is_empty() => fm.name = Some(unquote_scalar(val)),
            "tier" if !val.is_empty() => fm.tier = Some(unquote_scalar(val)),
            "trigger" if !val.is_empty() => fm.trigger = Some(unquote_scalar(val)),
            "description" if !val.is_empty() => fm.description = Some(unquote_scalar(val)),
            "priority" if !val.is_empty() => {
                fm.priority = Some(
                    unquote_scalar(val)
                        .parse::<i32>()
                        .with_context(|| format!("invalid frontmatter priority '{val}'"))?,
                );
            }
            "paths" => parse_yaml_list_field(val, "paths", &mut fm.paths, &mut active_list),
            "harnesses" | "harness" => {
                parse_yaml_list_field(val, "harnesses", &mut fm.harnesses, &mut active_list)
            }
            "providers" | "provider" => {
                parse_yaml_list_field(val, "providers", &mut fm.providers, &mut active_list)
            }
            _ => {}
        }
    }
    Ok(fm)
}

fn parse_yaml_list_field<'a>(
    val: &str,
    list_name: &'a str,
    target: &mut Vec<String>,
    active_list: &mut Option<&'a str>,
) {
    if val.is_empty() {
        *active_list = Some(list_name);
    } else if let Some(inner) = val.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        for part in inner.split(',') {
            let p = unquote_scalar(part);
            if !p.is_empty() {
                target.push(p);
            }
        }
    } else {
        let p = unquote_scalar(val);
        if !p.is_empty() {
            target.push(p);
        }
    }
}

fn unquote_scalar(raw: &str) -> String {
    let t = raw.trim();
    if t.starts_with('"')
        && t.ends_with('"')
        && t.len() >= 2
        && let Ok(s) = serde_json::from_str::<String>(t)
    {
        return s;
    }
    if t.starts_with('\'') && t.ends_with('\'') && t.len() >= 2 {
        return t[1..t.len() - 1].replace("''", "'");
    }
    t.to_string()
}

pub fn resolve_doc<'a>(docs: &'a [Document], query: &str) -> Result<&'a Document> {
    let q_trimmed = query.trim();
    if q_trimmed.is_empty() {
        bail!("document name cannot be empty");
    }
    if let Some(doc) = docs.iter().find(|d| d.name.eq_ignore_ascii_case(q_trimmed)) {
        return Ok(doc);
    }
    let q_key = normalize_key(q_trimmed);
    if let Some(doc) = docs.iter().find(|d| d.key == q_key) {
        return Ok(doc);
    }
    let prefix_matches: Vec<&Document> =
        docs.iter().filter(|d| d.key.starts_with(&q_key)).collect();
    if prefix_matches.len() == 1 {
        return Ok(prefix_matches[0]);
    }
    let sub_matches: Vec<&Document> = docs.iter().filter(|d| d.key.contains(&q_key)).collect();
    if sub_matches.len() == 1 {
        return Ok(sub_matches[0]);
    }
    let candidates: Vec<&str> = if !prefix_matches.is_empty() {
        prefix_matches.iter().map(|d| d.name.as_str()).collect()
    } else {
        sub_matches.iter().map(|d| d.name.as_str()).collect()
    };
    if !candidates.is_empty() {
        bail!(
            "ambiguous document name '{query}'; matches: {}",
            candidates.join(", ")
        );
    }
    bail!("document '{query}' not found (try `kern list` or `kern search {query}`)")
}

#[derive(Debug, Clone, Serialize)]
pub struct TocEntry {
    pub level: usize,
    pub heading: String,
    pub bytes: usize,
    pub line: usize,
}

pub fn extract_toc(body: &str) -> Vec<TocEntry> {
    let headings = scan_headings(body);
    let mut entries = Vec::with_capacity(headings.len());
    for (idx, h) in headings.iter().enumerate() {
        let end = headings
            .iter()
            .skip(idx + 1)
            .find(|next| next.level <= h.level)
            .map(|next| next.byte_offset)
            .unwrap_or(body.len());
        entries.push(TocEntry {
            level: h.level,
            heading: h.text.clone(),
            bytes: end.saturating_sub(h.byte_offset),
            line: h.line,
        });
    }
    entries
}

pub fn extract_section(body: &str, query: &str) -> Result<String> {
    let headings = scan_headings(body);
    let q_lower = query.trim().to_ascii_lowercase();
    let q_norm = normalize_key(query);
    let idx = headings
        .iter()
        .position(|h| h.text.to_ascii_lowercase() == q_lower)
        .or_else(|| {
            headings
                .iter()
                .position(|h| normalize_key(&h.text) == q_norm)
        })
        .or_else(|| {
            headings
                .iter()
                .position(|h| h.text.to_ascii_lowercase().contains(&q_lower))
        });

    let Some(idx) = idx else {
        let avail: Vec<&str> = headings.iter().map(|h| h.text.as_str()).collect();
        bail!(
            "section '{query}' not found. Available headings: {}",
            if avail.is_empty() {
                "(none)".to_string()
            } else {
                avail.join(" | ")
            }
        );
    };

    let start = headings[idx].byte_offset;
    let level = headings[idx].level;
    let end = headings
        .iter()
        .skip(idx + 1)
        .find(|next| next.level <= level)
        .map(|next| next.byte_offset)
        .unwrap_or(body.len());
    Ok(body[start..end].trim().to_string())
}

struct HeadingSpan {
    level: usize,
    text: String,
    byte_offset: usize,
    line: usize,
}

fn scan_headings(body: &str) -> Vec<HeadingSpan> {
    let mut out = Vec::new();
    let mut in_fence = false;
    let mut fence_char = '`';
    let mut offset = 0usize;

    for (idx, line) in body.split_inclusive('\n').enumerate() {
        let trimmed = line.trim_start();
        if let Some(fc) = fence_marker(trimmed) {
            if !in_fence {
                in_fence = true;
                fence_char = fc;
            } else if fc == fence_char {
                in_fence = false;
            }
        } else if !in_fence && trimmed.starts_with('#') {
            let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
            if (1..=6).contains(&hashes) {
                let rest = &trimmed[hashes..];
                if rest.starts_with(' ') || rest.starts_with('\t') {
                    let text = rest.trim().trim_end_matches('#').trim().to_string();
                    if !text.is_empty() {
                        out.push(HeadingSpan {
                            level: hashes,
                            text,
                            byte_offset: offset,
                            line: idx + 1,
                        });
                    }
                }
            }
        }
        offset += line.len();
    }
    out
}

pub fn fence_marker(trimmed_line: &str) -> Option<char> {
    if trimmed_line.starts_with("```") {
        Some('`')
    } else if trimmed_line.starts_with("~~~") {
        Some('~')
    } else {
        None
    }
}

fn collect_worktree_files(root: &Path, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if out.len() >= limit {
            break;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if out.len() >= limit {
                break;
            }
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if matches!(
                s.as_ref(),
                ".git"
                    | "target"
                    | "node_modules"
                    | "dist"
                    | "build"
                    | ".build"
                    | ".cargo"
                    | ".venv"
                    | "__pycache__"
                    | ".next"
                    | ".nuxt"
                    | ".svelte-kit"
                    | ".output"
                    | ".turbo"
                    | ".cache"
                    | "coverage"
                    | "vendor"
            ) {
                continue;
            }
            let path = entry.path();
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file()
                && let Ok(rel) = path.strip_prefix(root)
            {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out
}

fn matches_any_glob(patterns: &[String], files: &[String]) -> bool {
    let mut builder = GlobSetBuilder::new();
    let mut valid = 0;
    for pat in patterns {
        if let Ok(g) = Glob::new(pat) {
            builder.add(g);
            valid += 1;
        }
    }
    if valid == 0 {
        return false;
    }
    let Ok(set) = builder.build() else {
        return false;
    };
    files.iter().any(|f| set.is_match(f))
}

pub fn count_by_scope(docs: &[Document]) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for d in docs {
        *counts.entry(d.scope.label()).or_insert(0) += 1;
    }
    counts
}
