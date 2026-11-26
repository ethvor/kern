#![forbid(unsafe_code)]

mod adopt;
mod builtin;
mod config;
mod doc;
mod guard;
mod mcp;
mod paginate;
mod render;
mod search;
mod servers;
mod setup;
mod sync;
mod update;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::config::{KernHome, ResolveOverrides, resolve_workspace};
use crate::doc::{Scope, Tier, compose, load_all_vault_docs, resolve_doc};
use crate::render::{render_bootstrap, render_list, render_read};
use crate::search::search_docs;

#[derive(Parser)]
#[command(
    name = "kern",
    version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("KERN_GIT_SHA"), ")"),
    about = "Portable context metaharness with bounded Markdown delivery and MCP support"
)]
struct Cli {
    /// Emit machine-readable JSON where supported
    #[arg(long, global = true)]
    json: bool,

    /// Operate on a named managed vault configured in ~/.kern/config.toml (e.g. --vault team)
    #[arg(long, global = true)]
    vault: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Emit compact routing index and bounded Must Read context
    Bootstrap {
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        mode: Option<String>,
        #[arg(long)]
        norm: Option<String>,
        #[arg(long)]
        user: Option<String>,
        /// Target harness (auto-detected from environment when omitted; e.g. codex, claude, antigravity)
        #[arg(long)]
        harness: Option<String>,
        /// Target model provider (e.g. openai, anthropic, google)
        #[arg(long)]
        provider: Option<String>,
        #[arg(long, default_value_t = 1)]
        page: usize,
    },
    /// Read one or more governed documents in bounded <= 7.5 KB pages
    Read {
        #[arg(required = true)]
        names: Vec<String>,
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        mode: Option<String>,
        #[arg(long)]
        norm: Option<String>,
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        harness: Option<String>,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long, default_value_t = 1)]
        page: usize,
        /// Print Markdown headings and section byte sizes instead of full body
        #[arg(long)]
        toc: bool,
        /// Extract a single Markdown section by heading
        #[arg(long)]
        section: Option<String>,
    },
    /// Search governed documents by keyword or regex
    Search {
        query: String,
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        harness: Option<String>,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        regex: bool,
        /// Search across the entire vault instead of just the active composed context
        #[arg(long)]
        all: bool,
    },
    /// List active composed documents
    List {
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        harness: Option<String>,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        tier: Option<String>,
    },
    /// Run the in-process stdio MCP server (4 tools: bootstrap, read, search, list)
    Mcp,
    /// Register `kern mcp` and compaction-only hooks across installed harnesses (Claude, Codex, Gemini, OpenCode)
    Setup {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        uninstall: bool,
    },
    /// Handle harness lifecycle hook JSON on stdin (compaction-only reminder; silent on normal startup)
    Hook,
    /// Manage external fail-closed Git identity & leakage guards (zero tracked files)
    Guard {
        #[command(subcommand)]
        action: GuardAction,
    },
    /// Pull latest, open a governed doc in $EDITOR by fuzzy name, and auto-sync + fan-out on save
    Edit {
        name: String,
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        project: Option<String>,
        /// Skip pre-edit pull and post-save sync
        #[arg(long)]
        no_sync: bool,
    },
    /// Print the filesystem path of a governed document by fuzzy name
    Path {
        name: String,
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        project: Option<String>,
    },
    /// Initialize ~/.kern for a user (personal vault, config, starter profile)
    Init {
        /// User name (defaults to $USER / $USERNAME / $LOGNAME)
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        remote: Option<String>,
        #[arg(long)]
        norm: Option<String>,
        #[arg(long)]
        force: bool,
    },
    /// Adopt a repository with .kern/kern.toml, .kern/BOOTSTRAP.md, and AGENTS/CLAUDE/GEMINI shims
    Adopt {
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        mode: Option<String>,
        #[arg(long)]
        norm: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Bind a worktree externally in ~/.kern/bindings.toml (writes zero repo files)
    Bind {
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        mode: Option<String>,
        #[arg(long)]
        norm: Option<String>,
        #[arg(long)]
        unbind: bool,
    },
    /// Copy a Markdown file into your vault under a chosen scope
    Add {
        file: PathBuf,
        #[arg(long, default_value = "norm")]
        scope: String,
        #[arg(long)]
        owner: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        tier: Option<String>,
        #[arg(long)]
        trigger: Option<String>,
        /// Commit, push, and nudge awake peers immediately after adding
        #[arg(long)]
        sync: bool,
    },
    /// Export a standalone copy-and-own doc, or push directly to a managed vault (--to <vault>)
    Share {
        name: String,
        #[arg(long)]
        out: Option<PathBuf>,
        /// Target managed vault name from config.toml (copies, commits, pushes, and nudges peer)
        #[arg(long)]
        to: Option<String>,
        #[arg(long, default_value = "norm")]
        scope: String,
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        project: Option<String>,
    },
    /// Event-driven Git sync + immediate parallel fan-out nudge to awake peers (or install wake hook)
    Sync {
        #[arg(short, long)]
        commit: Option<String>,
        #[arg(long)]
        pull_only: bool,
        /// Skip nudging awake sync_peers after push
        #[arg(long)]
        no_fanout: bool,
        /// Install OS wake-from-sleep hook (`kern sync --pull-only` on lid-open/wake)
        #[arg(long)]
        install_wake_hook: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Check for updates, manage update policy/targets, rebuild kern, and push to target devices
    Update {
        /// Check remote commit SHA and refresh local cache without building
        #[arg(long)]
        check_only: bool,
        /// Show current SHA, update policy, configured targets, and cached remote SHA
        #[arg(long)]
        status: bool,
        /// Update only this machine (do not push to update_targets)
        #[arg(long)]
        self_only: bool,
        /// Push current local kern binary / notify targets without rebuilding locally
        #[arg(long)]
        push_only: bool,
        /// Skip pushing to update_targets after local update
        #[arg(long)]
        no_push: bool,
        /// Suppress informational output
        #[arg(long)]
        quiet: bool,
        /// Set bootstrap update policy: auto | prompt | off | unset
        #[arg(long)]
        policy: Option<String>,
        /// Add a device alias to update_targets in ~/.kern/config.toml
        #[arg(long)]
        target_add: Option<String>,
        /// Remove a device alias from update_targets in ~/.kern/config.toml
        #[arg(long)]
        target_rm: Option<String>,
        /// Explicit target device(s) to push/notify for this run (repeatable)
        #[arg(long = "target")]
        targets: Vec<String>,
    },
    /// List or manage official project dev servers declared in kern.toml or vault project.toml
    Servers {
        #[command(subcommand)]
        action: Option<ServersAction>,
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        project: Option<String>,
        /// List declared servers across all vault projects, bindings, and configured project_roots
        #[arg(long)]
        all: bool,
    },
    /// Validate vault frontmatter, triggers, bindings, and git state
    Doctor {
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        harness: Option<String>,
        #[arg(long)]
        provider: Option<String>,
    },
}

#[derive(Subcommand)]
enum ServersAction {
    /// Register an official project server in kern.toml or ~/.kern/vault/projects/<project>/project.toml
    Add {
        /// Short server name unique within the project (e.g. preview, api, web)
        #[arg(long)]
        name: String,
        /// Local TCP port
        #[arg(long)]
        port: u16,
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        project: Option<String>,
        /// Local bind host (defaults to 127.0.0.1)
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Optional URL path (e.g. /docs)
        #[arg(long)]
        path: Option<String>,
        /// Optional local start command
        #[arg(long)]
        cmd: Option<String>,
        /// Optional description
        #[arg(long)]
        description: Option<String>,
        /// Write privately into ~/.kern/vault/projects/<project>/project.toml instead of kern.toml
        #[arg(long)]
        private: bool,
    },
    /// Remove a declared project server
    Rm {
        name: String,
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        project: Option<String>,
        /// Only remove from ~/.kern/vault/projects/<project>/project.toml
        #[arg(long)]
        private: bool,
    },
}

#[derive(Subcommand)]
enum GuardAction {
    /// Create a starter ~/.kern/guards/<project>.toml policy
    Init {
        #[arg(long)]
        project: String,
        /// Pre-populate neutral confidential identity (`Local Development`)
        #[arg(long)]
        confidential: bool,
    },
    /// Install fail-closed pre-commit & pre-push hooks in a repo's .git/hooks
    Install {
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        #[arg(long)]
        project: String,
        /// Back up and replace any existing non-kern git hooks (<stage>.kern-orig)
        #[arg(long)]
        force: bool,
    },
    /// Remove kern-managed git hooks (and restore <stage>.kern-orig backups if present)
    Uninstall {
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// Run the fail-closed guard check (invoked by git hooks or manually)
    Check {
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        #[arg(long)]
        project: String,
        #[arg(long, default_value = "pre-commit")]
        stage: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let base_home = KernHome::resolve()?;
    let home = match cli.vault.as_deref() {
        Some(name) => base_home.with_managed_vault(name)?,
        None => base_home,
    };

    match cli.command {
        Commands::Bootstrap {
            dir,
            project,
            mode,
            norm,
            user,
            harness,
            provider,
            page,
        } => {
            let cfg = home.load_config()?;
            let ws = resolve_workspace(
                &home,
                &cfg,
                ResolveOverrides {
                    dir: dir.as_deref(),
                    project: project.as_deref(),
                    mode: mode.as_deref(),
                    norm: norm.as_deref(),
                    user: user.as_deref(),
                    harness: harness.as_deref(),
                    provider: provider.as_deref(),
                },
            )?;
            let is_codex = ws.harness.as_deref() == Some("codex");
            let corpus = compose(&home, &cfg, ws)?;
            let mut notices = Vec::new();
            if is_codex
                && let Some(msg) =
                    setup::codex_hook_approval_notice(&setup::detect_unapproved_codex_kern_hooks())
            {
                notices.push(msg);
            }
            if let Some(msg) = update::bootstrap_update_notice(&home, &cfg) {
                notices.push(msg);
            }
            print!(
                "{}",
                render_bootstrap(&corpus, page, cli.json, cfg.page_limit(), &notices)?
            );
        }
        Commands::Read {
            names,
            dir,
            project,
            mode,
            norm,
            user,
            harness,
            provider,
            page,
            toc,
            section,
        } => {
            let cfg = home.load_config()?;
            let ws = resolve_workspace(
                &home,
                &cfg,
                ResolveOverrides {
                    dir: dir.as_deref(),
                    project: project.as_deref(),
                    mode: mode.as_deref(),
                    norm: norm.as_deref(),
                    user: user.as_deref(),
                    harness: harness.as_deref(),
                    provider: provider.as_deref(),
                },
            )?;
            let corpus = compose(&home, &cfg, ws)?;
            let mut resolved = Vec::with_capacity(names.len());
            for q in &names {
                resolved.push(resolve_doc(&corpus.docs, q)?);
            }
            print!(
                "{}",
                render_read(
                    &resolved,
                    page,
                    toc,
                    section.as_deref(),
                    cli.json,
                    cfg.page_limit()
                )?
            );
        }
        Commands::Search {
            query,
            dir,
            project,
            harness,
            provider,
            regex,
            all,
        } => {
            let cfg = home.load_config()?;
            let out = if all {
                let docs = load_all_vault_docs(&home)?;
                search_docs(&docs, &query, regex, cli.json, cfg.page_limit())?
            } else {
                let ws = resolve_workspace(
                    &home,
                    &cfg,
                    ResolveOverrides {
                        dir: dir.as_deref(),
                        project: project.as_deref(),
                        mode: None,
                        norm: None,
                        user: None,
                        harness: harness.as_deref(),
                        provider: provider.as_deref(),
                    },
                )?;
                let corpus = compose(&home, &cfg, ws)?;
                search_docs(&corpus.docs, &query, regex, cli.json, cfg.page_limit())?
            };
            print!("{out}");
        }
        Commands::List {
            dir,
            project,
            harness,
            provider,
            scope,
            tier,
        } => {
            let cfg = home.load_config()?;
            let ws = resolve_workspace(
                &home,
                &cfg,
                ResolveOverrides {
                    dir: dir.as_deref(),
                    project: project.as_deref(),
                    mode: None,
                    norm: None,
                    user: None,
                    harness: harness.as_deref(),
                    provider: provider.as_deref(),
                },
            )?;
            let corpus = compose(&home, &cfg, ws)?;
            let sf = scope.as_deref().map(Scope::parse).transpose()?;
            let tf = tier.as_deref().map(Tier::parse).transpose()?;
            print!("{}", render_list(&corpus, sf, tf, cli.json)?);
        }
        Commands::Mcp => mcp::serve_stdio(&home)?,
        Commands::Setup { dry_run, uninstall } => setup::cmd_setup(dry_run, uninstall)?,
        Commands::Hook => setup::cmd_hook()?,
        Commands::Guard { action } => match action {
            GuardAction::Init {
                project,
                confidential,
            } => guard::cmd_guard_init(&home, &project, confidential)?,
            GuardAction::Install {
                dir,
                project,
                force,
            } => guard::cmd_guard_install(&home, &dir, &project, force)?,
            GuardAction::Uninstall { dir } => guard::cmd_guard_uninstall(&dir)?,
            GuardAction::Check {
                dir,
                project,
                stage,
            } => guard::cmd_guard_check(&home, &dir, &project, &stage)?,
        },
        Commands::Edit {
            name,
            dir,
            project,
            no_sync,
        } => {
            let cfg = home.load_config()?;
            let ws = resolve_workspace(
                &home,
                &cfg,
                ResolveOverrides {
                    dir: dir.as_deref(),
                    project: project.as_deref(),
                    mode: None,
                    norm: None,
                    user: None,
                    harness: None,
                    provider: None,
                },
            )?;
            let corpus = compose(&home, &cfg, ws)?;
            adopt::cmd_edit(&home, &corpus, &name, no_sync)?;
        }
        Commands::Path { name, dir, project } => {
            let cfg = home.load_config()?;
            let ws = resolve_workspace(
                &home,
                &cfg,
                ResolveOverrides {
                    dir: dir.as_deref(),
                    project: project.as_deref(),
                    mode: None,
                    norm: None,
                    user: None,
                    harness: None,
                    provider: None,
                },
            )?;
            let corpus = compose(&home, &cfg, ws)?;
            adopt::cmd_path(&corpus, &name)?;
        }
        Commands::Init {
            user,
            remote,
            norm,
            force,
        } => adopt::cmd_init(
            &home,
            user.as_deref(),
            remote.as_deref(),
            norm.as_deref(),
            force,
        )?,
        Commands::Adopt {
            dir,
            project,
            mode,
            norm,
            dry_run,
        } => adopt::cmd_adopt(
            &home,
            &dir,
            project.as_deref(),
            mode.as_deref(),
            norm.as_deref(),
            dry_run,
        )?,
        Commands::Bind {
            dir,
            project,
            mode,
            norm,
            unbind,
        } => adopt::cmd_bind(
            &home,
            &dir,
            project.as_deref(),
            mode.as_deref(),
            norm.as_deref(),
            unbind,
        )?,
        Commands::Add {
            file,
            scope,
            owner,
            name,
            tier,
            trigger,
            sync,
        } => {
            let sc = Scope::parse(&scope)?;
            let ti = tier.as_deref().map(Tier::parse).transpose()?;
            adopt::cmd_add(
                &home,
                adopt::AddOptions {
                    file: &file,
                    scope: sc,
                    target_owner: owner.as_deref(),
                    rename: name.as_deref(),
                    tier: ti,
                    trigger: trigger.as_deref(),
                    auto_sync: sync,
                },
            )?;
        }
        Commands::Share {
            name,
            out,
            to,
            scope,
            dir,
            project,
        } => {
            let cfg = home.load_config()?;
            let ws = resolve_workspace(
                &home,
                &cfg,
                ResolveOverrides {
                    dir: dir.as_deref(),
                    project: project.as_deref(),
                    mode: None,
                    norm: None,
                    user: None,
                    harness: None,
                    provider: None,
                },
            )?;
            let corpus = compose(&home, &cfg, ws)?;
            let sc = Scope::parse(&scope)?;
            adopt::cmd_share(&home, &corpus, &name, out.as_deref(), to.as_deref(), sc)?;
        }
        Commands::Sync {
            commit,
            pull_only,
            no_fanout,
            install_wake_hook,
            dry_run,
        } => {
            if install_wake_hook {
                sync::cmd_install_wake_hook()?;
            } else {
                sync::cmd_sync(&home, commit.as_deref(), pull_only, no_fanout, dry_run)?;
            }
        }
        Commands::Update {
            check_only,
            status,
            self_only,
            push_only,
            no_push,
            quiet,
            policy,
            target_add,
            target_rm,
            targets,
        } => {
            update::cmd_update(
                &home,
                update::UpdateOptions {
                    check_only,
                    status,
                    self_only,
                    push_only,
                    no_push,
                    quiet,
                    policy,
                    target_add,
                    target_rm,
                    targets,
                },
            )?;
        }
        Commands::Servers {
            action,
            dir,
            project,
            all,
        } => {
            let cfg = home.load_config()?;
            match action {
                None => servers::cmd_servers_list(
                    &home,
                    &cfg,
                    dir.as_deref(),
                    project.as_deref(),
                    all,
                    cli.json,
                )?,
                Some(ServersAction::Add {
                    name,
                    port,
                    dir,
                    project,
                    host,
                    path,
                    cmd,
                    description,
                    private,
                }) => servers::cmd_servers_add(
                    &home,
                    &cfg,
                    servers::ServerAddOptions {
                        dir: dir.as_deref(),
                        project: project.as_deref(),
                        name: &name,
                        port,
                        host: &host,
                        path: path.as_deref(),
                        cmd: cmd.as_deref(),
                        description: description.as_deref(),
                        in_vault: private,
                    },
                )?,
                Some(ServersAction::Rm {
                    name,
                    dir,
                    project,
                    private,
                }) => servers::cmd_servers_rm(
                    &home,
                    &cfg,
                    &name,
                    dir.as_deref(),
                    project.as_deref(),
                    private,
                )?,
            }
        }
        Commands::Doctor {
            dir,
            project,
            harness,
            provider,
        } => {
            let cfg = home.load_config()?;
            let ws = resolve_workspace(
                &home,
                &cfg,
                ResolveOverrides {
                    dir: dir.as_deref(),
                    project: project.as_deref(),
                    mode: None,
                    norm: None,
                    user: None,
                    harness: harness.as_deref(),
                    provider: provider.as_deref(),
                },
            )?;
            let corpus = compose(&home, &cfg, ws)?;
            adopt::cmd_doctor(&home, &corpus, cli.json)?;
        }
    }

    Ok(())
}
