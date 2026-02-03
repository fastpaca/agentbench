//! Indicatif-based progress display for the CLI.
//!
//! Provides rich terminal output using progress bars for each agent.
//! Receives events via tokio channel and updates the display.
//! Cost is computed here using pricing tables from the pricing module.

use crate::formatting::{print_run_stats, RunDistributions};
use crate::pricing::{calculate_cost, calculate_cost_from_metrics};
use console::style;
use dashmap::DashMap;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use pacabench_core::persistence::RunStore;
use pacabench_core::Event;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::broadcast;

const MICRODOLLARS_PER_USD: f64 = 1_000_000.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum CaseOutcome {
    Passed,
    Failed,
    Error,
}

struct AgentState {
    bar: ProgressBar,
    passed: AtomicU64,
    failed: AtomicU64,
    errors: AtomicU64,
    cost_micros: AtomicU64,
    // Tracks dataset:case_id so retries/resumes don't double-increment the bar.
    cases: DashMap<String, CaseOutcome>,
    // Tracks cost per case so retries replace prior cost instead of double counting.
    case_costs: DashMap<String, u64>,
}

impl AgentState {
    fn new(bar: ProgressBar) -> Self {
        Self {
            bar,
            passed: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            cost_micros: AtomicU64::new(0),
            cases: DashMap::new(),
            case_costs: DashMap::new(),
        }
    }

    fn cost_usd(&self) -> f64 {
        self.cost_micros.load(Ordering::Relaxed) as f64 / MICRODOLLARS_PER_USD
    }

    fn update_message(&self, start_time: Option<Instant>) {
        let passed = self.passed.load(Ordering::Relaxed);
        let failed = self.failed.load(Ordering::Relaxed);
        let errors = self.errors.load(Ordering::Relaxed);
        let cost = self.cost_usd();

        let elapsed = start_time.map(|t| t.elapsed().as_secs()).unwrap_or(0);

        let elapsed_str = if elapsed >= 60 {
            format!("{}m{}s", elapsed / 60, elapsed % 60)
        } else {
            format!("{}s", elapsed)
        };

        let msg = format!(
            "{} {} {} {} {} {} {} {}",
            style("✓").green(),
            style(passed).green().bold(),
            style("✗").red(),
            style(failed).red().bold(),
            style("⚠").yellow(),
            style(errors).yellow(),
            style(format!("${:.3}", cost)).cyan(),
            style(elapsed_str).dim(),
        );
        self.bar.set_message(msg);
    }

    fn adjust_counters(&self, from: Option<CaseOutcome>, to: CaseOutcome) -> bool {
        if from == Some(to) {
            return false;
        }

        match from {
            Some(CaseOutcome::Passed) => {
                self.passed.fetch_sub(1, Ordering::Relaxed);
            }
            Some(CaseOutcome::Failed) => {
                self.failed.fetch_sub(1, Ordering::Relaxed);
            }
            Some(CaseOutcome::Error) => {
                self.errors.fetch_sub(1, Ordering::Relaxed);
            }
            None => {}
        }

        match to {
            CaseOutcome::Passed => self.passed.fetch_add(1, Ordering::Relaxed),
            CaseOutcome::Failed => self.failed.fetch_add(1, Ordering::Relaxed),
            CaseOutcome::Error => self.errors.fetch_add(1, Ordering::Relaxed),
        };

        true
    }

    fn record_case(&self, key: &str, outcome: CaseOutcome, cost_micros: u64) {
        let prev = self.cases.get(key).map(|v| *v.value());
        let changed = self.adjust_counters(prev, outcome);
        if prev.is_none() {
            // Only move the bar forward on the first observation of a case; retries just refresh
            // the outcome and message.
            self.bar.inc(1);
        }
        if changed {
            self.cases.insert(key.to_string(), outcome);
        }

        self.update_case_cost(key, cost_micros);
    }

    fn update_case_cost(&self, key: &str, cost_micros: u64) {
        if let Some(prev) = self.case_costs.insert(key.to_string(), cost_micros) {
            self.cost_micros.fetch_sub(prev, Ordering::Relaxed);
        }
        self.cost_micros.fetch_add(cost_micros, Ordering::Relaxed);
    }
}

/// Progress display using indicatif for rich terminal output.
pub struct ProgressDisplay {
    multi: MultiProgress,
    agents: DashMap<String, AgentState>,
    start_time: Mutex<Option<Instant>>,
    runs_dir: PathBuf,
}

impl Default for ProgressDisplay {
    fn default() -> Self {
        Self::new(PathBuf::from("."))
    }
}

impl ProgressDisplay {
    pub fn new(runs_dir: PathBuf) -> Self {
        Self {
            multi: MultiProgress::new(),
            agents: DashMap::new(),
            start_time: Mutex::new(None),
            runs_dir,
        }
    }

    /// Process events until the channel closes.
    pub async fn run(self, mut rx: broadcast::Receiver<Event>) {
        loop {
            match rx.recv().await {
                Ok(event) => self.handle_event(event),
                Err(broadcast::error::RecvError::Closed) => break,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }
    }

    fn handle_event(&self, event: Event) {
        match event {
            Event::RunStarted {
                run_id,
                total_cases,
                resuming,
                retrying,
                completed_cases,
                agents,
                datasets: _,
                agent_totals,
                agent_completed,
            } => {
                *self.start_time.lock() = Some(Instant::now());

                // For retry scenarios, agents have different case counts so don't show "X cases x Y agents"
                let run_scope = if retrying {
                    format!("{total_cases} failed cases")
                } else if agents.is_empty() {
                    format!("{total_cases} cases")
                } else {
                    let cases_per_agent = total_cases / agents.len() as u64;
                    format!(
                        "{total_cases} tasks = {cases_per_agent} cases x {} agents",
                        agents.len()
                    )
                };

                if retrying {
                    println!(
                        "{} Retrying {} ({})",
                        style("→").cyan().bold(),
                        style(&run_id).bold(),
                        run_scope
                    );
                } else if resuming {
                    println!(
                        "{} Resuming {} ({} already done, {})",
                        style("→").cyan().bold(),
                        style(&run_id).bold(),
                        completed_cases,
                        run_scope
                    );
                } else {
                    println!(
                        "{} Starting {} ({})",
                        style("→").cyan().bold(),
                        style(&run_id).bold(),
                        run_scope
                    );
                }

                let mut agent_names = agents.clone();
                agent_names.sort();

                // Filter out agents with 0 cases (e.g., in retry scenarios)
                let agents_with_cases: Vec<_> = agent_names
                    .iter()
                    .filter(|name| agent_totals.get(*name).copied().unwrap_or(1) > 0)
                    .collect();

                let max_name_len = agents_with_cases
                    .iter()
                    .map(|n| n.len())
                    .max()
                    .unwrap_or(10);

                for agent_name in agents_with_cases {
                    let total_for_agent = agent_totals.get(agent_name).copied().unwrap_or(0);
                    let completed_for_agent = agent_completed.get(agent_name).copied().unwrap_or(0);

                    let bar_style = ProgressStyle::with_template(&format!(
                        "{{spinner:.green}} {{prefix:<{max_name_len}}} [{{bar:30.cyan/blue}}] {{pos}}/{{len}} {{msg}}"
                    ))
                    .unwrap()
                    .progress_chars("█▓▒░  ");

                    let bar = self.multi.add(ProgressBar::new(total_for_agent));
                    bar.set_style(bar_style);
                    bar.set_prefix(agent_name.clone());
                    bar.set_position(completed_for_agent.min(total_for_agent));

                    let state = AgentState::new(bar);
                    state.update_message(*self.start_time.lock());
                    state
                        .bar
                        .enable_steady_tick(std::time::Duration::from_millis(100));
                    self.agents.insert(agent_name.clone(), state);
                }
            }

            Event::CaseStarted { .. } => {}

            Event::CaseCompleted {
                agent,
                dataset,
                case_id,
                passed,
                is_error,
                input_tokens,
                output_tokens,
                cached_tokens,
                model,
                judge_input_tokens,
                judge_output_tokens,
                judge_cached_tokens,
                judge_model,
                ..
            } => {
                if let Some(state) = self.agents.get(&agent) {
                    let outcome = if is_error {
                        CaseOutcome::Error
                    } else if passed {
                        CaseOutcome::Passed
                    } else {
                        CaseOutcome::Failed
                    };

                    let key = format!("{dataset}:{case_id}");

                    // Use model-aware pricing; fallback to default model if missing
                    let model_name = model.as_deref().unwrap_or("gpt-4o-mini");
                    let judge_model_name = judge_model.as_deref().unwrap_or(model_name);

                    let default_agent_cost =
                        calculate_cost_from_metrics(input_tokens, output_tokens, cached_tokens);
                    let default_judge_cost = calculate_cost_from_metrics(
                        judge_input_tokens,
                        judge_output_tokens,
                        judge_cached_tokens,
                    );

                    // If we have explicit models, price per model for accuracy
                    let agent_cost = if model.is_some() {
                        calculate_cost(model_name, input_tokens, output_tokens, cached_tokens)
                    } else {
                        default_agent_cost
                    };

                    let judge_cost = if judge_model.is_some() {
                        calculate_cost(
                            judge_model_name,
                            judge_input_tokens,
                            judge_output_tokens,
                            judge_cached_tokens,
                        )
                    } else {
                        default_judge_cost
                    };

                    let total_cost = agent_cost + judge_cost;
                    let total_micros = (total_cost * MICRODOLLARS_PER_USD) as u64;
                    state.value().record_case(&key, outcome, total_micros);

                    state.value().update_message(*self.start_time.lock());
                }
            }

            Event::CircuitTripped { error_ratio } => {
                if let Some(state) = self.agents.iter().next() {
                    state.value().bar.println(format!(
                        "{} Circuit breaker tripped at {:.1}% error rate",
                        style("⚠").yellow().bold(),
                        error_ratio * 100.0
                    ));
                }
            }

            Event::RunCompleted { aborted, stats } => {
                for entry in self.agents.iter() {
                    entry.value().bar.finish();
                }

                let distributions = RunStore::new(self.runs_dir.join(&stats.run_id))
                    .ok()
                    .and_then(|store| store.load_results().ok())
                    .map(|results| RunDistributions::from_results(&results));

                print_run_stats(&stats, distributions.as_ref());

                if aborted {
                    println!(
                        "     {} Run was aborted early",
                        style("warning").yellow().bold()
                    );
                    println!();
                }
            }

            Event::Error { message } => {
                println!("{} {}", style("Error:").red().bold(), message);
            }
        }
    }
}
