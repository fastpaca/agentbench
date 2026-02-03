//! Ratatui-based live TUI display for benchmark runs.

use crate::formatting::{
    build_run_stats_view, RunDistributions, SpanColor, SpanStyle, StyledLine, StyledSpan,
};
use anyhow::{anyhow, Context, Result};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, ExecutableCommand};
use pacabench_core::persistence::RunStore;
use pacabench_core::stats::RunStats;
use pacabench_core::Event;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::{backend::CrosstermBackend, Frame, Terminal};
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

const DEFAULT_TICK_RATE: Duration = Duration::from_millis(200);
const SUMMARY_REFRESH_INTERVAL: Duration = Duration::from_millis(750);

struct RunCompletion {
    aborted: bool,
    stats: Box<RunStats>,
}

struct TuiState {
    run_id: Option<String>,
    summary_lines: Vec<StyledLine>,
    last_refresh: Option<Instant>,
    completed: Option<RunCompletion>,
}

impl TuiState {
    fn new() -> Self {
        Self {
            run_id: None,
            summary_lines: vec![vec![StyledSpan {
                text: "Waiting for run events...".to_string(),
                style: SpanStyle {
                    dim: true,
                    ..SpanStyle::default()
                },
            }]],
            last_refresh: None,
            completed: None,
        }
    }

    fn handle_event(&mut self, event: Event) -> bool {
        match event {
            Event::RunStarted { run_id, .. } => {
                self.run_id = Some(run_id);
            }
            Event::RunCompleted { aborted, stats } => {
                self.completed = Some(RunCompletion { aborted, stats });
                return true;
            }
            _ => {}
        }
        false
    }

    fn should_refresh(&self, now: Instant) -> bool {
        match self.last_refresh {
            Some(last) => now.duration_since(last) >= SUMMARY_REFRESH_INTERVAL,
            None => true,
        }
    }

    fn set_refresh_time(&mut self, now: Instant) {
        self.last_refresh = Some(now);
    }
}

pub struct TuiDisplay {
    state: TuiState,
    tick_rate: Duration,
    runs_dir: PathBuf,
}

impl TuiDisplay {
    pub fn new(runs_dir: PathBuf) -> Self {
        Self {
            state: TuiState::new(),
            tick_rate: DEFAULT_TICK_RATE,
            runs_dir,
        }
    }

    pub fn is_supported() -> bool {
        io::stdout().is_terminal()
    }

    pub async fn run(mut self, mut rx: broadcast::Receiver<Event>) -> Result<()> {
        let mut terminal = TerminalSession::new().context("initializing terminal")?;
        let mut ticker = tokio::time::interval(self.tick_rate);

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let now = Instant::now();
                    if self.state.should_refresh(now) {
                        self.refresh_summary()?;
                        self.state.set_refresh_time(now);
                    }
                    terminal.draw(|frame| render_summary(frame, &self.state.summary_lines))?;
                }
                event = rx.recv() => {
                    match event {
                        Ok(event) => {
                            let should_exit = self.state.handle_event(event);
                            if should_exit {
                                self.refresh_summary()?;
                                terminal.draw(|frame| render_summary(frame, &self.state.summary_lines))?;
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                    }
                }
            }
        }

        drop(terminal);

        if let Some(completion) = self.state.completed.take() {
            let distributions = RunStore::new(self.runs_dir.join(&completion.stats.run_id))
                .ok()
                .and_then(|store| store.load_results().ok())
                .map(|results| RunDistributions::from_results(&results));
            crate::formatting::print_run_stats(&completion.stats, distributions.as_ref());

            if completion.aborted {
                println!(
                    "     {} Run was aborted early",
                    console::style("warning").yellow().bold()
                );
                println!();
            }
        }

        Ok(())
    }

    fn refresh_summary(&mut self) -> Result<()> {
        let run_id = match &self.state.run_id {
            Some(run_id) => run_id.clone(),
            None => return Ok(()),
        };

        match load_summary_lines(&self.runs_dir, &run_id) {
            Ok(lines) => {
                self.state.summary_lines = lines;
                Ok(())
            }
            Err(err) => {
                self.state.summary_lines = vec![vec![StyledSpan {
                    text: format!("Failed to load run stats: {err}"),
                    style: SpanStyle {
                        fg: Some(SpanColor::Red),
                        bold: true,
                        ..SpanStyle::default()
                    },
                }]];
                Ok(())
            }
        }
    }
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl TerminalSession {
    fn new() -> Result<Self> {
        enable_raw_mode().context("enabling raw mode")?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen).context("entering alternate screen")?;
        stdout
            .execute(crossterm::cursor::Hide)
            .context("hiding cursor")?;

        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).context("creating terminal")?;

        Ok(Self { terminal })
    }

    fn draw<F>(&mut self, draw_fn: F) -> Result<()>
    where
        F: FnOnce(&mut Frame),
    {
        self.terminal
            .draw(draw_fn)
            .map(|_| ())
            .map_err(|err| anyhow!(err))
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.terminal.show_cursor();
        let mut stdout = io::stdout();
        let _ = stdout.execute(crossterm::cursor::Show);
        let _ = execute!(stdout, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

fn load_summary_lines(runs_dir: &Path, run_id: &str) -> Result<Vec<StyledLine>> {
    let store = RunStore::new(runs_dir.join(run_id))?;
    let stats = store.load_stats()?;
    let results = store.load_results()?;
    let distributions = RunDistributions::from_results(&results);
    Ok(build_run_stats_view(&stats, Some(&distributions)))
}

fn render_summary(frame: &mut Frame, lines: &[StyledLine]) {
    let area = frame.size();
    if area.width == 0 || area.height == 0 {
        return;
    }

    if should_use_two_columns(area.width) {
        let (left, right, failures) = split_for_layout(lines);
        let has_right = !right.is_empty();
        let has_failures = !failures.is_empty();

        if has_right {
            if has_failures {
                let failures_height = desired_failures_height(area.height, failures.len());
                let top_height = area.height.saturating_sub(failures_height);

                if top_height < 3 {
                    let clipped = rolling_lines(&failures, area.height);
                    frame.render_widget(
                        Paragraph::new(Text::from(lines_to_ratatui(&clipped))),
                        area,
                    );
                    return;
                }

                let rows = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(top_height),
                        Constraint::Length(failures_height),
                    ])
                    .split(area);

                render_columns(frame, rows[0], &left, &right);

                let clipped = rolling_lines(&failures, rows[1].height);
                frame.render_widget(
                    Paragraph::new(Text::from(lines_to_ratatui(&clipped))),
                    rows[1],
                );
            } else {
                render_columns(frame, area, &left, &right);
            }

            return;
        }
    }

    let text = Text::from(lines_to_ratatui(lines));
    frame.render_widget(Paragraph::new(text), area);
}

fn lines_to_ratatui(lines: &[StyledLine]) -> Vec<Line<'static>> {
    lines
        .iter()
        .map(|line| {
            if line.is_empty() {
                return Line::from("");
            }
            let spans = line
                .iter()
                .map(|span| Span::styled(span.text.clone(), span_style(span.style)))
                .collect::<Vec<_>>();
            Line::from(spans)
        })
        .collect()
}

fn should_use_two_columns(width: u16) -> bool {
    width >= 120
}

fn render_columns(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    left: &[StyledLine],
    right: &[StyledLine],
) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let left_lines = clamp_lines(left, columns[0].height);
    let right_lines = clamp_lines(right, columns[1].height);

    frame.render_widget(
        Paragraph::new(Text::from(lines_to_ratatui(&left_lines))),
        columns[0],
    );
    frame.render_widget(
        Paragraph::new(Text::from(lines_to_ratatui(&right_lines))),
        columns[1],
    );
}

fn split_for_layout(lines: &[StyledLine]) -> (Vec<StyledLine>, Vec<StyledLine>, Vec<StyledLine>) {
    let sections = collect_sections(lines);
    let mut left_sections: Vec<Vec<StyledLine>> = Vec::new();
    let mut right_sections: Vec<Vec<StyledLine>> = Vec::new();
    let mut failures_section: Vec<StyledLine> = Vec::new();

    for section in sections {
        if is_failures_header(section.header.as_deref()) {
            failures_section = section.lines;
        } else if section.header.as_deref() == Some("distributions") {
            right_sections.push(section.lines);
        } else {
            left_sections.push(section.lines);
        }
    }

    (
        flatten_sections(left_sections),
        flatten_sections(right_sections),
        failures_section,
    )
}

struct Section {
    header: Option<String>,
    lines: Vec<StyledLine>,
}

fn collect_sections(lines: &[StyledLine]) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    let mut current = Section {
        header: None,
        lines: Vec::new(),
    };

    for line in lines {
        if let Some(header) = header_name(line) {
            if !current.lines.is_empty() {
                trim_section_lines(&mut current.lines);
                sections.push(current);
            }
            current = Section {
                header: Some(header),
                lines: Vec::new(),
            };
        }
        current.lines.push(line.clone());
    }

    if !current.lines.is_empty() {
        trim_section_lines(&mut current.lines);
        sections.push(current);
    }

    sections
}

fn header_name(line: &StyledLine) -> Option<String> {
    line.iter()
        .find(|span| span.style.fg == Some(SpanColor::Magenta) && span.style.bold)
        .map(|span| span.text.trim().to_string())
}

fn is_failures_header(header: Option<&str>) -> bool {
    header
        .map(|name| name.starts_with("failures"))
        .unwrap_or(false)
}

fn trim_section_lines(lines: &mut Vec<StyledLine>) {
    while lines.first().is_some_and(|line| line.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
}

fn flatten_sections(sections: Vec<Vec<StyledLine>>) -> Vec<StyledLine> {
    let mut lines = Vec::new();
    for (idx, section) in sections.into_iter().enumerate() {
        if idx > 0 {
            lines.push(Vec::new());
        }
        lines.extend(section);
    }
    lines
}

fn clamp_lines(lines: &[StyledLine], height: u16) -> Vec<StyledLine> {
    let max = height as usize;
    if max == 0 {
        return Vec::new();
    }
    if lines.len() <= max {
        return lines.to_vec();
    }
    lines[..max].to_vec()
}

fn rolling_lines(lines: &[StyledLine], height: u16) -> Vec<StyledLine> {
    let max = height as usize;
    if max == 0 || lines.is_empty() {
        return Vec::new();
    }
    if lines.len() <= max {
        return lines.to_vec();
    }
    if max == 1 {
        return vec![lines[0].clone()];
    }

    let mut out = Vec::with_capacity(max);
    out.push(lines[0].clone());
    let tail_len = max - 1;
    let start = lines.len().saturating_sub(tail_len);
    out.extend_from_slice(&lines[start..]);
    out
}

fn desired_failures_height(area_height: u16, failures_len: usize) -> u16 {
    if failures_len == 0 {
        return 0;
    }

    let max_height = std::cmp::max(4, area_height / 3);
    let desired = failures_len.min(max_height as usize) as u16;
    let min_height = if area_height >= 3 { 3 } else { area_height };
    desired.max(min_height).min(area_height)
}

fn span_style(style: SpanStyle) -> Style {
    let mut rat_style = Style::default();
    if let Some(color) = style.fg {
        rat_style = rat_style.fg(match color {
            SpanColor::Green => Color::Green,
            SpanColor::Red => Color::Red,
            SpanColor::Yellow => Color::Yellow,
            SpanColor::Cyan => Color::Cyan,
            SpanColor::Magenta => Color::Magenta,
            SpanColor::White => Color::White,
        });
    }
    if style.bold {
        rat_style = rat_style.add_modifier(Modifier::BOLD);
    }
    if style.dim {
        rat_style = rat_style.add_modifier(Modifier::DIM);
    }
    rat_style
}
