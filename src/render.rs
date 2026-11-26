use anyhow::Result;
use serde_json::json;

use crate::doc::{Corpus, Document, Scope, Tier, extract_section, extract_toc};
use crate::paginate::{paginate, render_page};

pub fn render_bootstrap(
    corpus: &Corpus,
    page: usize,
    as_json: bool,
    max_bytes: usize,
    notices: &[String],
) -> Result<String> {
    let must_read = corpus.must_read();
    let on_demand = corpus.on_demand();
    let reference = corpus.reference();

    let project_label = corpus
        .workspace
        .project
        .as_deref()
        .unwrap_or("(no project)");

    if as_json {
        let stream = build_bootstrap_markdown(corpus, &must_read, &on_demand, &reference, notices);
        let pages = paginate(&stream, max_bytes);
        if page == 0 || page > pages.len() {
            anyhow::bail!("page {page} out of range (1..={})", pages.len());
        }
        let p = &pages[page - 1];
        let val = json!({
            "project": corpus.workspace.project,
            "mode": corpus.workspace.mode,
            "norm": corpus.workspace.norm,
            "user": corpus.workspace.user,
            "harness": corpus.workspace.harness,
            "provider": corpus.workspace.provider,
            "root": corpus.workspace.root,
            "source": corpus.workspace.source.label(),
            "page": p.number,
            "total_pages": p.total,
            "complete": p.number == p.total,
            "bytes": [p.start_byte, p.end_byte, p.total_bytes],
            "sha256": p.sha256_short,
            "must_read": must_read.iter().map(|d| &d.name).collect::<Vec<_>>(),
            "on_demand": on_demand.iter().map(|d| json!({
                "name": d.name,
                "trigger": d.trigger,
                "matched_paths": d.matched_paths,
            })).collect::<Vec<_>>(),
            "reference": reference.iter().map(|d| &d.name).collect::<Vec<_>>(),
            "warnings": corpus.warnings,
            "notices": notices,
            "content": p.content,
        });
        return Ok(serde_json::to_string_pretty(&val)?);
    }

    let stream = build_bootstrap_markdown(corpus, &must_read, &on_demand, &reference, notices);
    let label = format!("bootstrap:{project_label}");
    let (rendered, _) = render_page(
        &stream,
        page,
        max_bytes,
        &label,
        |next| format!("kern bootstrap --page {next}"),
        true,
    )?;
    Ok(rendered)
}

fn build_bootstrap_markdown(
    corpus: &Corpus,
    must_read: &[&Document],
    on_demand: &[&Document],
    reference: &[&Document],
    notices: &[String],
) -> String {
    let mut out = String::new();
    let project_label = corpus.workspace.project.as_deref().unwrap_or("(global)");

    out.push_str(&format!("# kern bootstrap: {project_label}\n\n"));
    let harness_part = corpus
        .workspace
        .harness
        .as_deref()
        .map(|h| format!(" | harness: `{h}`"))
        .unwrap_or_default();
    let provider_part = corpus
        .workspace
        .provider
        .as_deref()
        .map(|p| format!(" | provider: `{p}`"))
        .unwrap_or_default();
    out.push_str(&format!(
        "project: `{project_label}` | mode: `{}` | norm: `{}` | user: `{}`{harness_part}{provider_part} | via: `{}`\n",
        corpus.workspace.mode,
        corpus.workspace.norm,
        corpus.workspace.user,
        corpus.workspace.source.label()
    ));
    if let Some(root) = &corpus.workspace.root {
        out.push_str(&format!("root: `{}`\n", root.display()));
    }
    if !corpus.warnings.is_empty() {
        out.push_str("\n> Warnings:\n");
        for w in &corpus.warnings {
            out.push_str(&format!("> - {w}\n"));
        }
    }
    for notice in notices {
        out.push('\n');
        out.push_str(notice.trim_end());
        out.push('\n');
    }

    // Compact routing index
    out.push_str("\n## Routing Index\n\n");
    out.push_str(&format!(
        "**Must Read (included below):** {}\n",
        must_read
            .iter()
            .map(|d| format!("`{}`", d.name))
            .collect::<Vec<_>>()
            .join(", ")
    ));

    if !on_demand.is_empty() {
        out.push_str("\n**On Demand** (`kern read <NAME>` when trigger applies):\n");
        for d in on_demand {
            let trig = d.trigger.as_deref().unwrap_or("(no trigger declared)");
            if d.matched_paths {
                out.push_str(&format!("- `{}` *[matches repo files]* - {trig}\n", d.name));
            } else {
                out.push_str(&format!("- `{}` - {trig}\n", d.name));
            }
        }
    }

    if !reference.is_empty() {
        out.push_str("\n**Reference** (`kern read <NAME>` or `kern search <query>`): ");
        out.push_str(
            &reference
                .iter()
                .map(|d| format!("`{}`", d.name))
                .collect::<Vec<_>>()
                .join(", "),
        );
        out.push('\n');
    }

    // Must Read full bodies
    for d in must_read {
        out.push_str(&format!(
            "\n---\n\n<!-- doc: {} ({}) -->\n\n{}\n",
            d.name,
            d.scope.label(),
            d.body
        ));
    }

    out
}

pub fn render_read(
    docs: &[&Document],
    page: usize,
    toc: bool,
    section: Option<&str>,
    as_json: bool,
    max_bytes: usize,
) -> Result<String> {
    if toc && let [doc] = docs {
        let entries = extract_toc(&doc.body);
        if as_json {
            return Ok(serde_json::to_string_pretty(&json!({
                "name": doc.name,
                "scope": doc.scope.label(),
                "tier": doc.tier.label(),
                "bytes": doc.bytes,
                "headings": entries,
            }))?);
        }
        let mut out = format!(
            "# TOC: {} ({}, {} bytes)\n\n",
            doc.name,
            doc.scope.label(),
            doc.bytes
        );
        for e in &entries {
            let indent = "  ".repeat(e.level.saturating_sub(1));
            out.push_str(&format!(
                "{indent}- {} ({} bytes, line {})\n",
                e.heading, e.bytes, e.line
            ));
        }
        out.push_str(&format!(
            "\nUse `kern read {} --section \"<heading>\"` to read one section.\n",
            doc.name
        ));
        return Ok(out);
    }

    let mut combined = String::new();
    let label = if let [doc] = docs {
        doc.name.clone()
    } else {
        docs.iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>()
            .join("+")
    };

    for (idx, doc) in docs.iter().enumerate() {
        if idx > 0 {
            combined.push_str("\n---\n\n");
        }
        let content = match section {
            Some(sec) => extract_section(&doc.body, sec)?,
            None => doc.body.clone(),
        };
        combined.push_str(&format!(
            "<!-- doc: {} | scope: {} | tier: {} -->\n\n{}\n",
            doc.name,
            doc.scope.label(),
            doc.tier.label(),
            content
        ));
    }

    if as_json {
        let pages = paginate(&combined, max_bytes);
        if page == 0 || page > pages.len() {
            anyhow::bail!("page {page} out of range (1..={})", pages.len());
        }
        let p = &pages[page - 1];
        return Ok(serde_json::to_string_pretty(&json!({
            "docs": docs.iter().map(|d| &d.name).collect::<Vec<_>>(),
            "section": section,
            "page": p.number,
            "total_pages": p.total,
            "complete": p.number == p.total,
            "bytes": [p.start_byte, p.end_byte, p.total_bytes],
            "sha256": p.sha256_short,
            "content": p.content,
        }))?);
    }

    let cmd_names = docs
        .iter()
        .map(|d| d.name.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let sec_flag = section
        .map(|s| format!(" --section {:?}", s))
        .unwrap_or_default();

    let (rendered, _) = render_page(
        &combined,
        page,
        max_bytes,
        &label,
        |next| format!("kern read {cmd_names}{sec_flag} --page {next}"),
        false,
    )?;
    Ok(rendered)
}

pub fn render_list(
    corpus: &Corpus,
    scope_filter: Option<Scope>,
    tier_filter: Option<Tier>,
    as_json: bool,
) -> Result<String> {
    let filtered: Vec<&Document> = corpus
        .docs
        .iter()
        .filter(|d| scope_filter.is_none_or(|s| d.scope == s))
        .filter(|d| tier_filter.is_none_or(|t| d.tier == t))
        .collect();

    if as_json {
        return Ok(serde_json::to_string_pretty(&filtered)?);
    }

    let harness_part = corpus
        .workspace
        .harness
        .as_deref()
        .map(|h| format!(" harness={h}"))
        .unwrap_or_default();
    let provider_part = corpus
        .workspace
        .provider
        .as_deref()
        .map(|p| format!(" provider={p}"))
        .unwrap_or_default();
    let mut out = format!(
        "# kern docs ({} total | project={} mode={} norm={} user={}{harness_part}{provider_part})\n\n",
        filtered.len(),
        corpus.workspace.project.as_deref().unwrap_or("(global)"),
        corpus.workspace.mode,
        corpus.workspace.norm,
        corpus.workspace.user
    );
    for tier in [Tier::MustRead, Tier::OnDemand, Tier::Reference] {
        let group: Vec<&&Document> = filtered.iter().filter(|d| d.tier == tier).collect();
        if group.is_empty() {
            continue;
        }
        out.push_str(&format!("## {} ({})\n", tier.label(), group.len()));
        for d in group {
            let trig = d
                .trigger
                .as_deref()
                .map(|t| format!(" - {t}"))
                .unwrap_or_default();
            out.push_str(&format!(
                "- `{}` [{} | {} B]{trig}\n",
                d.name,
                d.scope.label(),
                d.bytes
            ));
        }
        out.push('\n');
    }
    Ok(out)
}
