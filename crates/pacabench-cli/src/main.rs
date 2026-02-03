//! CLI for PacaBench - a local-first benchmark harness for LLM agents.

mod formatting;
mod init;
mod pricing;
mod progress;
mod tui;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use formatting::{
    build_export_json_from_stats, build_export_markdown_from_stats, print_cases, print_run_list,
    print_run_stats, RunDistributions,
};
use pacabench_core::config::ConfigOverrides;
use pacabench_core::persistence::{list_run_summaries, RunStore, RunSummary};
use pacabench_core::{Benchmark, Command as BenchCommand, Config};
use progress::ProgressDisplay;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tui::TuiDisplay;

#[derive(Debug, Parser)]
#[command(
    name = "pacabench",
    version,
    about = "A local-first benchmark harness for LLM agents"
)]
struct Cli {
    /// Path to the benchmark configuration file.
    #[arg(short, long, default_value = "pacabench.yaml")]
    config: String,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Load and print the parsed configuration.
    ShowConfig,

    /// Run the benchmark.
    Run {
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        run_id: Option<String>,
        #[arg(long)]
        runs_dir: Option<String>,
        #[arg(long)]
        cache_dir: Option<String>,
        /// Comma-separated list of agents to run (e.g. --agents agent1,agent2).
        #[arg(long, short = 'a')]
        agents: Option<String>,
        /// Override concurrency (number of parallel workers).
        #[arg(long)]
        concurrency: Option<usize>,
        /// Override timeout in seconds.
        #[arg(long)]
        timeout: Option<f64>,
        /// Force the TUI progress display.
        #[arg(long)]
        tui: bool,
        /// Disable the TUI progress display.
        #[arg(long, conflicts_with = "tui")]
        no_tui: bool,
    },

    /// Show aggregated metrics for a run id.
    Show {
        /// Run ID (supports partial match).
        #[arg()]
        run_id: Option<String>,
        #[arg(long)]
        runs_dir: Option<String>,
        /// Show individual case rows.
        #[arg(long)]
        cases: bool,
        /// Show only failures in the case list.
        #[arg(long)]
        failures: bool,
        /// Limit number of cases shown.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },

    /// Retry failed cases from a previous run.
    Retry {
        /// Run ID to retry from.
        #[arg()]
        run_id: String,
        #[arg(long)]
        runs_dir: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
        /// Force the TUI progress display.
        #[arg(long)]
        tui: bool,
        /// Disable the TUI progress display.
        #[arg(long, conflicts_with = "tui")]
        no_tui: bool,
    },

    /// Export results to JSON or Markdown.
    Export {
        /// Run ID to export.
        #[arg()]
        run_id: String,
        /// Output format: json or markdown.
        #[arg(long, default_value = "json")]
        format: String,
        /// Output file path (defaults to stdout).
        #[arg(short, long)]
        output: Option<String>,
        #[arg(long)]
        runs_dir: Option<String>,
        /// Include per-case results in the export payload (JSON only).
        #[arg(long)]
        include_cases: bool,
    },

    /// Initialize a new pacabench project with example files.
    Init {
        /// Name for the benchmark. Defaults to project name from pyproject.toml if present.
        #[arg(long)]
        name: Option<String>,
        /// Overwrite existing files.
        #[arg(long)]
        force: bool,
        /// Skip updating pyproject.toml.
        #[arg(long)]
        no_pyproject: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = PathBuf::from(&cli.config);

    // Handle init separately (doesn't need config file)
    if let Some(Command::Init {
        name,
        force,
        no_pyproject,
    }) = cli.command
    {
        let project_name = name.unwrap_or_else(|| {
            init::read_pyproject_name().unwrap_or_else(|| "my-benchmark".to_string())
        });
        return init::execute(&project_name, force, !no_pyproject);
    }

    // Build overrides based on command
    let overrides = match &cli.command {
        Some(Command::Run {
            runs_dir,
            cache_dir,
            concurrency,
            timeout,
            ..
        }) => ConfigOverrides {
            concurrency: *concurrency,
            timeout_seconds: *timeout,
            max_retries: None,
            runs_dir: runs_dir.clone(),
            cache_dir: cache_dir.clone(),
        },
        _ => ConfigOverrides::default(),
    };

    // Load config
    let mut config = Config::from_file(&config_path, overrides)?;

    match cli.command {
        Some(Command::ShowConfig) => cmd_show_config(&config)?,
        Some(Command::Run {
            limit,
            run_id,
            agents,
            tui,
            no_tui,
            ..
        }) => cmd_run(&mut config, run_id, limit, agents, tui, no_tui)?,
        Some(Command::Show {
            run_id,
            runs_dir,
            cases,
            failures,
            limit,
        }) => cmd_show(&config, run_id, runs_dir, cases, failures, limit)?,
        Some(Command::Retry {
            run_id,
            runs_dir,
            limit,
            tui,
            no_tui,
        }) => cmd_retry(&mut config, &run_id, runs_dir, limit, tui, no_tui)?,
        Some(Command::Export {
            run_id,
            format,
            output,
            runs_dir,
            include_cases,
        }) => cmd_export(&config, &run_id, &format, output, runs_dir, include_cases)?,
        Some(Command::Init { .. }) => unreachable!(),
        None => {
            println!(
                "Loaded benchmark '{}': {} agent(s), {} dataset(s).",
                config.name,
                config.agents.len(),
                config.datasets.len()
            );
            println!("\nUse --help to see available commands.");
        }
    }

    Ok(())
}

// ============================================================================
// Command handlers
// ============================================================================

fn cmd_show_config(config: &Config) -> Result<()> {
    println!(
        "Loaded benchmark '{}': {} agent(s), {} dataset(s).",
        config.name,
        config.agents.len(),
        config.datasets.len()
    );
    let yaml = serde_yaml::to_string(config)?;
    println!("{yaml}");
    Ok(())
}

fn cmd_run(
    config: &mut Config,
    run_id: Option<String>,
    limit: Option<usize>,
    agents: Option<String>,
    tui: bool,
    no_tui: bool,
) -> Result<()> {
    // Filter agents if specified
    if let Some(agents_filter) = agents {
        let filter_set: std::collections::HashSet<&str> =
            agents_filter.split(',').map(|s| s.trim()).collect();
        let available: Vec<String> = config.agents.iter().map(|a| a.name.clone()).collect();
        config
            .agents
            .retain(|a| filter_set.contains(a.name.as_str()));
        if config.agents.is_empty() {
            return Err(anyhow!(
                "No agents found matching filter: {}. Available: {}",
                agents_filter,
                available.join(", ")
            ));
        }
        println!(
            "Running {} agent(s): {}",
            config.agents.len(),
            config
                .agents
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let bench = Benchmark::new(config.clone());
    let display_mode = resolve_display_mode(tui, no_tui)?;
    run_benchmark_with_progress(bench, run_id, limit, display_mode, config.runs_dir.clone())
}

fn cmd_show(
    config: &Config,
    run_id: Option<String>,
    runs_dir: Option<String>,
    cases: bool,
    failures: bool,
    limit: usize,
) -> Result<()> {
    let runs_dir = resolve_runs_dir(config, runs_dir);
    let summaries = list_run_summaries(&runs_dir)
        .with_context(|| format!("listing runs in {}", runs_dir.display()))?;

    if run_id.is_none() {
        print_run_list(&summaries, limit);
        return Ok(());
    }

    let partial = run_id.as_ref().expect("guarded above");
    let resolved_id = resolve_run_id_from_summaries(&summaries, partial)?;
    let store = RunStore::new(runs_dir.join(&resolved_id))
        .with_context(|| format!("opening run {}", resolved_id))?;

    // Use load_stats() - single source of truth
    let stats = store
        .load_stats()
        .with_context(|| format!("loading stats for {}", resolved_id))?;

    let results = store
        .load_results()
        .with_context(|| format!("loading results for {}", resolved_id))?;
    let distributions = RunDistributions::from_results(&results);
    print_run_stats(&stats, Some(&distributions));

    if cases {
        let errors = store
            .load_errors()
            .with_context(|| format!("loading errors for {}", resolved_id))?;
        print_cases(&resolved_id, &results, &errors, failures, limit);
    }

    Ok(())
}

fn cmd_retry(
    config: &mut Config,
    run_id: &str,
    runs_dir: Option<String>,
    limit: Option<usize>,
    tui: bool,
    no_tui: bool,
) -> Result<()> {
    if let Some(dir) = runs_dir {
        config.runs_dir = PathBuf::from(dir);
    }

    let resolved_id = resolve_run_id(&config.runs_dir, run_id)?;
    let store = RunStore::new(config.runs_dir.join(&resolved_id))
        .with_context(|| format!("opening run {}", resolved_id))?;
    let results = store
        .load_results()
        .with_context(|| format!("loading results for {}", resolved_id))?;
    let failed_count = results.iter().filter(|r| !r.passed).count();

    if failed_count == 0 {
        println!("No failed cases to retry in run {resolved_id}");
        return Ok(());
    }

    println!(
        "Retrying {} failed cases from run {resolved_id}",
        failed_count
    );

    let bench = Benchmark::new(config.clone());
    let display_mode = resolve_display_mode(tui, no_tui)?;
    run_benchmark_with_progress(
        bench,
        Some(resolved_id),
        limit,
        display_mode,
        config.runs_dir.clone(),
    )
}

fn cmd_export(
    config: &Config,
    run_id: &str,
    format: &str,
    output: Option<String>,
    runs_dir: Option<String>,
    include_cases: bool,
) -> Result<()> {
    let runs_dir = resolve_runs_dir(config, runs_dir);
    let resolved_id = resolve_run_id(&runs_dir, run_id)?;
    let store = RunStore::new(runs_dir.join(&resolved_id))
        .with_context(|| format!("opening run {}", resolved_id))?;

    // Use load_stats() - single source of truth
    let stats = store
        .load_stats()
        .with_context(|| format!("loading stats for {}", resolved_id))?;

    let cases = if include_cases {
        Some(
            store
                .load_results()
                .with_context(|| format!("loading results for {}", resolved_id))?,
        )
    } else {
        None
    };

    let content = match format {
        "json" => {
            serde_json::to_string_pretty(&build_export_json_from_stats(&stats, cases.as_deref()))?
        }
        "markdown" | "md" => build_export_markdown_from_stats(&stats),
        _ => return Err(anyhow!("unsupported format: {format}")),
    };

    match output {
        Some(path) => {
            fs::write(&path, &content)?;
            println!("Exported to {path}");
        }
        None => println!("{content}"),
    }

    Ok(())
}

// ============================================================================
// Utilities
// ============================================================================

fn resolve_run_id(runs_dir: &std::path::Path, partial: &str) -> Result<String> {
    let summaries = list_run_summaries(runs_dir)?;
    if summaries.is_empty() {
        return Err(anyhow!("no runs found in {}", runs_dir.display()));
    }

    if let Some(exact) = summaries.iter().find(|s| s.run_id == partial) {
        return Ok(exact.run_id.clone());
    }

    let matches: Vec<&RunSummary> = summaries
        .iter()
        .filter(|s| s.run_id.starts_with(partial) || s.run_id.contains(partial))
        .collect();

    match matches.len() {
        0 => Err(anyhow!("no run found matching '{partial}'")),
        1 => Ok(matches[0].run_id.clone()),
        _ => {
            let options: Vec<_> = matches.iter().map(|s| s.run_id.clone()).collect();
            Err(anyhow!(
                "ambiguous run ID '{partial}', matches: {}",
                options.join(", ")
            ))
        }
    }
}

fn resolve_run_id_from_summaries(summaries: &[RunSummary], partial: &str) -> Result<String> {
    if summaries.is_empty() {
        return Err(anyhow!("no runs available"));
    }

    if let Some(exact) = summaries.iter().find(|s| s.run_id == partial) {
        return Ok(exact.run_id.clone());
    }

    let matches: Vec<&RunSummary> = summaries
        .iter()
        .filter(|s| s.run_id.starts_with(partial) || s.run_id.contains(partial))
        .collect();

    match matches.len() {
        0 => Err(anyhow!("no run found matching '{partial}'")),
        1 => Ok(matches[0].run_id.clone()),
        _ => {
            let options: Vec<_> = matches.iter().map(|s| s.run_id.clone()).collect();
            Err(anyhow!(
                "ambiguous run ID '{partial}', matches: {}",
                options.join(", ")
            ))
        }
    }
}

fn resolve_runs_dir(config: &Config, override_dir: Option<String>) -> PathBuf {
    override_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| config.runs_dir.clone())
}

fn run_benchmark_with_progress(
    bench: Benchmark,
    run_id: Option<String>,
    limit: Option<usize>,
    display_mode: DisplayMode,
    runs_dir: PathBuf,
) -> Result<()> {
    let events = bench.subscribe();
    let cmd_tx = bench.command_sender();
    let rt = tokio::runtime::Runtime::new().context("creating tokio runtime")?;
    rt.block_on(async {
        let display_handle = match display_mode {
            DisplayMode::Tui => {
                let display = TuiDisplay::new(runs_dir.clone());
                tokio::spawn(async move { display.run(events).await })
            }
            DisplayMode::Progress => {
                let display = ProgressDisplay::new(runs_dir);
                tokio::spawn(async move {
                    display.run(events).await;
                    Ok(())
                })
            }
        };

        // Spawn signal handler
        let signal_handle = tokio::spawn(handle_signals(cmd_tx));

        let result = bench.run(run_id, limit).await;

        // Abort signal handler since benchmark is done
        signal_handle.abort();

        let display_result = display_handle.await;
        let display_result = match display_result {
            Ok(inner) => inner,
            Err(err) => return Err(anyhow!("display task failed: {err}")),
        };

        display_result?;

        result.map_err(|err| anyhow!(err))?;
        Ok(())
    })
    .context("running benchmark")?;

    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum DisplayMode {
    Tui,
    Progress,
}

fn resolve_display_mode(tui: bool, no_tui: bool) -> Result<DisplayMode> {
    if no_tui {
        return Ok(DisplayMode::Progress);
    }

    if tui {
        if TuiDisplay::is_supported() {
            return Ok(DisplayMode::Tui);
        }
        return Err(anyhow!(
            "TUI requested but stdout is not a TTY or TERM is unsupported"
        ));
    }

    if TuiDisplay::is_supported() {
        Ok(DisplayMode::Tui)
    } else {
        Ok(DisplayMode::Progress)
    }
}

/// Handle shutdown signals (SIGINT/SIGTERM).
///
/// First signal sends a graceful stop, second signal aborts immediately.
async fn handle_signals(cmd_tx: mpsc::UnboundedSender<BenchCommand>) {
    let stop_requested = Arc::new(AtomicBool::new(false));

    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigint = signal(SignalKind::interrupt()).expect("failed to register SIGINT");
        let mut sigterm = signal(SignalKind::terminate()).expect("failed to register SIGTERM");

        loop {
            tokio::select! {
                _ = sigint.recv() => {}
                _ = sigterm.recv() => {}
            }

            if stop_requested.swap(true, Ordering::SeqCst) {
                eprintln!("\nForce abort requested, terminating...");
                let _ = cmd_tx.send(BenchCommand::Abort {
                    reason: "user interrupt (force)".to_string(),
                });
                break;
            } else {
                eprintln!("\nShutdown requested, finishing current cases... (press Ctrl+C again to force)");
                let _ = cmd_tx.send(BenchCommand::Stop {
                    reason: "user interrupt".to_string(),
                });
            }
        }
    }

    #[cfg(not(unix))]
    {
        loop {
            tokio::signal::ctrl_c().await.ok();

            if stop_requested.swap(true, Ordering::SeqCst) {
                eprintln!("\nForce abort requested, terminating...");
                let _ = cmd_tx.send(BenchCommand::Abort {
                    reason: "user interrupt (force)".to_string(),
                });
                break;
            } else {
                eprintln!("\nShutdown requested, finishing current cases... (press Ctrl+C again to force)");
                let _ = cmd_tx.send(BenchCommand::Stop {
                    reason: "user interrupt".to_string(),
                });
            }
        }
    }
}
