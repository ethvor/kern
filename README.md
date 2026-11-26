# kern

**Bring your own context to any coding harness.**

kern is a lightweight context metaharness that sits above AI coding harnesses such as Codex, Claude Code, Antigravity, or any shell-capable agent.

Instead of duplicating instructions across .md files in dozens of projects, kern composes context across project types and users, with specific overrides per project. It feeds this context to your agent in readable, truncation-proof chunks through MCP or via CLI.

---

## Switch harnesses easily

Your harness no longer has an excuse to tie you down. Making a switch to try a new model has never been easier. Because `AGENTS.md`, `GEMINI.md`, `CLAUDE.md`, and so on funnel agent context into the `kern` bootstrap, starting up a project in any coding harness is as simple as downloading the new harness, adding the project as usual, and chatting. No special instructions or porting are necessary in any way.

---

## Example

I have worked across personal repos and confidential work repositories. For different tasks, I often switch between Claude Code, Codex, and Antigravity.

With `kern`, your context lives in layers instead of copies in each repo:

```text
1. built-in core  (KERN_ORIENTATION)
2. core           ~/.kern/vault/core/
3. mode           ~/.kern/vault/modes/<mode>/
4. norm           ~/.kern/vault/norms/<norm>/
5. user           ~/.kern/vault/users/<user>/
6. repo-local     <repo>/.kern/docs/             (optional; committed with the repo)
7. project vault  ~/.kern/vault/projects/<name>/ (optional; private per-repo notes/overrides)
```

Each layer in that stack has a specific job (lower layers override higher ones when names collide):

**built-in core (`KERN_ORIENTATION`):** Bundled inside the `kern` binary so agents know how `kern` works even in an empty vault.  
*Example:* An agent immediately knows how to use `kern bootstrap`, open specific sections with `kern read`, and query docs with `kern search`.

**core (`~/.kern/vault/core/`):** Universal rules that apply to every repo regardless of workflow / team.  
*Example:* A rule to never commit secrets and to always verify before stopping or telling the user that work is complete.

**mode (`~/.kern/vault/modes/<mode>/`):** Environmental instructions that change how strict the process is (`standard`, `work`, `research`).  
*Example:* A `work` mode adds strict security and authentication while `standard` keeps personal projects fast with less ceremony.

**norm (`~/.kern/vault/norms/<norm>/`):** A shared ruleset or style profile. Personal repos and team repos can follow different conventions.  
*Example:* Your personal repos inherit your own coding and writing conventions while company repos use a company standard.

**Why `mode` and `norm` are separate (composition):** `mode` sets the repository's environment and safety/data-sensitivity boundary (e.g. `standard`, `confidential`, `corporate-stealth`), while `norm` sets whose coding and writing conventions apply (`personal`, `acme-corp`, `student`). Keeping them independent lets projects choose a `norm` and `mode` instead of duplicating rules and documents for every combination:

- `cs-major-jumper-game` (`mode = "standard"`, `norm = "student"`): Your class coding standards (from lecture slides) and writing conventions (like MLA) in a regular repo with `.kern/` committed.
- `billing-service` (`mode = "corporate-stealth"`, `norm = "acme-corp"`): Follows company conventions while keeping zero `.kern/` files in Git via `kern bind` and enforcing work Git identity via `kern guard`.
- `work-agentic-design-scratchpad` (`mode = "corporate-stealth"`, `norm = "personal"`): Uses the same zero-footprint safety boundary your work requires for work-related content, while keeping the personal engineering and debugging standards you prefer since the repo is a scratchpad your boss won't see outside a lawsuit.
- `stealth-saas` (`mode = "confidential"`, `norm = "personal"`): A secret project operating with high leakage protection, while following your preferred personal agent standards.

**user (`~/.kern/vault/users/<user>/`):** Your personal profile and working preferences that follow you into every project.  
*Example:* How you want agents to address you, format answers, or structure plans. Multiple users can share the same norms and modes if granted, so a repo's final context is composed per `user` x `norm` x `mode`.

**repo-local (`<repo>/.kern/docs/`):** Project documentation committed right inside the repo so teammates and agents share the same architecture notes.  
*Example:* A `PROJECT_ORIENTATION` doc covering the folder layout, build commands, and corrections related to this project for agentic context.

**project vault (`~/.kern/vault/projects/<name>/`):** Private per-repo notes and overrides kept in your vault and never committed to Git. For client or confidential repos where you cannot add `.kern/` files, `kern bind` maps the repo externally with zero tracked files.  
*Example:* Local staging URLs, personal scratch notes, or private work-repo overrides.

Layers control **where** a document lives. Inside any layer, a short metadata block at the top of each Markdown file (YAML frontmatter, between `---` lines, shown in Schemas below) controls **when** it loads and **which tools** see it:

**`must_read`:** Loads immediately on `kern bootstrap` in bounded `<= 7.5 KB` pages.  
*Example:* Your user profile or core session directives that every session needs upfront.

**`on_demand`:** Indexed at bootstrap with a one-line trigger (and optional `paths:` globs), so agents open it only when the task matches.  
*Example:* `WRITING_STANDARDS` gets read when drafting prose or READMEs and is skipped during coding.

**`reference`:** Listed by name only at bootstrap, without a read trigger description taking up context. Agents open specific parts with `kern read --toc`, `--section`, or `kern search` only when looking something up.  
*Example:* A long API table, schema catalog, or architecture manual.

**`harnesses:` / `providers:`:** Restricts a doc to specific coding tools (`claude`, `codex`, `antigravity`, `gemini`, `opencode`) or model providers (`anthropic`, `openai`, `google`).  
*Example:* If you never want Claude Code to use its ask-user-question tool in any project, put that rule in a doc tagged `harnesses: [claude]`.

---

## How `kern` Compares

| Capability | Raw `AGENTS.md` / `CLAUDE.md` | Static Rule Generators (`ruler`, `rulesync`) | `kern` |
|---|---|---|---|
| **Single source across harnesses** | Copy-paste per tool | Generates tool-specific files per repo | Live CLI + MCP metaharness (one-line shim) |
| **Layered overrides (`mode`/`norm`/`user`/`project`)** | Manual | Flat or per-repo templates | Built-in 7-layer precedence engine |
| **Per-harness / per-provider filtering** | Separate files | Separate templates | Frontmatter `harnesses:` / `providers:` with auto-detection |
| **Bounded delivery (`<= 7.5 KB` pages)** | Truncation risk | Truncation risk | Structural Markdown pagination + SHA-256 |
| **Lazy `On Demand` triggers + `paths:` globs** | Loads everything up front | Harness-dependent | Uniform routing index across all harnesses |
| **Section-level reading & vault search** | Full-file reads | Full-file reads | `--toc`, `--section`, multi-token & regex search |
| **Zero-footprint work/stealth repos** | Must commit or `.git/info/exclude` | Writes generated files in repo | External `kern bind` + `kern guard` (zero files) |
| **Post-compaction recovery** | Manual | None | Compaction-only `PostCompact` / `SessionStart` hook |

---

## Schemas & Configuration

### Project Manifest (`.kern/kern.toml` or root `kern.toml`)

```toml
project = "example-app"
# Optional overrides (otherwise inherited from ~/.kern/config.toml):
# mode = "standard"
# norm = "ethvor"
harness_files = ["AGENTS.md", "CLAUDE.md", "GEMINI.md"]
```

### Document Frontmatter

Every `.md` file in `~/.kern/vault/` or `<repo>/.kern/docs/` supports optional YAML frontmatter:

```markdown
---
name: RUST_STANDARDS
tier: on_demand          # must_read | on_demand | reference (default: reference)
trigger: "Editing or reviewing Rust crates or Cargo.toml"
paths: ["Cargo.toml", "**/*.rs"]
harnesses: [codex, claude]  # optional; omit to include in all harnesses
providers: [openai]         # optional; omit to include for all providers
priority: 30                # optional integer; lower sorts first in must_read
---

# Rust Standards
...
```

- **`name`**: Defaults to the filename stem. Lookup in `kern read` / `kern edit` / `kern path` is case-insensitive and fuzzy (ignores punctuation, supports unambiguous prefixes/substrings).
- **`tier`**: `must_read` (streamed at bootstrap), `on_demand` (indexed with `trigger` at bootstrap), or `reference` (listed by name only until read/searched).
- **`trigger`**: One-line description telling agents *when* to fetch an `on_demand` doc (alias: `description`). Checked by `kern doctor`.
- **`paths`**: Optional glob list (`globset`). If any file in the repo matches, `kern bootstrap` highlights the doc with `*[matches repo files]*`.
- **`harnesses` / `providers`**: Optional audience filters. When present, the document is included only when the active harness (auto-detected from runtime env or MCP `clientInfo`, or set via `--harness`) or `--provider` matches.
- **`priority`**: Controls ordering inside `must_read` (defaults: `core=10`, `mode=20`, `norm=30`, `user=40`, `project=50`).

Files or directories starting with `.` or `_` inside scope folders are ignored.

### Global Configuration (`~/.kern/config.toml`)

```toml
user = "ethvor"
default_mode = "standard"
default_norm = "ethvor"
max_page_bytes = 7500                 # clamped to 2048..=32768

# Optional multi-device vault sync fan-out (SSH host aliases)
sync_peers = ["laptop", "devbox"]
# remote_cmd = "ssh"                  # optional custom remote runner prefix
# remote_copy_cmd = ""                # optional custom copy command prefix for binary push

# Optional update controls
update_policy = "prompt"              # auto | prompt | off
# update_repo = "https://github.com/ethvor/kern.git"
# update_source_dir = "~/dev/kern"
# update_targets = ["laptop"]

# Optional additional vaults you maintain or share into (`kern --vault team ...`, `kern share --to team`)
[managed_vaults.team]
path = "~/.kern/vaults/team"
user = "team"
peer = "team-box"
```

Set `KERN_HOME` to override `~/.kern` for isolated testing or multiple environments.

---

## CLI & MCP Reference

### CLI Commands

| Command | Description |
|---|---|
| `kern bootstrap [--dir] [--project] [--mode] [--norm] [--harness] [--provider] [--page N]` | Emit routing index and bounded `Must Read` pages |
| `kern read <NAME...> [--toc] [--section "<H>"] [--harness] [--page N]` | Read one or more docs (or inspect TOC / single heading) |
| `kern search <query> [--regex] [--all] [--harness]` | Multi-token AND or regex search across composed docs or whole vault |
| `kern list [--scope] [--tier] [--harness]` | List active composed documents |
| `kern mcp` | Run stdio JSON-RPC 2.0 MCP server |
| `kern setup [--dry-run] [--uninstall]` | Register MCP + compaction hooks across detected local harnesses |
| `kern hook` | Handle harness compaction hook JSON on stdin |
| `kern init [--user <name>] [--remote <url>] [--norm <name>]` | Initialize `~/.kern` config and Git-backed vault (defaults to `$USER`) |
| `kern adopt [--dir] [--project] [--mode] [--norm] [--dry-run]` | Add `.kern/kern.toml`, `.kern/BOOTSTRAP.md`, and `AGENTS`/`CLAUDE`/`GEMINI` shims |
| `kern bind [--dir] --project <name> [--unbind]` | Bind a worktree externally in `~/.kern/bindings.toml` (zero repo files) |
| `kern add <file> --scope <scope> [--tier] [--trigger] [--sync]` | Import a Markdown file into the vault |
| `kern edit <NAME> [--no-sync]` | Pull latest, open doc in `$VISUAL`/`$EDITOR`, auto-sync on save |
| `kern path <NAME>` | Print resolved filesystem path for a doc |
| `kern share <NAME> [--out <file> \| --to <vault>]` | Export standalone Markdown or push to a managed vault |
| `kern sync [--commit "msg"] [--pull-only] [--install-wake-hook]` | Sync vault via Git and nudge awake peers |
| `kern guard init \| install [--force] \| uninstall \| check` | Manage external fail-closed Git identity & leakage guards |
| `kern servers [add \| rm] [--all]` | List or manage declared project dev servers |
| `kern update [--status \| --check-only \| --policy <p> \| --target <peer>]` | Check updates, rebuild `kern`, and push/notify peer devices |
| `kern doctor [--harness] [--json]` | Validate frontmatter, triggers, `Must Read` size budget, bindings, hooks, and vault Git state |

Global flags: `--json` (machine-readable output where supported), `--vault <name>` (operate on a configured managed vault).

### MCP Tools (`kern mcp`)

- `kern_bootstrap`: `{ dir?, project?, mode?, norm?, user?, harness?, provider?, page? }`
- `kern_read`: `{ name?, names?, dir?, project?, mode?, norm?, user?, harness?, provider?, page?, toc?, section? }`
- `kern_search`: `{ query, dir?, project?, harness?, provider?, regex?, all? }`
- `kern_list`: `{ dir?, project?, harness?, provider?, scope?, tier? }`

---

## Setup

Requires Rust `1.88+` (`cargo`).

### Setup with an agent

Paste this prompt into any coding agent to handle installation and initial setup for you:

```text
Set up kern (https://github.com/ethvor/kern) on this machine:
1. Check that Rust 1.88+ (cargo) is installed.
2. Clone https://github.com/ethvor/kern.git and install the release binary using ./scripts/install.sh (macOS/Linux), .\scripts\install.ps1 (Windows PowerShell), or `cargo install --path kern`.
3. Ask me whether I want a local-only vault or to attach a private Git remote for multi-device sync, then run `kern init` (with `--remote <url>` if provided).
4. Run `kern setup` to register the `kern` MCP server and compaction hooks across my installed harnesses (note any manual hook-approval step if Codex is installed).
5. Ask which repository I want to adopt first (or whether to use `kern bind` for a zero-file work repo), adopt or bind it, and verify everything with `kern doctor` and `kern bootstrap`.
```

### Manual setup

#### 1. Install

**macOS / Linux:**

```bash
git clone https://github.com/ethvor/kern.git
cd kern && ./scripts/install.sh
```

**Windows (PowerShell):**

```powershell
git clone https://github.com/ethvor/kern.git
cd kern
.\scripts\install.ps1
```

**Any OS (Cargo):**

```bash
git clone https://github.com/ethvor/kern.git
cargo install --path kern
```

#### 2. Initialize your vault and register harnesses

```bash
kern init
kern setup
```

`kern init` defaults to `$USER` (override with `--user <name>`). Pass `--remote git@github.com:<you>/kern-vault.git` if you want to sync your vault across devices.

#### 3. Adopt a repository

From inside any Git repository:

```bash
kern adopt
kern bootstrap
```

`kern adopt` infers the project name from the folder name (override with `--project <name>`). For work or shared repos where you cannot commit files, run `kern bind --project <name>` instead.

---

## License

MIT, see [LICENSE](LICENSE).
