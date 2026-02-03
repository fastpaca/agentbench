//! Output formatting for run results (show, export commands).
//!
//! All formatting functions should take [`RunStats`] as input - the single
//! source of truth for all run metrics. Do not re-aggregate from raw results.

use crate::pricing::{calculate_cost, calculate_cost_from_metrics, calculate_cost_from_tokens};
use console::style;
use pacabench_core::persistence::{ErrorEntry, RunSummary};
use pacabench_core::stats::RunStats;
use pacabench_core::types::ErrorType;
use pacabench_core::CaseResult;

fn indent_line(mut spans: StyledLine) -> StyledLine {
    let mut line = Vec::with_capacity(spans.len() + 1);
    line.push(span("     ", style_default()));
    line.append(&mut spans);
    line
}

fn section_line(name: &str) -> StyledLine {
    indent_line(vec![span(name, style_magenta_bold())])
}

fn metric_line(name: &str, value: StyledLine) -> StyledLine {
    let width: usize = 20;
    let dots = ".".repeat(width.saturating_sub(name.len()));
    let mut spans = Vec::new();
    spans.push(span(name, style_cyan()));
    spans.push(span(dots, style_dim()));
    spans.push(span(" ", style_default()));
    spans.extend(value);
    indent_line(spans)
}

fn histogram_lines(label: &str, buckets: &[(String, u64)]) -> Vec<StyledLine> {
    let mut lines = Vec::new();
    lines.push(indent_line(vec![span(label, style_cyan())]));
    if buckets.is_empty() {
        lines.push(indent_line(vec![span("no data", style_dim())]));
        return lines;
    }

    let max_count = buckets.iter().map(|(_, count)| *count).max().unwrap_or(0);
    let bar_width = 24usize;
    let label_width = buckets
        .iter()
        .map(|(bucket_label, _)| bucket_label.len())
        .max()
        .unwrap_or(0);

    for (bucket_label, count) in buckets {
        let filled = if max_count == 0 {
            0
        } else {
            ((count.saturating_mul(bar_width as u64) as f64 / max_count as f64).round() as usize)
                .min(bar_width)
        };
        let empty = bar_width.saturating_sub(filled);
        let bar = "█".repeat(filled);
        let pad = "░".repeat(empty);

        lines.push(indent_line(vec![
            span(format!("{:>label_width$}", bucket_label), style_dim()),
            span(" | ", style_default()),
            span(bar, style_cyan()),
            span(pad, style_dim()),
            span(format!(" {}", count), style_default()),
        ]));
    }

    lines
}

// Show command formatting

pub fn print_run_list(runs: &[RunSummary], limit: usize) {
    if runs.is_empty() {
        println!("No runs found.");
        return;
    }

    println!("Runs (showing up to {limit}):");
    println!(
        "{:<28} {:<10} {:>14} {:>10}",
        "run_id", "status", "cases", "progress"
    );

    for r in runs.iter().take(limit.min(runs.len())) {
        let progress = format!("{:.0}%", r.progress * 100.0);
        let cases = if r.total_cases > 0 {
            format!("{}/{}", r.completed_cases, r.total_cases)
        } else {
            "-".into()
        };
        let status = format!("{:?}", r.status).to_lowercase();

        println!(
            "{:<28} {:<10} {:>14} {:>10}",
            r.run_id, status, cases, progress
        );
    }

    if runs.len() > limit {
        println!("... and {} more", runs.len() - limit);
    }
}

// RunStats-based formatting (single source of truth)

/// Format duration in human-readable form
fn format_duration_ms(ms: f64) -> String {
    if ms >= 1000.0 {
        format!("{:.1}s", ms / 1000.0)
    } else {
        format!("{:.0}ms", ms)
    }
}

/// Format large numbers with commas for readability
fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.insert(0, ',');
        }
        result.insert(0, c);
    }
    result
}

pub fn build_run_stats_view(
    stats: &RunStats,
    distributions: Option<&RunDistributions>,
) -> Vec<StyledLine> {
    let cost = calculate_cost_from_tokens(&stats.tokens);
    let mut lines: Vec<StyledLine> = Vec::new();

    let status_text = match stats.status {
        pacabench_core::RunStatus::Completed => "COMPLETED",
        pacabench_core::RunStatus::Aborted => "ABORTED",
        pacabench_core::RunStatus::Failed => "FAILED",
        pacabench_core::RunStatus::Running => "RUNNING",
        pacabench_core::RunStatus::Loading => "LOADING",
        pacabench_core::RunStatus::Pending => "PENDING",
        pacabench_core::RunStatus::Finalizing => "FINALIZING",
    };
    let status_style = match stats.status {
        pacabench_core::RunStatus::Completed => style_green_bold(),
        pacabench_core::RunStatus::Aborted | pacabench_core::RunStatus::Failed => style_red_bold(),
        _ => style_yellow(),
    };

    lines.push(Vec::new());
    lines.push(indent_line(vec![
        span(stats.run_id.clone(), style_cyan_bold()),
        span(" ", style_default()),
        span(status_text, status_style),
    ]));

    if let Some(retry) = &stats.retry_of {
        lines.push(indent_line(vec![
            span("retry of:", style_dim()),
            span(" ", style_default()),
            span(retry.clone(), style_default()),
        ]));
    }

    if stats.completed_cases == 0 {
        lines.push(Vec::new());
        lines.push(indent_line(vec![span("No results yet.", style_default())]));
        lines.push(Vec::new());
        return lines;
    }

    let acc_pct = stats.accuracy * 100.0;
    let bar_width = 20;
    let filled = ((stats.accuracy * bar_width as f64).round() as usize).min(bar_width);
    let empty = bar_width - filled;
    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(empty));
    let acc_style = if acc_pct >= 80.0 {
        style_green_bold()
    } else if acc_pct >= 50.0 {
        SpanStyle {
            fg: Some(SpanColor::Yellow),
            bold: true,
            ..SpanStyle::default()
        }
    } else {
        style_red_bold()
    };

    lines.push(Vec::new());
    lines.push(section_line("results"));
    lines.push(metric_line(
        "accuracy",
        vec![
            span(bar, style_cyan()),
            span(" ", style_default()),
            span(format!("{:.1}%", acc_pct), acc_style),
            span("  passed=", style_default()),
            span(stats.passed_cases.to_string(), style_green_bold()),
            span("  failed=", style_default()),
            span(stats.failed_cases.to_string(), style_red_bold()),
        ],
    ));
    lines.push(metric_line(
        "cases",
        vec![span(
            format!("{}/{}", stats.completed_cases, stats.planned_cases),
            style_default(),
        )],
    ));

    lines.push(Vec::new());
    lines.push(section_line("performance"));
    lines.push(metric_line(
        "duration",
        vec![span(
            format!(
                "p50={} p95={}",
                format_duration_ms(stats.metrics.p50_duration_ms),
                format_duration_ms(stats.metrics.p95_duration_ms)
            ),
            style_default(),
        )],
    ));
    lines.push(metric_line(
        "llm_latency",
        vec![span(
            format!(
                "avg={} p50={} p95={}",
                format_duration_ms(stats.metrics.avg_llm_latency_ms),
                format_duration_ms(stats.metrics.p50_llm_latency_ms),
                format_duration_ms(stats.metrics.p95_llm_latency_ms)
            ),
            style_default(),
        )],
    ));
    lines.push(metric_line(
        "attempts",
        vec![span(
            format!(
                "avg={:.1} max={}",
                stats.metrics.avg_attempts, stats.metrics.max_attempts
            ),
            style_default(),
        )],
    ));

    if let Some(distributions) = distributions {
        lines.push(Vec::new());
        lines.push(section_line("distributions"));
        lines.extend(histogram_lines("duration", distributions.duration()));
        lines.extend(histogram_lines("cost", distributions.cost()));
    }

    lines.push(Vec::new());
    lines.push(section_line("tokens"));
    lines.push(metric_line(
        "agent_input",
        vec![span(
            format_number(stats.tokens.agent_input_tokens),
            style_default(),
        )],
    ));
    lines.push(metric_line(
        "agent_output",
        vec![span(
            format_number(stats.tokens.agent_output_tokens),
            style_default(),
        )],
    ));
    lines.push(metric_line(
        "llm_calls",
        vec![span(stats.tokens.agent_calls.to_string(), style_default())],
    ));
    if stats.tokens.judge_input_tokens > 0 || stats.tokens.judge_output_tokens > 0 {
        lines.push(metric_line(
            "judge_input",
            vec![span(
                format_number(stats.tokens.judge_input_tokens),
                style_default(),
            )],
        ));
        lines.push(metric_line(
            "judge_output",
            vec![span(
                format_number(stats.tokens.judge_output_tokens),
                style_default(),
            )],
        ));
    }

    lines.push(Vec::new());
    lines.push(section_line("cost"));
    lines.push(metric_line(
        "total",
        vec![span(
            format!("${:.4}", cost.total_cost_usd),
            style_default(),
        )],
    ));
    lines.push(metric_line(
        "agent",
        vec![span(
            format!("${:.4}", cost.agent_cost_usd),
            style_default(),
        )],
    ));
    lines.push(metric_line(
        "judge",
        vec![span(
            format!("${:.4}", cost.judge_cost_usd),
            style_default(),
        )],
    ));
    if !stats.tokens.models_used.is_empty() {
        lines.push(metric_line(
            "models",
            vec![span(stats.tokens.models_used.join(", "), style_default())],
        ));
    }

    if stats.system_error_count > 0 || stats.fatal_error_count > 0 {
        lines.push(Vec::new());
        lines.push(section_line("errors"));
        if stats.system_error_count > 0 {
            lines.push(metric_line(
                "system_errors",
                vec![span(stats.system_error_count.to_string(), style_yellow())],
            ));
        }
        if stats.fatal_error_count > 0 {
            lines.push(metric_line(
                "fatal_errors",
                vec![span(stats.fatal_error_count.to_string(), style_red_bold())],
            ));
        }
    }

    if !stats.by_agent.is_empty() {
        lines.push(Vec::new());
        lines.push(section_line("agents"));

        let mut agents: Vec<_> = stats.by_agent.values().collect();
        agents.sort_by(|a, b| {
            b.accuracy
                .partial_cmp(&a.accuracy)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        for agent in agents {
            let agent_cost = calculate_cost_from_tokens(&agent.tokens);
            let acc = agent.accuracy * 100.0;
            let acc_style = if acc >= 80.0 {
                style_green_bold()
            } else if acc >= 50.0 {
                SpanStyle {
                    fg: Some(SpanColor::Yellow),
                    bold: true,
                    ..SpanStyle::default()
                }
            } else {
                style_red_bold()
            };
            lines.push(metric_line(
                &agent.agent_name,
                vec![
                    span(format!("{:.1}%", acc), acc_style),
                    span("  passed=", style_default()),
                    span(agent.passed_cases.to_string(), style_green_bold()),
                    span("  failed=", style_default()),
                    span(agent.failed_cases.to_string(), style_red_bold()),
                    span(
                        format!(
                            "  p50={}",
                            format_duration_ms(agent.metrics.p50_duration_ms)
                        ),
                        style_default(),
                    ),
                    span(
                        format!("  ${:.4}", agent_cost.total_cost_usd),
                        style_default(),
                    ),
                ],
            ));
        }
    }

    if !stats.failures.is_empty() {
        lines.push(Vec::new());
        lines.push(section_line(&format!(
            "failures ({})",
            stats.failures.len()
        )));
        lines.push(Vec::new());

        for failure in stats.failures.iter().take(10) {
            let reason = if failure.reason.len() > 60 {
                format!("{}...", &failure.reason[..57])
            } else {
                failure.reason.clone()
            };
            lines.push(indent_line(vec![
                span(
                    format!("{}/{}", failure.agent_name, failure.case_id),
                    style_white(),
                ),
                span(" ", style_default()),
                span(reason, style_dim()),
            ]));
        }
        if stats.failures.len() > 10 {
            lines.push(indent_line(vec![
                span("...", style_dim()),
                span(
                    format!(" and {} more", stats.failures.len() - 10),
                    style_dim(),
                ),
            ]));
        }
    }

    lines.push(Vec::new());
    lines
}

const DURATION_BUCKETS: &[(f64, &str)] = &[
    (100.0, "0-100ms"),
    (250.0, "100-250ms"),
    (500.0, "250-500ms"),
    (1000.0, "0.5-1s"),
    (2000.0, "1-2s"),
    (5000.0, "2-5s"),
    (10_000.0, "5-10s"),
    (20_000.0, "10-20s"),
    (40_000.0, "20-40s"),
    (f64::INFINITY, "40s+"),
];
const COST_BUCKETS: &[(f64, &str)] = &[
    (0.0001, "<$0.0001"),
    (0.0005, "$0.0001-0.0005"),
    (0.001, "$0.0005-0.001"),
    (0.005, "$0.001-0.005"),
    (0.01, "$0.005-0.01"),
    (0.05, "$0.01-0.05"),
    (0.1, "$0.05-0.1"),
    (0.5, "$0.1-0.5"),
    (f64::INFINITY, "$0.5+"),
];

#[derive(Debug, Clone)]
pub struct RunDistributions {
    duration: Vec<(String, u64)>,
    cost: Vec<(String, u64)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanColor {
    Green,
    Red,
    Yellow,
    Cyan,
    Magenta,
    White,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SpanStyle {
    pub fg: Option<SpanColor>,
    pub bold: bool,
    pub dim: bool,
}

#[derive(Debug, Clone)]
pub struct StyledSpan {
    pub text: String,
    pub style: SpanStyle,
}

pub type StyledLine = Vec<StyledSpan>;

fn span(text: impl Into<String>, style: SpanStyle) -> StyledSpan {
    StyledSpan {
        text: text.into(),
        style,
    }
}

fn style_default() -> SpanStyle {
    SpanStyle::default()
}

fn style_dim() -> SpanStyle {
    SpanStyle {
        dim: true,
        ..SpanStyle::default()
    }
}

fn style_cyan() -> SpanStyle {
    SpanStyle {
        fg: Some(SpanColor::Cyan),
        ..SpanStyle::default()
    }
}

fn style_cyan_bold() -> SpanStyle {
    SpanStyle {
        fg: Some(SpanColor::Cyan),
        bold: true,
        ..SpanStyle::default()
    }
}

fn style_green_bold() -> SpanStyle {
    SpanStyle {
        fg: Some(SpanColor::Green),
        bold: true,
        ..SpanStyle::default()
    }
}

fn style_red_bold() -> SpanStyle {
    SpanStyle {
        fg: Some(SpanColor::Red),
        bold: true,
        ..SpanStyle::default()
    }
}

fn style_yellow() -> SpanStyle {
    SpanStyle {
        fg: Some(SpanColor::Yellow),
        ..SpanStyle::default()
    }
}

fn style_magenta_bold() -> SpanStyle {
    SpanStyle {
        fg: Some(SpanColor::Magenta),
        bold: true,
        ..SpanStyle::default()
    }
}

fn style_white() -> SpanStyle {
    SpanStyle {
        fg: Some(SpanColor::White),
        ..SpanStyle::default()
    }
}

impl RunDistributions {
    pub fn from_results(results: &[CaseResult]) -> Self {
        let mut duration_counts = vec![0u64; DURATION_BUCKETS.len()];
        let mut cost_counts = vec![0u64; COST_BUCKETS.len()];

        for result in results {
            let idx = duration_bucket_index(result.runner_duration_ms);
            if let Some(slot) = duration_counts.get_mut(idx) {
                *slot = slot.saturating_add(1);
            }

            let cost_idx = cost_bucket_index(case_cost_usd(result));
            if let Some(slot) = cost_counts.get_mut(cost_idx) {
                *slot = slot.saturating_add(1);
            }
        }

        let duration = DURATION_BUCKETS
            .iter()
            .zip(duration_counts)
            .map(|((_, label), count)| (label.to_string(), count))
            .collect();
        let cost = COST_BUCKETS
            .iter()
            .zip(cost_counts)
            .map(|((_, label), count)| (label.to_string(), count))
            .collect();

        Self { duration, cost }
    }

    fn duration(&self) -> &[(String, u64)] {
        &self.duration
    }

    fn cost(&self) -> &[(String, u64)] {
        &self.cost
    }
}

fn duration_bucket_index(duration_ms: f64) -> usize {
    let value = if duration_ms.is_finite() && duration_ms > 0.0 {
        duration_ms
    } else {
        0.0
    };
    DURATION_BUCKETS
        .iter()
        .position(|(bound, _)| value <= *bound)
        .unwrap_or_else(|| DURATION_BUCKETS.len().saturating_sub(1))
}

fn cost_bucket_index(cost_usd: f64) -> usize {
    let value = if cost_usd.is_finite() && cost_usd >= 0.0 {
        cost_usd
    } else {
        0.0
    };
    COST_BUCKETS
        .iter()
        .position(|(bound, _)| value <= *bound)
        .unwrap_or_else(|| COST_BUCKETS.len().saturating_sub(1))
}

fn case_cost_usd(result: &CaseResult) -> f64 {
    let agent_model = result.llm_metrics.model.as_deref();
    let agent_cost = match agent_model {
        Some(model) => calculate_cost(
            model,
            result.llm_metrics.input_tokens,
            result.llm_metrics.output_tokens,
            result.llm_metrics.cached_tokens,
        ),
        None => calculate_cost_from_metrics(
            result.llm_metrics.input_tokens,
            result.llm_metrics.output_tokens,
            result.llm_metrics.cached_tokens,
        ),
    };

    let judge_cost = result
        .judge_metrics
        .as_ref()
        .map(|judge| {
            let model = judge.model.as_deref().or(agent_model);
            match model {
                Some(model) => calculate_cost(
                    model,
                    judge.input_tokens,
                    judge.output_tokens,
                    judge.cached_tokens,
                ),
                None => calculate_cost_from_metrics(
                    judge.input_tokens,
                    judge.output_tokens,
                    judge.cached_tokens,
                ),
            }
        })
        .unwrap_or(0.0);

    agent_cost + judge_cost
}

/// Print run details from RunStats - k6 style output.
pub fn print_run_stats(stats: &RunStats, distributions: Option<&RunDistributions>) {
    let lines = build_run_stats_view(stats, distributions);
    for line in lines {
        if line.is_empty() {
            println!();
            continue;
        }
        for span in line {
            let mut styled = style(span.text);
            if let Some(color) = span.style.fg {
                styled = match color {
                    SpanColor::Green => styled.green(),
                    SpanColor::Red => styled.red(),
                    SpanColor::Yellow => styled.yellow(),
                    SpanColor::Cyan => styled.cyan(),
                    SpanColor::Magenta => styled.magenta(),
                    SpanColor::White => styled.white(),
                };
            }
            if span.style.bold {
                styled = styled.bold();
            }
            if span.style.dim {
                styled = styled.dim();
            }
            print!("{}", styled);
        }
        println!();
    }
}

/// Export schema version.
///
/// ## v2 (current)
/// - Structure derived from `RunStats` (single source of truth)
/// - Added `schema_version` field
/// - Added `tokens.per_model` for per-model token breakdown
/// - Added `original_total_cases`, `active_cases`, `retry_of` for retry tracking
/// - `by_agent` replaces `agents` with computed stats (no raw results per-agent)
/// - `failures` array includes `reason` from judge/error
/// - `cases` field (optional, via `--include-cases`) contains raw `CaseResult` array
/// - Error counts include transient errors (not just final failures)
///
/// ## v1 (legacy, no longer produced)
/// - `agents` contained per-agent raw results
/// - No per-model token breakdown
/// - No retry lineage tracking
/// - Error counts only included final failures
pub const EXPORT_SCHEMA_VERSION: &str = "v2";

/// Build JSON export from RunStats.
pub fn build_export_json_from_stats(
    stats: &RunStats,
    cases: Option<&[CaseResult]>,
) -> serde_json::Value {
    let cost = calculate_cost_from_tokens(&stats.tokens);

    serde_json::json!({
        "schema_version": EXPORT_SCHEMA_VERSION,
        "run_id": stats.run_id,
        "status": format!("{:?}", stats.status).to_lowercase(),
        "start_time": stats.start_time,
        "end_time": stats.end_time,
        "planned_cases": stats.planned_cases,
        "original_total_cases": stats.original_total_cases,
        "active_cases": stats.active_cases,
        "completed_cases": stats.completed_cases,
        "passed_cases": stats.passed_cases,
        "failed_cases": stats.failed_cases,
        "accuracy": stats.accuracy,
        "retry_of": stats.retry_of,
        "metrics": stats.metrics,
        "tokens": {
            "agent_input": stats.tokens.agent_input_tokens,
            "agent_output": stats.tokens.agent_output_tokens,
            "agent_cached": stats.tokens.agent_cached_tokens,
            "judge_input": stats.tokens.judge_input_tokens,
            "judge_output": stats.tokens.judge_output_tokens,
            "models_used": stats.tokens.models_used,
            "per_model": stats.tokens.per_model.iter().map(|(model, usage)| {
                (model.clone(), serde_json::json!({
                    "agent_input": usage.agent_input_tokens,
                    "agent_output": usage.agent_output_tokens,
                    "agent_cached": usage.agent_cached_tokens,
                    "agent_calls": usage.agent_calls,
                    "judge_input": usage.judge_input_tokens,
                    "judge_output": usage.judge_output_tokens,
                    "judge_cached": usage.judge_cached_tokens,
                }))
            }).collect::<serde_json::Map<String, serde_json::Value>>()
        },
        "cost": {
            "agent_usd": cost.agent_cost_usd,
            "judge_usd": cost.judge_cost_usd,
            "total_usd": cost.total_cost_usd,
        },
        "by_agent": stats.by_agent.iter().map(|(name, agent)| {
            let agent_cost = calculate_cost_from_tokens(&agent.tokens);
            (name.clone(), serde_json::json!({
                "completed_cases": agent.completed_cases,
                "passed_cases": agent.passed_cases,
                "failed_cases": agent.failed_cases,
                "accuracy": agent.accuracy,
                "metrics": agent.metrics,
                "cost_usd": agent_cost.total_cost_usd,
            }))
        }).collect::<serde_json::Map<String, serde_json::Value>>(),
        "failures": stats.failures.iter().map(|f| {
            serde_json::json!({
                "case_id": f.case_id,
                "dataset": f.dataset_name,
                "agent": f.agent_name,
                "error_type": format!("{:?}", f.error_type).to_lowercase(),
                "reason": f.reason,
                "attempt": f.attempt,
            })
        }).collect::<Vec<_>>(),
        "system_error_count": stats.system_error_count,
        "fatal_error_count": stats.fatal_error_count,
        "cases": cases.map(|c| serde_json::to_value(c).unwrap_or_default())
    })
}

/// Build Markdown export from RunStats.
pub fn build_export_markdown_from_stats(stats: &RunStats) -> String {
    let mut md = String::new();
    let cost = calculate_cost_from_tokens(&stats.tokens);

    md.push_str(&format!("# Run: {}\n\n", stats.run_id));
    md.push_str(&format!("_Schema {}_\n\n", EXPORT_SCHEMA_VERSION));
    md.push_str("## Summary\n\n");
    md.push_str(&format!(
        "- **Status**: {}\n",
        format!("{:?}", stats.status).to_lowercase()
    ));
    if let Some(retry) = &stats.retry_of {
        md.push_str(&format!("- **Retry of**: {}\n", retry));
    }
    md.push_str(&format!(
        "- **Cases**: {} / {}\n",
        stats.completed_cases, stats.planned_cases
    ));
    if let Some(active) = stats
        .active_cases
        .filter(|active| *active != stats.planned_cases)
    {
        md.push_str(&format!("- **Scheduled This Run**: {}\n", active));
    }
    if let Some(orig) = stats
        .original_total_cases
        .filter(|orig| *orig != stats.planned_cases)
    {
        md.push_str(&format!("- **Original Planned Cases**: {}\n", orig));
    }
    md.push_str(&format!("- **Accuracy**: {:.1}%\n", stats.accuracy * 100.0));
    md.push_str(&format!(
        "- **Duration (p50/p95)**: {:.0}ms / {:.0}ms\n",
        stats.metrics.p50_duration_ms, stats.metrics.p95_duration_ms
    ));
    md.push_str(&format!(
        "- **LLM Latency (avg/p50/p95)**: {:.0}ms / {:.0}ms / {:.0}ms\n",
        stats.metrics.avg_llm_latency_ms,
        stats.metrics.p50_llm_latency_ms,
        stats.metrics.p95_llm_latency_ms
    ));
    md.push_str(&format!(
        "- **Tokens (in/out)**: {} / {}\n",
        stats.tokens.agent_input_tokens, stats.tokens.agent_output_tokens
    ));
    md.push_str(&format!(
        "- **Judge Tokens (in/out)**: {} / {}\n",
        stats.tokens.judge_input_tokens, stats.tokens.judge_output_tokens
    ));
    md.push_str(&format!(
        "- **Cost**: ${:.4} (judge ${:.4})\n",
        cost.agent_cost_usd, cost.judge_cost_usd
    ));
    md.push_str(&format!(
        "- **Attempts (avg/max)**: {:.1} / {}\n",
        stats.metrics.avg_attempts, stats.metrics.max_attempts
    ));

    if !stats.tokens.models_used.is_empty() {
        md.push_str(&format!(
            "- **Models**: {}\n",
            stats.tokens.models_used.join(", ")
        ));
    }

    if !stats.tokens.per_model.is_empty() {
        md.push_str("\n### Tokens by Model\n\n");
        md.push_str("| Model | Agent In | Agent Out | Agent Cached | Judge In | Judge Out |\n");
        md.push_str("|-------|----------|-----------|--------------|----------|-----------|\n");
        let mut models: Vec<_> = stats.tokens.per_model.iter().collect();
        models.sort_by(|a, b| a.0.cmp(b.0));
        for (model, usage) in models {
            md.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                model,
                usage.agent_input_tokens,
                usage.agent_output_tokens,
                usage.agent_cached_tokens,
                usage.judge_input_tokens,
                usage.judge_output_tokens
            ));
        }
    }

    // Per-agent table
    if !stats.by_agent.is_empty() {
        md.push_str("\n## By Agent\n\n");
        md.push_str("| Agent | Passed/Total | Accuracy | p50 | Cost |\n");
        md.push_str("|-------|--------------|----------|-----|------|\n");

        let mut agents: Vec<_> = stats.by_agent.values().collect();
        agents.sort_by(|a, b| a.agent_name.cmp(&b.agent_name));

        for agent in agents {
            let agent_cost = calculate_cost_from_tokens(&agent.tokens);
            md.push_str(&format!(
                "| {} | {}/{} | {:.1}% | {:.0}ms | ${:.4} |\n",
                agent.agent_name,
                agent.passed_cases,
                agent.completed_cases,
                agent.accuracy * 100.0,
                agent.metrics.p50_duration_ms,
                agent_cost.total_cost_usd
            ));
        }
    }

    // Failures
    if !stats.failures.is_empty() {
        md.push_str("\n## Failures\n\n");
        for failure in &stats.failures {
            md.push_str(&format!(
                "- **{}/{}** ({}): {}\n",
                failure.dataset_name, failure.case_id, failure.agent_name, failure.reason
            ));
        }
    }

    md
}

// Case-level display (still needs raw results)
pub fn print_cases(
    run_id: &str,
    results: &[CaseResult],
    errors: &[ErrorEntry],
    failures_only: bool,
    limit: usize,
) {
    use std::collections::HashSet;

    println!("\nCases for {run_id}:");
    let mut rows: Vec<(String, String, String, String, String)> = Vec::new();
    let mut seen_keys: HashSet<(String, String, String)> = HashSet::new();

    for r in results {
        let status = if matches!(
            r.error_type,
            ErrorType::SystemFailure | ErrorType::FatalError
        ) {
            "error"
        } else if r.passed {
            "pass"
        } else {
            "fail"
        };
        if failures_only && status == "pass" {
            continue;
        }

        seen_keys.insert((
            r.agent_name.clone(),
            r.dataset_name.clone(),
            r.case_id.clone(),
        ));

        let summary = r
            .judge_reason
            .clone()
            .or_else(|| r.error.clone())
            .or_else(|| r.output.clone())
            .unwrap_or_else(|| "-".into());
        rows.push((
            r.case_id.clone(),
            r.agent_name.clone(),
            r.dataset_name.clone(),
            status.to_string(),
            summary,
        ));
    }

    // Only add error entries that don't have a corresponding case result
    for e in errors {
        let key = (
            e.agent_name.clone().unwrap_or_default(),
            e.dataset_name.clone().unwrap_or_default(),
            e.case_id.clone().unwrap_or_default(),
        );
        if seen_keys.contains(&key) {
            continue;
        }

        let status = match e.error_type {
            ErrorType::FatalError => "fatal",
            _ => "error",
        };
        let case_id = e.case_id.clone().unwrap_or_else(|| "-".into());
        let agent = e.agent_name.clone().unwrap_or_else(|| "-".into());
        let dataset = e.dataset_name.clone().unwrap_or_else(|| "-".into());
        let summary = e.error.clone().unwrap_or_else(|| "unknown error".into());
        rows.push((case_id, agent, dataset, status.into(), summary));
    }

    if rows.is_empty() {
        println!("No cases recorded.");
        return;
    }

    rows.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));

    println!(
        "{:<12} {:<16} {:<16} {:<8} output/error",
        "case_id", "agent", "dataset", "status"
    );
    let total = rows.len();
    for (case_id, agent, dataset, status, summary) in rows.into_iter().take(limit) {
        let truncated = if summary.len() > 80 {
            format!("{}...", &summary[..77])
        } else {
            summary
        };
        println!(
            "{:<12} {:<16} {:<16} {:<8} {}",
            case_id, agent, dataset, status, truncated
        );
    }

    if total > limit {
        println!("... showing {limit} of {total}");
    }
}
