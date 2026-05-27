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
        /// Agent name to evaluate (matches `.engram/evals/<agent>/cases/`).
        /// Required unless `--all` is set; ignored with `--all`.
        #[arg(required_unless_present = "all")]
        agent: Option<String>,
        /// Optional comma-separated case-id filter. Runs every case if absent.
        /// Incompatible with `--all`.
        #[arg(long, value_delimiter = ',', conflicts_with = "all")]
        cases: Option<Vec<String>>,
        /// Vault root containing `.engram/evals/<agent>/cases/` and
        /// `.engram/index.sqlite`. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        vault: PathBuf,
        /// Run every agent's eval suite under `<vault>/.engram/evals/`
        /// in turn. Mutually exclusive with `<agent>` and `--cases`.
        #[arg(long)]
        all: bool,
        /// Path to a JSON `Aggregate` baseline. After the run, the
        /// current aggregate is compared against it; any metric
        /// that regresses by more than `--regression-threshold`
        /// (default 5%) causes the command to exit non-zero.
        /// Per the spec's "PR cannot merge if any aggregate metric
        /// regresses by > 5%" CI gate.
        #[arg(long, conflicts_with = "all")]
        baseline: Option<PathBuf>,
        /// Regression tolerance as a fraction. Default `0.05` (5%).
        /// Has no effect without `--baseline`.
        #[arg(long, default_value = "0.05")]
        regression_threshold: f64,
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
    // Always send tracing output to stderr so it never corrupts stdout-based
    // transports (e.g. `engram serve --mcp-stdio` uses stdout for JSON-RPC).
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    match cli.command {
        Command::Serve {
            mcp_stdio, vault, ..
        } => {
            // Load config — absent file uses all defaults.
            let cfg = EngramConfig::load(&vault).unwrap_or_default();
            let run_mcp = mcp_stdio || cfg.mcp.enabled;
            if run_mcp {
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
        Command::Eval {
            agent,
            cases,
            vault,
            all,
            baseline,
            regression_threshold,
        } => {
            let dispatch = if all {
                run_eval_all(vault).await
            } else {
                // `agent` is `required_unless_present = "all"`, so clap
                // guarantees Some(_) here.
                let agent_name = agent.expect("clap enforces agent is Some when --all is unset");
                run_eval(agent_name, cases, vault, baseline, regression_threshold).await
            };
            if let Err(e) = dispatch {
                eprintln!("engram eval: {e}");
                std::process::exit(1);
            }
        }
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

// ─── `engram eval` implementation ──────────────────────────────────────────

/// Run the eval framework against `agent` at `vault`, optionally
/// filtering to a subset of `cases`.
///
/// This slice wires every layer together end-to-end against an
/// **EchoLlmProvider** that returns a canned low-confidence
/// response. Cases score deterministically against that fixed
/// output; the framework's wiring (SnapshotCache → EvalRunner →
/// scorer → aggregate → JSON write → DB persist → scorecard print)
/// is exercised on every invocation.
///
/// Production-provider wiring (Anthropic / OpenAI / Ollama
/// composed via the resilience stack) is a separate slice; the
/// AgentRunner here uses EchoLlmProvider as a deterministic
/// placeholder so operators can validate their case fixtures
/// before secrets are configured.
async fn run_eval(
    agent: String,
    cases: Option<Vec<String>>,
    vault: PathBuf,
    baseline: Option<PathBuf>,
    regression_threshold: f64,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::{Arc, Mutex};

    use engram_agents::{
        eval_adapter::agent_runner_invoker,
        locks::{LockConfig, LockManager},
        runner::AgentRunner,
    };
    use engram_eval::{EvalRunner, PersistParams, SnapshotCache};

    // Resolve paths under the vault.
    let agents_dir = vault.join("agents");
    let evals_root = vault.join(".engram").join("evals");
    let cases_dir = evals_root.join(&agent).join("cases");
    let runs_dir = evals_root.join(&agent).join("runs");
    let snapshots_dir = evals_root.join("snapshots");
    let db_path = vault.join(".engram").join("index.sqlite");

    if !cases_dir.is_dir() {
        return Err(format!("cases directory missing: {}", cases_dir.display()).into());
    }

    // Open + migrate SQLite.
    std::fs::create_dir_all(db_path.parent().unwrap())?;
    let conn = Connection::open(&db_path)?;
    Migrator::new(&conn).apply_all()?;
    let sqlite = Arc::new(Mutex::new(conn));

    // Select provider + model. Read EngramConfig and look at the
    // `fast` model tier (Linker/Scribe/Cartographer all use fast;
    // matches the per-agent config's `model_tier = "fast"` default).
    // Operators with a different tier need an explicit follow-up;
    // tier-selection-per-case is out of scope for this slice.
    let cfg = EngramConfig::load(&vault).unwrap_or_default();
    let model_entry = &cfg.models.fast;
    let (provider, model) = build_provider_and_model(model_entry, &vault)?;

    // Build AgentRunner with the chosen provider.
    let runner = Arc::new(AgentRunner::new(
        Arc::clone(&sqlite),
        provider,
        model,
        agents_dir,
        LockManager::new(
            Arc::clone(&sqlite),
            LockConfig {
                ttl_secs: 60,
                max_retries: 2,
                retry_base_ms: 5,
            },
        ),
        vault.clone(),
    ));

    let cache = SnapshotCache::new(snapshots_dir);
    let invoker = agent_runner_invoker(Arc::clone(&runner), agent.clone());
    let eval_runner = EvalRunner::new(&agent, &cases_dir, cache, invoker);

    let started_at = chrono::Utc::now().to_rfc3339();
    let report = match cases.as_deref() {
        Some(ids) => {
            let id_refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
            eval_runner.run_subset(&id_refs)?
        }
        None => eval_runner.run_all()?,
    };
    let completed_at = chrono::Utc::now().to_rfc3339();

    // Compute SHAs of the agent's prompt + config so eval_runs ties
    // each run to the exact prompt/config that produced it.
    fn sha_file(path: &PathBuf) -> String {
        use sha2::{Digest, Sha256};
        match std::fs::read(path) {
            Ok(bytes) => format!("{:x}", Sha256::digest(&bytes)),
            Err(_) => "missing".into(),
        }
    }
    let agent_prompt_sha = sha_file(&vault.join("agents").join(&agent).join("prompt.md"));
    let agent_config_sha = sha_file(&vault.join("agents").join(&agent).join("config.toml"));

    // Write JSON artifact first so persist() has the path to record.
    let json_path = report.write_json(&runs_dir)?;

    // Persist the eval_runs + eval_case_results rows.
    let total_tokens: i64 = 0; // total tokens aren't threaded into Observation yet (follow-up).
    let model_used = format!("{}/{}", model_entry.provider, model_entry.model);
    let params = PersistParams {
        agent_prompt_sha: &agent_prompt_sha,
        agent_config_sha: &agent_config_sha,
        model_used: &model_used,
        output_path: &json_path,
        started_at: &started_at,
        completed_at: &completed_at,
        total_tokens,
    };
    let run_id = {
        let mut conn = sqlite.lock().unwrap();
        report.persist(&mut conn, &params)?
    };

    // Print summary scorecard to stdout.
    let md = engram_eval::render_scorecard(&agent, &report.aggregate, &[]);
    println!("{md}");
    println!("eval_run_id: {run_id}");
    println!("artifact: {}", json_path.display());

    // Regression gate: compare current aggregate against a baseline
    // JSON file. The CI workflow that enforces "PR cannot merge if
    // any aggregate metric regresses by > 5%" reads the committed
    // baseline and passes its path on the command line.
    if let Some(baseline_path) = baseline {
        let body = std::fs::read_to_string(&baseline_path)?;
        let baseline_agg: engram_eval::Aggregate = serde_json::from_str(&body)?;
        let regs = report
            .aggregate
            .regressions_against(&baseline_agg, regression_threshold);
        if !regs.is_empty() {
            println!(
                "\n✗ regression check failed — {} metric(s) exceeded {:.0}% tolerance vs {}:",
                regs.len(),
                regression_threshold * 100.0,
                baseline_path.display()
            );
            for r in &regs {
                println!(
                    "    {}: baseline {:.4} → current {:.4} ({:+.1}%)",
                    r.metric,
                    r.baseline,
                    r.current,
                    r.delta_pct * 100.0
                );
            }
            return Err(format!("{} regression(s) past threshold", regs.len()).into());
        }
        println!(
            "\n✓ regression check passed (threshold {:.0}% vs {})",
            regression_threshold * 100.0,
            baseline_path.display()
        );
    }

    Ok(())
}

/// `engram eval --all` — enumerate every subdirectory under
/// `<vault>/.engram/evals/` that contains a `cases/` directory and
/// run its eval suite via [`run_eval`]. Agents are visited in
/// sorted order so the output is reproducible. A single agent's
/// failure does not abort the rest of the sweep — its error
/// surfaces in stderr and the loop continues; the process exits
/// non-zero at the end iff any agent failed.
async fn run_eval_all(vault: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let evals_root = vault.join(".engram").join("evals");
    if !evals_root.is_dir() {
        return Err(format!(
            "evals directory missing: {} (no agents to evaluate)",
            evals_root.display()
        )
        .into());
    }

    // Enumerate <evals_root>/<agent>/ that contain cases/ —
    // anything else (e.g. `snapshots/` under the same root) is
    // skipped. Sort by file name for reproducible output.
    let mut agents: Vec<String> = std::fs::read_dir(&evals_root)?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            // The `snapshots/` cache directory lives next to per-
            // agent directories and is NOT an agent.
            if path.file_name()?.to_str()? == "snapshots" {
                return None;
            }
            if !path.join("cases").is_dir() {
                return None;
            }
            path.file_name()?.to_str().map(String::from)
        })
        .collect();
    agents.sort();

    if agents.is_empty() {
        return Err(format!(
            "no agents found under {} (each agent needs a `cases/` subdirectory)",
            evals_root.display()
        )
        .into());
    }

    let mut failed: Vec<(String, String)> = Vec::new();
    for agent in &agents {
        println!("─── eval: {agent} ────────────────────────────────");
        if let Err(e) = run_eval(agent.clone(), None, vault.clone(), None, 0.05).await {
            eprintln!("engram eval {agent}: {e}");
            failed.push((agent.clone(), format!("{e}")));
        }
    }
    println!("─── summary ─────────────────────────────────────");
    println!(
        "agents run: {} / {} (failed: {})",
        agents.len() - failed.len(),
        agents.len(),
        failed.len()
    );
    if !failed.is_empty() {
        for (a, msg) in &failed {
            println!("  ✗ {a}: {msg}");
        }
        return Err(format!("{} agent(s) failed", failed.len()).into());
    }
    Ok(())
}

/// Resolve `ModelEntry { provider, model }` to a concrete
/// [`engram_llm::LlmProvider`] + [`engram_llm::Model`] pair.
///
/// Currently:
///   - `provider = "anthropic"`: builds `AnthropicProvider` against
///     the default base URL with secrets resolved via
///     `engram_secrets::store::open_default(<vault>/.engram)`.
///   - any other provider: falls back to an inline EchoLlmProvider
///     stub with a stderr warning. OpenAI + Ollama wiring lands in
///     future slices; until then the eval pipeline runs to
///     completion against the echo stub.
///
/// The full resilience stack (RetryProvider + TimeoutProvider +
/// CircuitBreakerProvider per ADR 0011) is intentionally NOT wired
/// here — that's a separate slice and the unwrapped provider gives
/// operators a clearer error when secrets aren't configured.
fn build_provider_and_model(
    entry: &engram_core::config::ModelEntry,
    vault: &std::path::Path,
) -> Result<
    (
        std::sync::Arc<dyn engram_llm::LlmProvider>,
        engram_llm::Model,
    ),
    Box<dyn std::error::Error>,
> {
    use async_trait::async_trait;
    use engram_llm::anthropic::AnthropicProvider;
    use engram_llm::{
        CompleteOptions, Completion, Cost, EmbeddingModel, LlmProvider, Model, ModelProvider,
        OllamaProvider, PromptStructured, StreamedCompletion, Usage,
    };
    use std::sync::Arc;

    /// Fallback provider for non-Anthropic configs (Ollama / OpenAI
    /// wiring isn't implemented yet). Mirrors the inline stub the
    /// CLI used before slice 18; lets the eval pipeline still
    /// complete end-to-end with a deterministic low-confidence
    /// response while we wait for the real provider wiring.
    struct EchoLlmProvider;
    #[async_trait]
    impl LlmProvider for EchoLlmProvider {
        async fn complete(
            &self,
            _: &PromptStructured,
            model: &Model,
            _: &CompleteOptions,
        ) -> engram_llm::Result<Completion> {
            Ok(Completion {
                text: r#"{"confidence":0.1,"kind":"echo","rationale":"non-anthropic provider wiring is a follow-up slice"}"#.into(),
                usage: Usage { input_tokens_total: 10, output_tokens: 10, ..Default::default() },
                cost: Cost {
                    input_cents: 0.0,
                    cache_create_cents: 0.0,
                    cache_read_cents: 0.0,
                    output_cents: 0.0,
                    total_cents: 0.0,
                },
                model_used: format!("echo/{}", model.name),
                latency_ms: 0,
            })
        }
        async fn complete_streamed(
            &self,
            _: &PromptStructured,
            _: &Model,
            _: &CompleteOptions,
        ) -> engram_llm::Result<StreamedCompletion> {
            Err(engram_llm::Error::Decode("echo: no streaming".into()))
        }
        async fn embed(&self, _: &str, _: &EmbeddingModel) -> engram_llm::Result<Vec<f32>> {
            Err(engram_llm::Error::Decode("echo: no embeddings".into()))
        }
    }

    match entry.provider.as_str() {
        "anthropic" => {
            let secrets = engram_secrets::open_default(&vault.join(".engram"))
                .map_err(|e| format!("secrets store init failed: {e}"))?;
            let provider =
                AnthropicProvider::new(Arc::new(secrets), AnthropicProvider::DEFAULT_BASE_URL)
                    .map_err(|e| format!("AnthropicProvider init failed: {e}"))?;
            Ok((
                Arc::new(provider),
                Model {
                    provider: ModelProvider::Anthropic,
                    name: entry.model.clone(),
                },
            ))
        }
        "ollama" => {
            // Ollama runs locally; no secrets store needed. Base
            // URL defaults to localhost:11434 (the daemon's stock
            // bind). A future EngramConfig field can let operators
            // override the host; for now we keep the wiring tight
            // by hardcoding the convention.
            let base_url = "http://localhost:11434";
            let provider = OllamaProvider::new(base_url)
                .map_err(|e| format!("OllamaProvider init failed: {e}"))?;
            Ok((
                Arc::new(provider),
                Model {
                    provider: ModelProvider::Ollama,
                    name: entry.model.clone(),
                },
            ))
        }
        other => {
            eprintln!(
                "warning: provider `{other}` wiring is not implemented yet — falling back to EchoLlmProvider"
            );
            Ok((
                Arc::new(EchoLlmProvider),
                Model {
                    provider: ModelProvider::Anthropic,
                    name: format!("echo/{}", entry.model),
                },
            ))
        }
    }
}
