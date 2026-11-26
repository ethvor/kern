---
tier: must_read
priority: 50
---
# kern: Project Orientation

Single-crate Rust CLI and stdio MCP server (`src/`) implementing a portable context metaharness with bounded Markdown pagination, compaction-only hooks, multi-device vault sync, and external fail-closed Git guards.

## Layout

- `src/main.rs`: Clap CLI surface and subcommand dispatch (`#![forbid(unsafe_code)]`).
- `src/config.rs`: `~/.kern/config.toml`, `~/.kern/bindings.toml`, `.kern/kern.toml` (and root `kern.toml`) workspace resolution, and managed vault switching.
- `src/doc.rs`: Frontmatter parser, precedence layering (`core -> mode -> norm -> user -> <repo>/.kern/docs -> vault projects/<name>`), `paths:` glob matching, TOC and heading extraction.
- `src/paginate.rs`: Structural Markdown paginator (headings, blank-line paragraphs, code fences) enforcing `<= max_page_bytes` with SHA-256 page checksums and `NEXT REQUIRED` / `COMPLETE` footers.
- `src/render.rs`: Markdown and `--json` output renderers for `bootstrap`, `read`, and `list`.
- `src/search.rs`: Multi-token AND keyword search and regex search across composed docs or the full vault.
- `src/mcp.rs`: In-process JSON-RPC 2.0 stdio MCP server (`kern_bootstrap`, `kern_read`, `kern_search`, `kern_list`).
- `src/setup.rs`: Harness MCP registration (`Claude Code`, `Codex`, `Gemini CLI`, `OpenCode`) and compaction-only lifecycle hook handler (`kern hook`).
- `src/adopt.rs`: `init`, `adopt`, `bind`, `add`, `share`, `edit`, `path`, and `doctor` workflows.
- `src/guard.rs`: External `~/.kern/guards/<project>.toml` policies and fail-closed `.git/hooks/{pre-commit,pre-push}` enforcement (`init`, `install`, `uninstall`, `check`).
- `src/sync.rs`: Event-driven Git vault sync, parallel peer pull nudges (`remote_cmd`), and OS wake hooks.
- `src/update.rs`: Non-blocking cached update checks, policy controls (`auto | prompt | off`), atomic self-rebuilds, and peer binary push/notification.
- `src/servers.rs`: Official project dev server registry (`kern servers [--all] [add | rm]`) reading `kern.toml` and private vault `project.toml` with live localhost TCP probing.
- `src/builtin.rs`: Embedded `KERN_ORIENTATION`, harness shims, and starter templates.
- `scripts/build.rs`: Captures short Git SHA into `KERN_GIT_SHA`.
- `scripts/install.sh` / `scripts/install.ps1`: Standalone macOS/Linux/Windows installers.

## Build & Install

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo build --release
./scripts/install.sh
```

## Design Invariants

- **Safe Rust only:** `#![forbid(unsafe_code)]` with zero panicking `unwrap()` / `expect()` calls in runtime paths.
- **Bounded output:** Every `bootstrap` and `read` page stays within `max_page_bytes` (default `7500` B) and never splits inside fenced code blocks or orphans headings.
- **Atomic binary replacement:** Install scripts and `kern update` write `<bin>.new`, `chmod 755`, then `mv -f` to a fresh inode (required for macOS Apple Silicon code-signature verification, with `.old` self-rename on Windows).
- **Zero tracked footprint in stealth repos:** `kern bind` and `kern guard` never create tracked files inside the target worktree.
- **Self-contained docs:** Keep all documentation, CLI help, and comments free of private hostnames, personal infrastructure, or external tool coupling.
