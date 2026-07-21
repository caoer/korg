//! Dual-pane traffic monitor TUI for anthropic-serve.
//!
//! ```text
//! ┌ Requests ──────────────────────────────────────────────────────────┐
//! │ * r1  frames=4  phase=out  C:2 G:2                                 │
//! ├ Claude → bridge ──────────────────┬ bridge → Grok ─────────────────┤
//! │ pretty JSON / SSE summary         │ pretty JSON / SSE summary      │
//! ├ status ────────────────────────────────────────────────────────────┤
//! │ q quit · j/k select · Tab pane · [/] scroll · w write capture      │
//! └────────────────────────────────────────────────────────────────────┘
//! ```

use std::io::{self, Stdout, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
    enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use tokio::sync::Notify;
use tokio::sync::oneshot;

use crate::traffic::{TrafficBus, TrafficFrame, TrafficSide, TrafficSnapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneFocus {
    Requests,
    Claude,
    Grok,
}

/// Run the monitor until quit; signals `shutdown_tx` so the HTTP server stops.
pub async fn run_monitor(
    traffic: TrafficBus,
    listen_addr: String,
    capture_dir: Option<PathBuf>,
    shutdown_tx: oneshot::Sender<()>,
) -> anyhow::Result<()> {
    let notify = traffic.notify_handle();
    let mut terminal = setup_terminal()?;
    let result = monitor_loop(
        &mut terminal,
        traffic,
        notify,
        listen_addr,
        capture_dir,
        shutdown_tx,
    )
    .await;
    restore_terminal(&mut terminal)?;
    result
}

/// Own the full terminal like vim / grok fullscreen: alternate screen, cleared,
/// cursor hidden. Main scrollback is left untouched until LeaveAlternateScreen.
fn setup_terminal() -> anyhow::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        Clear(ClearType::All),
        Clear(ClearType::Purge),
        Hide,
    )?;
    stdout.flush()?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    install_panic_hook();
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> anyhow::Result<()> {
    // Best-effort restore even if individual steps fail.
    // LeaveAlternateScreen restores the main scrollback; do not Clear it.
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen, Show);
    let _ = terminal.show_cursor();
    Ok(())
}

fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let mut out = io::stdout();
        let _ = execute!(out, LeaveAlternateScreen, Show);
        let _ = out.flush();
        original(info);
    }));
}

struct UiState {
    list_state: ListState,
    focus: PaneFocus,
    claude_scroll: u16,
    grok_scroll: u16,
    selected_id: Option<String>,
    status: String,
    follow_latest: bool,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            list_state: ListState::default(),
            focus: PaneFocus::Requests,
            claude_scroll: 0,
            grok_scroll: 0,
            selected_id: None,
            status: String::new(),
            follow_latest: true,
        }
    }
}

async fn monitor_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    traffic: TrafficBus,
    notify: Arc<Notify>,
    listen_addr: String,
    capture_dir: Option<PathBuf>,
    shutdown_tx: oneshot::Sender<()>,
) -> anyhow::Result<()> {
    let mut ui = UiState::default();
    let mut shutdown_tx = Some(shutdown_tx);

    loop {
        let snap = traffic.snapshot();
        sync_selection(&mut ui, &snap);

        terminal.draw(|f| draw(f, &snap, &mut ui, &listen_addr, capture_dir.as_ref()))?;

        // Poll keys with timeout; also wake on traffic notify.
        let timeout = Duration::from_millis(200);
        tokio::select! {
            _ = notify.notified() => {
                // redraw next loop
            }
            _ = tokio::time::sleep(timeout) => {
                // fall through to poll keys non-blocking
            }
        }

        while event::poll(Duration::from_millis(0))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => {
                        if let Some(tx) = shutdown_tx.take() {
                            let _ = tx.send(());
                        }
                        return Ok(());
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if let Some(tx) = shutdown_tx.take() {
                            let _ = tx.send(());
                        }
                        return Ok(());
                    }
                    KeyCode::Tab => {
                        ui.focus = match ui.focus {
                            PaneFocus::Requests => PaneFocus::Claude,
                            PaneFocus::Claude => PaneFocus::Grok,
                            PaneFocus::Grok => PaneFocus::Requests,
                        };
                    }
                    KeyCode::BackTab => {
                        ui.focus = match ui.focus {
                            PaneFocus::Requests => PaneFocus::Grok,
                            PaneFocus::Claude => PaneFocus::Requests,
                            PaneFocus::Grok => PaneFocus::Claude,
                        };
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if ui.focus == PaneFocus::Requests {
                            ui.follow_latest = false;
                            select_delta(&mut ui, &snap, 1);
                        } else {
                            scroll_focus(&mut ui, 1);
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if ui.focus == PaneFocus::Requests {
                            ui.follow_latest = false;
                            select_delta(&mut ui, &snap, -1);
                        } else {
                            scroll_focus(&mut ui, -1);
                        }
                    }
                    KeyCode::Char('[') => scroll_focus(&mut ui, -3),
                    KeyCode::Char(']') => scroll_focus(&mut ui, 3),
                    KeyCode::Char('g') => {
                        if ui.focus == PaneFocus::Requests && !snap.requests.is_empty() {
                            ui.follow_latest = true;
                            ui.list_state.select(Some(0));
                            ui.selected_id = snap.requests.first().map(|r| r.request_id.clone());
                            ui.claude_scroll = 0;
                            ui.grok_scroll = 0;
                        }
                    }
                    KeyCode::Char('w') => {
                        if let Some(dir) = &capture_dir {
                            if let Some(id) = &ui.selected_id {
                                let n = write_request_snapshot(dir, id, &traffic);
                                ui.status = format!("wrote {n} frames for {id} → {}", dir.display());
                            } else {
                                ui.status = "no request selected".into();
                            }
                        } else {
                            ui.status =
                                "no --capture-dir; start with --capture-dir to enable w".into();
                        }
                    }
                    KeyCode::Char('?') => {
                        ui.status = "q quit · j/k · Tab panes · [/] scroll · g latest · w dump"
                            .into();
                    }
                    _ => {}
                }
            }
        }
    }
}

fn sync_selection(ui: &mut UiState, snap: &TrafficSnapshot) {
    if snap.requests.is_empty() {
        ui.list_state.select(None);
        ui.selected_id = None;
        return;
    }
    if ui.follow_latest {
        ui.list_state.select(Some(0));
        ui.selected_id = Some(snap.requests[0].request_id.clone());
        return;
    }
    if let Some(id) = &ui.selected_id {
        if let Some(idx) = snap.requests.iter().position(|r| r.request_id == *id) {
            ui.list_state.select(Some(idx));
            return;
        }
    }
    // Selection evaporated — pin newest.
    ui.list_state.select(Some(0));
    ui.selected_id = Some(snap.requests[0].request_id.clone());
}

fn select_delta(ui: &mut UiState, snap: &TrafficSnapshot, delta: i32) {
    if snap.requests.is_empty() {
        return;
    }
    let len = snap.requests.len() as i32;
    let cur = ui.list_state.selected().unwrap_or(0) as i32;
    let next = (cur + delta).clamp(0, len - 1) as usize;
    ui.list_state.select(Some(next));
    ui.selected_id = Some(snap.requests[next].request_id.clone());
    ui.claude_scroll = 0;
    ui.grok_scroll = 0;
}

fn scroll_focus(ui: &mut UiState, delta: i32) {
    let apply = |s: &mut u16| {
        if delta < 0 {
            *s = s.saturating_sub((-delta) as u16);
        } else {
            *s = s.saturating_add(delta as u16);
        }
    };
    match ui.focus {
        PaneFocus::Claude => apply(&mut ui.claude_scroll),
        PaneFocus::Grok => apply(&mut ui.grok_scroll),
        PaneFocus::Requests => {}
    }
}

fn draw(
    f: &mut ratatui::Frame,
    snap: &TrafficSnapshot,
    ui: &mut UiState,
    listen_addr: &str,
    capture_dir: Option<&PathBuf>,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(min_u16(12, f.area().height.saturating_sub(8))),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(f.area());

    draw_requests(f, chunks[0], snap, ui);
    draw_panes(f, chunks[1], snap, ui);
    draw_status(f, chunks[2], snap, ui, listen_addr, capture_dir);
}

fn min_u16(a: u16, b: u16) -> u16 {
    a.min(b).max(3)
}

fn draw_requests(f: &mut ratatui::Frame, area: Rect, snap: &TrafficSnapshot, ui: &mut UiState) {
    let items: Vec<ListItem> = snap
        .requests
        .iter()
        .map(|r| {
            let err = if r.has_error { " ERR" } else { "" };
            let short = short_id(&r.request_id);
            let line = format!(
                "{short}  f={:<3}  C:{:<2} G:{:<2}  {phase}{err}",
                r.frame_count,
                r.claude_frames,
                r.grok_frames,
                phase = truncate(&r.last_phase, 24),
            );
            let style = if r.has_error {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(line, style)))
        })
        .collect();

    let border = if ui.focus == PaneFocus::Requests {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };
    let title = format!(" Requests ({}) ", snap.requests.len());
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(border),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");
    f.render_stateful_widget(list, area, &mut ui.list_state);
}

fn draw_panes(f: &mut ratatui::Frame, area: Rect, snap: &TrafficSnapshot, ui: &UiState) {
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let id = ui.selected_id.as_deref().unwrap_or("");
    let frames: Vec<&TrafficFrame> = snap
        .frames
        .iter()
        .filter(|fr| fr.request_id == id)
        .collect();

    let claude_text = format_side_frames(&frames, TrafficSide::Claude);
    let grok_text = format_side_frames(&frames, TrafficSide::Grok);

    render_scroll_pane(
        f,
        panes[0],
        " Claude → bridge ",
        &claude_text,
        ui.claude_scroll,
        ui.focus == PaneFocus::Claude,
    );
    render_scroll_pane(
        f,
        panes[1],
        " bridge → Grok ",
        &grok_text,
        ui.grok_scroll,
        ui.focus == PaneFocus::Grok,
    );
}

fn render_scroll_pane(
    f: &mut ratatui::Frame,
    area: Rect,
    title: &str,
    text: &str,
    scroll: u16,
    focused: bool,
) {
    let border = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };
    let para = Paragraph::new(text.to_string())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(border),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(para, area);
}

fn draw_status(
    f: &mut ratatui::Frame,
    area: Rect,
    snap: &TrafficSnapshot,
    ui: &UiState,
    listen_addr: &str,
    capture_dir: Option<&PathBuf>,
) {
    let cap = capture_dir
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "—".into());
    let help = if ui.status.is_empty() {
        "q quit · j/k · Tab · [/] scroll · g latest · w dump · ? help".to_string()
    } else {
        ui.status.clone()
    };
    let line = format!(
        " {listen_addr}  frames={}  capture={cap}  | {help}",
        snap.total_frames
    );
    let para = Paragraph::new(line).block(Block::default().borders(Borders::ALL).title(" status "));
    f.render_widget(para, area);
}

fn format_side_frames(frames: &[&TrafficFrame], side: TrafficSide) -> String {
    let mut out = String::new();
    let mut any = false;
    for fr in frames.iter().filter(|f| f.side == side) {
        any = true;
        out.push_str(&format!("── {} ──\n", fr.phase));
        match serde_json::to_string_pretty(&fr.payload) {
            Ok(s) => {
                // Cap huge payloads for UI.
                let s = if s.len() > 12_000 {
                    format!("{}…\n[truncated {} bytes]", &s[..12_000], s.len())
                } else {
                    s
                };
                out.push_str(&s);
            }
            Err(_) => out.push_str(&format!("{:?}", fr.payload)),
        }
        out.push_str("\n\n");
    }
    if !any {
        out.push_str("(no frames for this side yet)");
    }
    out
}

fn short_id(id: &str) -> String {
    let s = id.strip_prefix("msg_").unwrap_or(id);
    if s.len() > 8 {
        s[..8].to_string()
    } else {
        s.to_string()
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

fn write_request_snapshot(dir: &PathBuf, request_id: &str, traffic: &TrafficBus) -> usize {
    let frames = traffic.frames_for_request(request_id);
    let path = dir.join(format!("tui-dump-{request_id}.json"));
    if let Ok(body) = serde_json::to_vec_pretty(&frames) {
        let _ = std::fs::write(path, body);
    }
    frames.len()
}
