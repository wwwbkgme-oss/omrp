//! Live TUI dashboard — `omrp dashboard`
//!
//! Reads the persistent ledger on each refresh cycle and renders a live
//! model-health table, routing stats, and a help bar.
//!
//! Controls:
//!   q / Ctrl-C  — quit
//!   r           — force refresh

use std::io;
use std::time::{Duration, Instant};

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState},
    Terminal,
};

use omrp_core::pipeline::EventPipeline;
use omrp_core::router::RouterEngine;
use omrp_core::state::State;
use omrp_types::task::RouteRequest;

use crate::config::Config;

// ─── App state ───────────────────────────────────────────────────────────────

/// Snapshot of state read from the ledger — cheap to clone, never mutated.
struct Snapshot {
    models: Vec<ModelRow>,
    total_completions: u64,
    total_failures: u64,
    total_fallbacks: u64,
    best_chat: String,
    ledger_entries: usize,
    last_refreshed: Instant,
}

struct ModelRow {
    id: String,
    provider: String,
    score: f64,
    latency_ms: u64,
    success_ratio: f32,
    garbage: bool,
    tasks: String,
}

impl Snapshot {
    fn from_pipeline(pipeline: &EventPipeline, _cfg: &Config) -> Self {
        let router = RouterEngine::default();

        let (models, diagnostics, best_chat, ledger_len) = pipeline.state().read(|state: &State| {
            let decision = router.select(state, &RouteRequest::default());
            let best_chat = decision.selected_model.clone();

            let mut rows: Vec<ModelRow> = state
                .models
                .iter()
                .map(|m| {
                    let h = state.health.get(&m.id);
                    let score = decision
                        .scores
                        .iter()
                        .find(|s| s.model_id == m.id)
                        .map(|s| s.total)
                        .unwrap_or(0.0);
                    let tasks: Vec<&str> =
                        m.capabilities.task_suitability.iter().map(|t| t.as_str()).collect();
                    ModelRow {
                        id: m.id.clone(),
                        provider: m.provider.clone(),
                        score,
                        latency_ms: h.map(|h| h.rolling_latency_avg_ms as u64).unwrap_or(0),
                        success_ratio: h.map(|h| h.success_ratio).unwrap_or(0.5),
                        garbage: h.map(|h| h.garbage).unwrap_or(false),
                        tasks: tasks.join(", "),
                    }
                })
                .collect();

            // Sort best-first so the winner is always at the top.
            rows.sort_by(|a, b| {
                b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)
            });

            let d = state.diagnostics.clone();
            (rows, d, best_chat, pipeline.event_log().len())
        });

        Snapshot {
            models,
            total_completions: diagnostics.total_completions,
            total_failures: diagnostics.total_failures,
            total_fallbacks: diagnostics.total_fallbacks,
            best_chat,
            ledger_entries: ledger_len,
            last_refreshed: Instant::now(),
        }
    }
}

// ─── Entry point ─────────────────────────────────────────────────────────────

/// Run the dashboard.  Blocks until the user presses `q`.
pub fn run(cfg: &Config) -> io::Result<()> {
    // ── Terminal setup ────────────────────────────────────────────────────────
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, cfg);

    // ── Teardown (always runs, even on error) ─────────────────────────────────
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn load_snapshot(cfg: &Config) -> Snapshot {
    let path = cfg.ledger_path();
    let pipeline = EventPipeline::load_from_ledger(&path)
        .unwrap_or_else(|_| EventPipeline::new());
    // Re-register configured models that may not be in the ledger yet.
    let mut pipeline = pipeline;
    let existing: Vec<String> = pipeline.state().read(|s| s.models.iter().map(|m| m.id.clone()).collect());
    for event in cfg.to_model_events() {
        if let omrp_events::event::Event::ModelAdded { ref model, .. } = event {
            if !existing.contains(&model.id) {
                let _ = pipeline.process(event);
            }
        }
    }
    Snapshot::from_pipeline(&pipeline, cfg)
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    cfg: &Config,
) -> io::Result<()> {
    const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

    let mut snap = load_snapshot(cfg);
    let mut table_state = TableState::default();
    if !snap.models.is_empty() {
        table_state.select(Some(0));
    }

    loop {
        terminal.draw(|f| draw(f, &snap, &mut table_state))?;

        // Poll for keyboard events with a short timeout so we can auto-refresh.
        let timeout = REFRESH_INTERVAL
            .checked_sub(snap.last_refreshed.elapsed())
            .unwrap_or(Duration::ZERO);

        if event::poll(timeout)? {
            if let Event::Key(KeyEvent { code, modifiers, .. }) = event::read()? {
                match code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(());
                    }
                    KeyCode::Char('r') => {
                        snap = load_snapshot(cfg);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let next = snap
                            .models
                            .len()
                            .saturating_sub(1)
                            .min(table_state.selected().unwrap_or(0) + 1);
                        table_state.select(Some(next));
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        let prev = table_state.selected().unwrap_or(0).saturating_sub(1);
                        table_state.select(Some(prev));
                    }
                    _ => {}
                }
            }
        }

        // Auto-refresh when interval elapsed.
        if snap.last_refreshed.elapsed() >= REFRESH_INTERVAL {
            snap = load_snapshot(cfg);
        }
    }
}

// ─── Rendering ───────────────────────────────────────────────────────────────

fn draw(
    f: &mut ratatui::Frame,
    snap: &Snapshot,
    table_state: &mut TableState,
) {
    let area = f.area();

    // Layout: title | table | stats bar | help bar
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // title
            Constraint::Min(5),     // table
            Constraint::Length(3),  // stats
            Constraint::Length(1),  // help
        ])
        .split(area);

    draw_title(f, chunks[0]);
    draw_table(f, chunks[1], snap, table_state);
    draw_stats(f, chunks[2], snap);
    draw_help(f, chunks[3]);
}

fn draw_title(f: &mut ratatui::Frame, area: Rect) {
    let title = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("OMRP", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled("Open Model Routing Protocol", Style::default().fg(Color::White)),
            Span::raw("  —  "),
            Span::styled("live dashboard", Style::default().fg(Color::DarkGray)),
        ]),
    ])
    .block(Block::default().borders(Borders::ALL))
    .alignment(Alignment::Center);
    f.render_widget(title, area);
}

fn draw_table(
    f: &mut ratatui::Frame,
    area: Rect,
    snap: &Snapshot,
    table_state: &mut TableState,
) {
    let header = Row::new(vec![
        Cell::from("Model").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Cell::from("Provider").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Cell::from("Score").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Cell::from("Latency").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Cell::from("Ratio").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Cell::from("Tasks").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Cell::from("Status").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
    ])
    .height(1)
    .bottom_margin(1);

    let rows: Vec<Row> = snap.models.iter().map(|m| {
        let status_style = if m.garbage {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Green)
        };
        let status_text = if m.garbage { "GARBAGE" } else { "ok" };

        let score_style = if m.score >= 1.0 {
            Style::default().fg(Color::Green)
        } else if m.score >= 0.7 {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::Red)
        };

        let provider_style = if m.provider == "kilo" {
            Style::default().fg(Color::Magenta)
        } else {
            Style::default().fg(Color::Cyan)
        };

        let latency_str = if m.latency_ms == 0 {
            "—".into()
        } else {
            format!("{}ms", m.latency_ms)
        };

        Row::new(vec![
            Cell::from(m.id.clone()),
            Cell::from(m.provider.clone()).style(provider_style),
            Cell::from(format!("{:.3}", m.score)).style(score_style),
            Cell::from(latency_str),
            Cell::from(format!("{:.1}%", m.success_ratio * 100.0)),
            Cell::from(m.tasks.clone()).style(Style::default().fg(Color::DarkGray)),
            Cell::from(status_text).style(status_style),
        ])
        .height(1)
    }).collect();

    let widths = [
        Constraint::Percentage(30),
        Constraint::Percentage(11),
        Constraint::Percentage(8),
        Constraint::Percentage(9),
        Constraint::Percentage(8),
        Constraint::Percentage(24),
        Constraint::Percentage(10),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} model(s) ", snap.models.len())),
        )
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");

    f.render_stateful_widget(table, area, table_state);
}

fn draw_stats(f: &mut ratatui::Frame, area: Rect, snap: &Snapshot) {
    let elapsed = snap.last_refreshed.elapsed().as_secs();
    let age = if elapsed == 0 { "just now".into() } else { format!("{elapsed}s ago") };

    let stats = vec![
        Span::styled("Completions: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            snap.total_completions.to_string(),
            Style::default().fg(Color::Green),
        ),
        Span::raw("   "),
        Span::styled("Failures: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            snap.total_failures.to_string(),
            if snap.total_failures > 0 { Style::default().fg(Color::Red) } else { Style::default().fg(Color::White) },
        ),
        Span::raw("   "),
        Span::styled("Fallbacks: ", Style::default().fg(Color::DarkGray)),
        Span::styled(snap.total_fallbacks.to_string(), Style::default().fg(Color::Yellow)),
        Span::raw("   "),
        Span::styled("Ledger events: ", Style::default().fg(Color::DarkGray)),
        Span::styled(snap.ledger_entries.to_string(), Style::default().fg(Color::White)),
        Span::raw("   "),
        Span::styled("Best (chat): ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            if snap.best_chat.is_empty() { "—".into() } else { snap.best_chat.clone() },
            Style::default().fg(Color::Cyan),
        ),
        Span::raw("   "),
        Span::styled("Refreshed: ", Style::default().fg(Color::DarkGray)),
        Span::styled(age, Style::default().fg(Color::DarkGray)),
    ];

    let paragraph = Paragraph::new(Line::from(stats))
        .block(Block::default().borders(Borders::ALL).title(" Stats "))
        .alignment(Alignment::Left);
    f.render_widget(paragraph, area);
}

fn draw_help(f: &mut ratatui::Frame, area: Rect) {
    let help = Paragraph::new(Line::from(vec![
        Span::styled(" q ", Style::default().bg(Color::DarkGray).fg(Color::White)),
        Span::raw(" quit   "),
        Span::styled(" r ", Style::default().bg(Color::DarkGray).fg(Color::White)),
        Span::raw(" refresh   "),
        Span::styled(" ↑↓ ", Style::default().bg(Color::DarkGray).fg(Color::White)),
        Span::raw(" navigate   auto-refresh: 2s"),
    ]))
    .alignment(Alignment::Center)
    .style(Style::default().fg(Color::DarkGray));
    f.render_widget(help, area);
}
