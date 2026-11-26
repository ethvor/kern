use anyhow::{Result, bail};
use sha2::{Digest, Sha256};

use crate::doc::fence_marker;

const FOOTER_RESERVE: usize = 160;

#[derive(Debug, Clone)]
pub struct Page {
    pub number: usize,
    pub total: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub total_bytes: usize,
    pub content: String,
    pub sha256_short: String,
}

/// Paginates Markdown content on clean structural boundaries (headings, blank-line
/// paragraphs, fenced code blocks) so every emitted page + footer stays <= `max_bytes`.
pub fn paginate(input: &str, max_bytes: usize) -> Vec<Page> {
    let budget = max_bytes.saturating_sub(FOOTER_RESERVE).max(512);
    let total_bytes = input.len();
    if total_bytes == 0 {
        return vec![Page {
            number: 1,
            total: 1,
            start_byte: 0,
            end_byte: 0,
            total_bytes: 0,
            content: String::new(),
            sha256_short: short_sha256(""),
        }];
    }

    let raw_blocks = split_markdown_blocks(input);
    let mut blocks: Vec<(usize, usize)> = Vec::with_capacity(raw_blocks.len());
    for (start, end) in raw_blocks {
        if end - start <= budget {
            blocks.push((start, end));
        } else {
            // Split an oversized single block on line boundaries (or UTF-8 char boundary)
            split_oversized(input, start, end, budget, &mut blocks);
        }
    }

    let Some(&(mut cur_start, mut cur_end)) = blocks.first() else {
        return vec![Page {
            number: 1,
            total: 1,
            start_byte: 0,
            end_byte: 0,
            total_bytes,
            content: String::new(),
            sha256_short: short_sha256(""),
        }];
    };

    let mut spans: Vec<(usize, usize)> = Vec::new();
    for &(start, end) in &blocks[1..] {
        if end - cur_start <= budget {
            cur_end = end;
        } else {
            spans.push((cur_start, cur_end));
            cur_start = start;
            cur_end = end;
        }
    }
    spans.push((cur_start, cur_end));

    let total = spans.len();
    spans
        .into_iter()
        .enumerate()
        .map(|(idx, (s, e))| {
            let slice = &input[s..e];
            Page {
                number: idx + 1,
                total,
                start_byte: s,
                end_byte: e,
                total_bytes,
                content: slice.to_string(),
                sha256_short: short_sha256(slice),
            }
        })
        .collect()
}

pub fn render_page(
    input: &str,
    page_num: usize,
    max_bytes: usize,
    label: &str,
    next_cmd: impl Fn(usize) -> String,
    required_next: bool,
) -> Result<(String, Page)> {
    let pages = paginate(input, max_bytes);
    if page_num == 0 || page_num > pages.len() {
        bail!(
            "requested page {page_num} is out of range (valid: 1..={})",
            pages.len()
        );
    }
    let page = pages[page_num - 1].clone();
    let status = if page.number < page.total {
        let prefix = if required_next {
            "NEXT REQUIRED"
        } else {
            "NEXT"
        };
        format!("{prefix}: run `{}`", next_cmd(page.number + 1))
    } else {
        "COMPLETE".to_string()
    };

    let body = page.content.trim_end();
    let rendered = format!(
        "{body}\n\n---\n[kern | {label} | page {}/{} | bytes {}..{}/{} | sha256={} | {status}]\n",
        page.number,
        page.total,
        page.start_byte,
        page.end_byte,
        page.total_bytes,
        page.sha256_short
    );
    Ok((rendered, page))
}

fn split_markdown_blocks(input: &str) -> Vec<(usize, usize)> {
    let mut blocks = Vec::new();
    let mut in_fence = false;
    let mut fence_char = '`';
    let mut block_start = 0usize;
    let mut offset = 0usize;
    let mut block_has_body = false;

    for line in input.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let line_start = offset;
        offset += line.len();

        if let Some(fc) = fence_marker(trimmed) {
            if !in_fence {
                in_fence = true;
                fence_char = fc;
            } else if fc == fence_char {
                in_fence = false;
            }
            block_has_body = true;
            continue;
        }
        if in_fence {
            block_has_body = true;
            continue;
        }

        // Start a new block right before a heading or horizontal rule (when prior block has body)
        let is_heading = trimmed.starts_with('#')
            && trimmed.bytes().take_while(|&b| b == b'#').count() <= 6
            && trimmed
                .as_bytes()
                .get(trimmed.bytes().take_while(|&b| b == b'#').count())
                .is_some_and(|&b| b == b' ' || b == b'\t');
        if is_heading && block_has_body && line_start > block_start {
            blocks.push((block_start, line_start));
            block_start = line_start;
            block_has_body = false;
            continue;
        }

        // Split after blank lines ONLY once the current block contains non-heading body text,
        // keeping headings attached to their following paragraph/list so headings never orphan.
        if trimmed.is_empty() {
            if block_has_body && offset > block_start {
                blocks.push((block_start, offset));
                block_start = offset;
                block_has_body = false;
            }
            continue;
        }

        if !is_heading && !trimmed.starts_with("<!--") && trimmed != "---" {
            block_has_body = true;
        }
    }

    if block_start < input.len() {
        blocks.push((block_start, input.len()));
    }
    blocks
}

fn split_oversized(
    input: &str,
    mut start: usize,
    end: usize,
    budget: usize,
    out: &mut Vec<(usize, usize)>,
) {
    while end - start > budget {
        let limit = floor_char_boundary(input, start + budget);
        let chunk = &input[start..limit];
        let cut = chunk
            .rfind('\n')
            .filter(|&pos| pos > 0)
            .map(|pos| start + pos + 1)
            .unwrap_or(limit);
        let cut = if cut <= start { limit } else { cut };
        if cut <= start {
            break;
        }
        out.push((start, cut));
        start = cut;
    }
    if start < end {
        out.push((start, end));
    }
}

fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

pub fn short_sha256(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let digest = hasher.finalize();
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3]
    )
}
