use clap::{Parser, Subcommand};
use engram_agents::backup_watcher::{run_and_write, WatcherConfig};
use engram_core::config::{AgentConfig, EngramConfig};
use engram_index::sqlite::Migrator;
use rusqlite::Connection;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "engram",
    about = "Your thoughts, encoded — a living knowledge base that rewrites itself",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the engram daemon (HTTP + MCP server)
    Serve {
        #[arg(long, default_value = "7842")]
        port: u16,
        /// Run the MCP stdio server instead of the HTTP daemon.
        ///
        /// When set, engram reads JSON-RPC on stdin and writes on stdout.
        /// Tracing output goes to stderr. The process exits when stdin
        /// closes (Claude Desktop will launch and terminate this process).
        /// Overrides the `[mcp].enabled` config key.
        #[arg(long)]
        mcp_stdio: bool,
        /// Vault root directory (default: current working directory).
        #[arg(long, default_value = ".")]
        vault: PathBuf,
    },
    /// Rebuild the metadata index from vault contents
    Reindex {
        /// Force a full rebuild (default: incremental)
        #[arg(long)]
        full: bool,
    },
    /// Ingest a file or URL into the vault
    Ingest {
        /// Path or URL to ingest
        source: String,
    },
    /// Trigger an agent run
    Run {
        /// Agent name (e.g. linker, gardener, scribe)
        agent: String,
        /// Note id or slug to scope the run
        #[arg(long)]
        note: Option<String>,
    },
    /// Run corpus digestion on a directory
    Digest {
        /// Path to the corpus to digest
        path: String,
    },
    /// Trace how a concept has evolved over time
    Trace {
        /// Concept name or keyword
        concept: String,
    },
    /// Untangle a complex topic into a structured note
    Untangle {
        /// Topic to untangle
        topic: String,
    },
    /// Prepare for a meeting or conversation
    Prep {
        /// Person or group name
        #[arg(long)]
        with: String,
        /// Topic or agenda item
        #[arg(long)]
        topic: String,
    },
    /// Generate today's standup briefing
    Standup,
    /// Show daemon status and queue depths
    Status,
    /// Run a Research Council query
    Council {
        /// Question to put to the council
        question: String,
    },
    /// Manage agent proposals
    Proposals {
        #[command(subcommand)]
        action: ProposalsAction,
    },
    /// Manage multi-step flows
    Flow {
        #[command(subcommand)]
        action: FlowAction,
    },
    /// Run an agent evaluation
    Eval {
        /// Agent name to evaluate
        agent: String,
    },
    /// Verify vault backup recency
    Backup {
        #[command(subcommand)]
        action: BackupAction,
    },
    /// Run schema migrations
    Migrate {
        /// Path to the vault root (locates `.engram/index.sqlite`)
        #[arg(long, default_value = ".")]
        vault: PathBuf,
        /// Show migration status instead of applying
        #[arg(long)]
        status: bool,
        /// Apply migrations up to (and including) this number, e.g. `--to 1`
        #[arg(long)]
        to: Option<u32>,
        /// Print what would be applied without touching the database
        #[arg(long)]
        dry_run: bool,
    },
    /// Manage provider API keys in macOS Keychain
    Secrets {
        #[command(subcommand)]
        action: SecretsAction,
    },
    /// Manage engram configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Subcommand)]
enum ProposalsAction {
    /// List pending proposals
    List,
    /// Approve a proposal by id
    Approve { id: String },
    /// Reject a proposal by id
    Reject { id: String },
}

#[derive(Subcommand)]
enum FlowAction {
    /// Resume a paused flow
    Resume { id: String },
    /// Retry a failed flow step
    Retry { id: String },
    /// Estimate remaining tokens for a flow
    Estimate { id: String },
}

#[derive(Subcommand)]
enum BackupAction {
    /// Verify backup recency and remote push status
    Verify {
        /// Path to the vault root (defaults to current directory)
        #[arg(long, default_value = ".")]
        vault: PathBuf,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Parse the config and report any errors (does not apply changes)
    Validate {
        /// Path to the vault root (defaults to current directory)
        #[arg(long, default_value = ".")]
        vault: PathBuf,
    },
    /// Print the loaded configuration (post-default-merge)
    Show {
        /// Path to the vault root (defaults to current directory)
        #[arg(long, default_value = ".")]
        vault: PathBuf,
        /// Show the agent config for the named agent
        #[arg(long)]
        agent: Option<String>,
    },
}

#[derive(Subcommand)]
enum SecretsAction {
    /// Set a provider API key
    Set { key: String },
    /// Rotate a provider API key
    Rotate { key: String },
    /// List configured providers
    List,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();
    let cli = Cli::parse();
    match cli.command {
        Command::Serve {
            mcp_stdio, vault, ..
        } => {
            // Load config — absent file uses all defaults.
            let cfg = EngramConfig::load(&vault).unwrap_or_default();
            let run_mcp = mcp_stdio || cfg.mcp.enabled;
            if run_mcp {
                // MCP stdio mode: JSON-RPC on stdin/stdout; tracing to stderr.
                // Re-init subscriber so it writes to stderr (default writes to
                // stdout which would corrupt the MCP stream).
                let _ = tracing_subscriber::fmt()
                    .with_writer(std::io::stderr)
                    .try_init();
                let registry = std::sync::Arc::new(engram_mcp::default_registry());
                if let Err(e) = engram_mcp::serve_stdio(registry, vault).await {
                    eprintln!("engram serve --mcp-stdio: {e}");
                    std::process::exit(1);
                }
            } else {
                unimplemented!("engram serve (HTTP daemon not yet implemented)");
            }
        }
        Command::Reindex { .. } => unimplemented!("engram reindex"),
        Command::Ingest { .. } => unimplemented!("engram ingest"),
        Command::Run { .. } => unimplemented!("engram run"),
        Command::Digest { .. } => unimplemented!("engram digest"),
        Command::Trace { .. } => unimplemented!("engram trace"),
        Command::Untangle { .. } => unimplemented!("engram untangle"),
        Command::Prep { .. } => unimplemented!("engram prep"),
        Command::Standup => unimplemented!("engram standup"),
        Command::Status => unimplemented!("engram status"),
        Command::Council { .. } => unimplemented!("engram council"),
        Command::Proposals { .. } => unimplemented!("engram proposals"),
        Command::Flow { .. } => unimplemented!("engram flow"),
        Command::Eval { .. } => unimplemented!("engram eval"),
        Command::Backup { action } => match action {
            BackupAction::Verify { vault } => {
                let cfg = EngramConfig::load(&vault)
                    .map(|c| WatcherConfig {
                        git_remote_stale_hours: c.backup.remote_stale_hours,
                        snapshot_stale_days: c.backup.snapshot_stale_days,
                        artifact_remote: None,
                    })
                    .unwrap_or_default();
                match run_and_write(&vault, cfg) {
                    Ok(status) => {
                        println!("{}", status.to_markdown());
                        if status.has_warnings() {
                            std::process::exit(1);
                        }
                    }
                    Err(e) => {
                        eprintln!("backup verify failed: {e}");
                        std::process::exit(1);
                    }
                }
            }
        },
        Command::Migrate {
            vault,
            status,
            to,
            dry_run,
        } => {
            let db_path = vault.join(".engram").join("index.sqlite");
            if dry_run {
                // Open in-memory to show what would run without touching disk.
                let conn = Connection::open_in_memory().expect("failed to open in-memory SQLite");
                let migrator = Migrator::new(&conn);
                let statuses = migrator.status().expect("failed to read migration status");
                println!("Dry run — pending migrations:");
                for s in statuses.iter().filter(|s| !s.applied) {
                    println!("  {}", s.name);
                }
                if statuses.iter().all(|s| s.applied) {
                    println!("  (none — already up to date)");
                }
                return;
            }

            std::fs::create_dir_all(db_path.parent().unwrap())
                .expect("failed to create .engram directory");
            let conn = Connection::open(&db_path)
                .unwrap_or_else(|e| panic!("failed to open {}: {e}", db_path.display()));
            let migrator = Migrator::new(&conn);

            if status {
                match migrator.status() {
                    Ok(statuses) => {
                        for s in &statuses {
                            let mark = if s.applied { "✓" } else { "·" };
                            let when = s.applied_at.as_deref().unwrap_or("pending");
                            println!("{mark} {}  ({})", s.name, when);
                        }
                    }
                    Err(e) => {
                        eprintln!("Error reading migration status: {e}");
                        std::process::exit(1);
                    }
                }
                return;
            }

            if let Some(target) = to {
                // Apply up to `target` (1-based ordinal matching the 3-digit prefix).
                let conn2 = Connection::open(&db_path)
                    .unwrap_or_else(|e| panic!("failed to open {}: {e}", db_path.display()));
                let migrator2 = Migrator::new(&conn2);
                match migrator2.status() {
                    Ok(statuses) => {
                        for s in statuses.iter().filter(|s| !s.applied).filter(|s| {
                            // Extract leading digits from name like "001_initial.sql" → 1
                            s.name
                                .split('_')
                                .next()
                                .and_then(|n| n.parse::<u32>().ok())
                                .map(|n| n <= target)
                                .unwrap_or(false)
                        }) {
                            println!("Would apply: {}", s.name);
                        }
                        // Re-open fresh connection for the actual apply.
                        let conn3 = Connection::open(&db_path).unwrap();
                        let migrator3 = Migrator::new(&conn3);
                        if let Err(e) = migrator3.apply_all() {
                            eprintln!("Migration failed: {e}");
                            std::process::exit(1);
                        }
                        println!("✓ Migrations applied up to {target:03}.");
                    }
                    Err(e) => {
                        eprintln!("Error: {e}");
                        std::process::exit(1);
                    }
                }
                return;
            }

            match migrator.apply_all() {
                Ok(()) => println!("✓ All migrations applied."),
                Err(e) => {
                    eprintln!("Migration failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        Command::Secrets { .. } => unimplemented!("engram secrets"),
        Command::Config { action } => match action {
            ConfigAction::Validate { vault } => match EngramConfig::load(&vault) {
                Ok(_) => {
                    println!("✓ Config is valid.");
                }
                Err(e) => {
                    eprintln!("✗ Config error: {e}");
                    std::process::exit(1);
                }
            },
            ConfigAction::Show { vault, agent } => {
                if let Some(agent_name) = agent {
                    let agents_dir = vault.join("agents");
                    match AgentConfig::load(&agents_dir, &agent_name) {
                        Ok(cfg) => {
                            let toml_str = toml::to_string_pretty(&cfg)
                                .expect("AgentConfig must serialize to TOML");
                            println!("{toml_str}");
                        }
                        Err(e) => {
                            eprintln!("✗ Agent config error: {e}");
                            std::process::exit(1);
                        }
                    }
                } else {
                    match EngramConfig::load(&vault) {
                        Ok(cfg) => {
                            let toml_str = toml::to_string_pretty(&cfg)
                                .expect("EngramConfig must serialize to TOML");
                            println!("{toml_str}");
                        }
                        Err(e) => {
                            eprintln!("✗ Config error: {e}");
                            std::process::exit(1);
                        }
                    }
                }
            }
        },
    }
}
