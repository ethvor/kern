pub const BUILTIN_ORIENTATION_NAME: &str = "KERN_ORIENTATION";

pub const BUILTIN_ORIENTATION: &str = r#"---
tier: must_read
priority: 1
---
# kern Orientation

`kern` delivers your active project, mode, norm, and user context in bounded pages (`<= 7.5 KB` each) so no agent harness silently middle-truncates instructions.

## Operating Rules

1. **Read every `Must Read` page before acting.** If a bootstrap or document response ends with `NEXT:` or `NEXT REQUIRED:`, fetch the remaining page(s) before proceeding. Only `COMPLETE` indicates the full document/bootstrap has been received.
2. **Read `On Demand` docs when their trigger fires.** Use `kern read <NAME>` (or the `kern_read` MCP tool). Use `kern read <NAME> --toc` and `--section "<heading>"` for large docs. Use `kern search <query>` to locate rules across the store.
3. **Compaction resets readiness.** After context compaction or starting a fresh session, run `kern bootstrap` again before context-dependent work.
4. **Edit canonical docs in `~/.kern/vault/`** (or via `kern add` / `kern sync`). Do not duplicate governed standards into project repositories unless the project explicitly owns them.
5. **Preserve confidentiality.** Never expose private profiles, credentials, personal topology, or confidential project material in public or collaborator-visible surfaces.
"#;

pub const HARNESS_SHIM: &str = "Read and follow .kern/BOOTSTRAP.md.\n";
pub const ROOT_HARNESS_SHIM: &str = "Read and follow BOOTSTRAP.md.\n";

pub const MANAGED_START: &str = "<!-- kern:managed:start -->";
pub const MANAGED_END: &str = "<!-- kern:managed:end -->";
pub const LOCAL_START: &str = "<!-- kern:local:start -->";
pub const LOCAL_END: &str = "<!-- kern:local:end -->";

pub const DEFAULT_MANAGED_BOOTSTRAP: &str = r#"# Project Orientation

This repository uses `kern` for canonical context delivery.

1. Run `kern bootstrap` (or call the `kern_bootstrap` MCP tool) from anywhere inside this repository.
2. If the response ends with `NEXT REQUIRED: run 'kern bootstrap --page N'`, read all remaining pages until `COMPLETE`.
3. Fetch `On Demand` documents when their trigger applies using `kern read <NAME>` (or `kern_read`). Use `kern search <query>` to find relevant guidance.
4. After context compaction, run `kern bootstrap` again before context-dependent work.
"#;

pub fn starter_profile(user: &str) -> String {
    format!(
        r#"---
tier: must_read
priority: 10
---
# {user} - Collaboration Profile

## Preferences

- Address the user as **{user}**.
- Be concise, direct, and honest about what is verified vs. unverified.
- Prefer simple, maintainable implementations over speculative machinery.
"#
    )
}
