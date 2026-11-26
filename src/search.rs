use anyhow::{Context, Result};
use regex::RegexBuilder;
use serde::Serialize;
use serde_json::json;

use crate::doc::Document;

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub doc: String,
    pub scope: String,
    pub tier: String,
    pub heading: Option<String>,
    pub line: usize,
    pub snippet: String,
}

pub fn search_docs(
    docs: &[Document],
    query: &str,
    use_regex: bool,
    as_json: bool,
    max_bytes: usize,
) -> Result<String> {
    let query_trimmed = query.trim();
    if query_trimmed.is_empty() {
        anyhow::bail!("search query cannot be empty");
    }

    let regex = if use_regex {
        Some(
            RegexBuilder::new(query_trimmed)
                .case_insensitive(true)
                .build()
                .context("invalid search regex")?,
        )
    } else {
        None
    };
    let needle = query_trimmed.to_ascii_lowercase();
    let tokens: Vec<&str> = needle.split_whitespace().collect();

    let mut hits = Vec::new();
    for doc in docs {
        let name_lower = doc.name.to_ascii_lowercase();
        let trig_lower = doc
            .trigger
            .as_deref()
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();

        let body_lower = if regex.is_none() {
            Some(doc.body.to_ascii_lowercase())
        } else {
            None
        };

        if let Some(ref bl) = body_lower
            && tokens.len() > 1
        {
            let all_present = tokens
                .iter()
                .all(|t| name_lower.contains(t) || trig_lower.contains(t) || bl.contains(t));
            if !all_present {
                continue;
            }
        }

        let name_matches = match &regex {
            Some(re) => {
                re.is_match(&doc.name) || doc.trigger.as_deref().is_some_and(|t| re.is_match(t))
            }
            None if tokens.len() > 1 => tokens
                .iter()
                .all(|t| name_lower.contains(t) || trig_lower.contains(t)),
            None => name_lower.contains(&needle) || trig_lower.contains(&needle),
        };
        if name_matches {
            hits.push(SearchHit {
                doc: doc.name.clone(),
                scope: doc.scope.label().to_string(),
                tier: doc.tier.label().to_string(),
                heading: None,
                line: 0,
                snippet: doc
                    .trigger
                    .clone()
                    .unwrap_or_else(|| "(matched document name)".to_string()),
            });
        }

        let mut current_heading: Option<String> = None;
        let mut exact_line_hits = Vec::new();
        let mut partial_line_hits = Vec::new();
        let mut lower_lines = body_lower.as_deref().map(str::lines);
        for (idx, line) in doc.body.lines().enumerate() {
            let line_lower = lower_lines.as_mut().and_then(Iterator::next).unwrap_or("");
            let trimmed = line.trim();
            if trimmed.starts_with('#') && trimmed.chars().take_while(|&c| c == '#').count() <= 6 {
                let h = trimmed.trim_start_matches('#').trim();
                if !h.is_empty() {
                    current_heading = Some(h.to_string());
                }
            }
            let (is_exact, is_partial) = match &regex {
                Some(re) => (re.is_match(line), false),
                None if tokens.len() > 1 => {
                    let all = tokens.iter().all(|t| line_lower.contains(t));
                    let any = !all && tokens.iter().any(|t| line_lower.contains(t));
                    (all, any)
                }
                None => (line_lower.contains(&needle), false),
            };
            if is_exact && exact_line_hits.len() < 4 {
                exact_line_hits.push(SearchHit {
                    doc: doc.name.clone(),
                    scope: doc.scope.label().to_string(),
                    tier: doc.tier.label().to_string(),
                    heading: current_heading.clone(),
                    line: idx + 1,
                    snippet: truncate_snippet(trimmed, 140),
                });
            } else if is_partial && partial_line_hits.len() < 4 {
                partial_line_hits.push(SearchHit {
                    doc: doc.name.clone(),
                    scope: doc.scope.label().to_string(),
                    tier: doc.tier.label().to_string(),
                    heading: current_heading.clone(),
                    line: idx + 1,
                    snippet: truncate_snippet(trimmed, 140),
                });
            }
        }
        let chosen = if !exact_line_hits.is_empty() {
            exact_line_hits
        } else {
            partial_line_hits
        };
        hits.extend(chosen);
    }

    if as_json {
        return Ok(serde_json::to_string_pretty(&json!({
            "query": query_trimmed,
            "total_hits": hits.len(),
            "hits": hits,
        }))?);
    }

    if hits.is_empty() {
        return Ok(format!("No matches for `{query_trimmed}`.\n"));
    }

    let mut out = format!("# kern search: `{query_trimmed}` ({} hits)\n\n", hits.len());
    let budget = max_bytes.saturating_sub(140).max(512);
    let mut shown = 0usize;

    for hit in &hits {
        let loc = match (&hit.heading, hit.line) {
            (_, 0) => "metadata".to_string(),
            (Some(h), ln) => format!("`{h}` L{ln}"),
            (None, ln) => format!("L{ln}"),
        };
        let entry = format!(
            "- **{}** [{} | {}] ({loc}): {}\n",
            hit.doc, hit.scope, hit.tier, hit.snippet
        );
        if out.len() + entry.len() > budget {
            break;
        }
        out.push_str(&entry);
        shown += 1;
    }

    if shown < hits.len() {
        out.push_str(&format!(
            "\n(Showing {shown} of {} hits; narrow query or run `kern read <DOC>`)\n",
            hits.len()
        ));
    } else {
        out.push_str("\nUse `kern read <DOC>` (or `--section \"<heading>\"`) to read a match.\n");
    }
    Ok(out)
}

fn truncate_snippet(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let collected: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{collected}...")
    } else {
        collected
    }
}
