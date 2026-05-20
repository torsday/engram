use clap::{Parser, Subcommand};
use engram_core::config::{AgentConfig, EngramConfig};
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
    Migrate,
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
    Verify,
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
        Command::Serve { .. } => unimplemented!("engram serve"),
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
        Command::Backup { .. } => unimplemented!("engram backup"),
        Command::Migrate => unimplemented!("engram migrate"),
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
