//! Interactive session resume TUI.
//!
//! Launched via `/resume` in the REPL. Shows a split view with:
//! - Left: session list with metadata
//! - Right: preview of selected session's messages

use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use std::io;
use std::path::PathBuf;

use flint_agent::SessionMeta;
use flint_config::Config;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Strip ANSI escape sequences from text.
///
/// Claude Code sessions may embed raw ANSI codes (e.g. `\x1b[1m`) in message
/// content.  ratatui treats these as literal characters, producing garbled
/// display and corrupting width calculations.
fn strip_ansi_codes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip the ESC + everything until we leave the sequence
            if chars.peek() == Some(&'[') {
                chars.next(); // consume '['
                while let Some(&next) = chars.peek() {
                    if next.is_ascii_alphabetic() || next == 'm' {
                        chars.next();
                        break;
                    }
                    chars.next();
                }
            } else if chars.peek() == Some(&']') {
                // OSC sequence — skip until ST (\x1b\\) or BEL (\x07)
                chars.next();
                while let Some(next) = chars.next() {
                    if next == '\x07' {
                        break;
                    }
                    if next == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            // Other ESC-prefixed sequences: just drop the ESC
        } else if c.is_control() && c != '\n' && c != '\t' {
            // Drop other control characters (but keep newlines/tabs)
            continue;
        } else {
            out.push(c);
        }
    }
    out
}

/// Session entry for the resume list.
#[derive(Clone)]
struct SessionEntry {
    path: PathBuf,
    meta: SessionMeta,
    is_claude: bool,
}

/// App state for the resume TUI.
struct App {
    sessions: Vec<SessionEntry>,
    list_state: ListState,
    preview_cache: Option<(usize, Vec<String>)>, // (index, lines)
}

impl App {
    fn new(config: &Config, working_dir: &std::path::Path) -> Self {
        let sessions = Self::load_sessions(config, working_dir);
        let mut list_state = ListState::default();

        if !sessions.is_empty() {
            list_state.select(Some(0));
        }

        Self {
            sessions,
            list_state,
            preview_cache: None,
        }
    }

    fn load_sessions(config: &Config, working_dir: &std::path::Path) -> Vec<SessionEntry> {
        let mut sessions = Vec::new();

        // Load flint sessions
        let session_dir = &config.session.path;
        if session_dir.exists() {
            if let Ok(flint_sessions) = flint_agent::Session::list_sessions(session_dir) {
                for meta in flint_sessions {
                    let path = session_dir.join(format!("{}.json", meta.id));
                    if path.exists() {
                        sessions.push(SessionEntry {
                            path,
                            meta,
                            is_claude: false,
                        });
                    }
                }
            }
        }

        // Load Claude Code sessions
        if let Ok(claude_sessions) = crate::session_import::list_claude_sessions(working_dir) {
            for (path, meta) in claude_sessions {
                if path.exists() {
                    sessions.push(SessionEntry {
                        path,
                        meta,
                        is_claude: true,
                    });
                }
            }
        }

        // Sort by updated_at descending
        sessions.sort_by(|a, b| b.meta.updated_at.cmp(&a.meta.updated_at));

        sessions
    }

    fn selected_session(&self) -> Option<&SessionEntry> {
        self.list_state.selected().and_then(|i| self.sessions.get(i))
    }

    fn load_preview(&mut self) {
        if let Some(idx) = self.list_state.selected() {
            if let Some(cached) = &self.preview_cache {
                if cached.0 == idx {
                    return;
                }
            }

            if let Some(entry) = self.sessions.get(idx) {
                let lines = Self::extract_preview(&entry.path, entry.is_claude);
                self.preview_cache = Some((idx, lines));
            }
        }
    }

    fn extract_preview(path: &PathBuf, is_claude: bool) -> Vec<String> {
        let mut lines = Vec::new();

        if is_claude {
            // Load Claude Code session
            if let Ok(content) = std::fs::read_to_string(path) {
                let mut message_count = 0;
                for line in content.lines() {
                    if message_count >= 50 {
                        break;
                    }

                    if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) {
                        if let Some(msg_type) = entry.get("type").and_then(|t| t.as_str()) {
                            if msg_type == "user" || msg_type == "assistant" {
                                if let Some(message) = entry.get("message") {
                                    let role = message.get("role").and_then(|r| r.as_str()).unwrap_or("");
                                    let content = message.get("content");

                                    let text = strip_ansi_codes(&Self::extract_text_from_content(content));
                                    if !text.is_empty() {
                                        let role_display = if role == "user" { "User" } else { "Assistant" };
                                        lines.push(format!("{}: {}", role_display, text));
                                        message_count += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        } else {
            // Load flint session
            if let Ok((session, _)) = flint_agent::Session::load(path) {
                for msg in session.messages.iter().take(50) {
                    let role = match msg.role {
                        flint_types::Role::User => "User",
                        flint_types::Role::Assistant => "Assistant",
                        flint_types::Role::System => "System",
                        flint_types::Role::Tool => "Tool",
                    };

                    let text = strip_ansi_codes(&msg.text());
                    if !text.is_empty() {
                        // Truncate long messages
                        let display_text: String = text.chars().take(200).collect();
                        if display_text.len() < text.len() {
                            lines.push(format!("{}: {}...", role, display_text));
                        } else {
                            lines.push(format!("{}: {}", role, display_text));
                        }
                    }
                }
            }
        }

        lines
    }

    fn extract_text_from_content(content: Option<&serde_json::Value>) -> String {
        match content {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Array(arr)) => {
                let texts: Vec<String> = arr
                    .iter()
                    .filter_map(|block| {
                        if let Some(block_type) = block.get("type").and_then(|t| t.as_str()) {
                            if block_type == "text" {
                                block.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    })
                    .collect();
                texts.join(" ")
            }
            _ => String::new(),
        }
    }

    fn move_up(&mut self) {
        if let Some(i) = self.list_state.selected() {
            if i > 0 {
                self.list_state.select(Some(i - 1));
                self.preview_cache = None;
            }
        }
    }

    fn move_down(&mut self) {
        if let Some(i) = self.list_state.selected() {
            if i < self.sessions.len().saturating_sub(1) {
                self.list_state.select(Some(i + 1));
                self.preview_cache = None;
            }
        }
    }
}

/// Draw the resume TUI.
fn draw(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(35), // Left: session list
            Constraint::Percentage(65), // Right: preview
        ])
        .split(f.area());

    draw_session_list(f, app, chunks[0]);
    draw_preview(f, app, chunks[1]);
}

/// ASCII-safe border set — avoids garbled box-drawing characters on Windows
/// ConPTY / cmd.exe where Unicode borders may render as garbage.
const ASCII_BORDER: border::Set = border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

/// Draw the session list on the left.
fn draw_session_list(f: &mut Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = app
        .sessions
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let source = if entry.is_claude { "[CC]" } else { "[F]" };
            let updated = entry
                .meta
                .updated_at
                .split('T')
                .next()
                .unwrap_or(&entry.meta.updated_at);
            // Truncate title to fit available space
            let max_title_width = (area.width as usize).saturating_sub(20); // Reserve space for other fields
            let title: String = entry.meta.title.chars().take(max_title_width).collect();

            let style = if Some(i) == app.list_state.selected() {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:<4}", source),
                    Style::default().fg(if entry.is_claude { Color::Cyan } else { Color::Green }),
                ),
                Span::styled(
                    title,
                    style,
                ),
                Span::styled(
                    format!(" {} ", updated),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("[{}]", entry.meta.message_count),
                    Style::default().fg(Color::Yellow),
                ),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .title(Span::styled(
                    " Sessions ",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_set(ASCII_BORDER)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    f.render_stateful_widget(list, area, &mut app.list_state);
}

/// Draw the preview on the right.
fn draw_preview(f: &mut Frame, app: &mut App, area: Rect) {
    let preview_lines = if let Some((_, lines)) = &app.preview_cache {
        lines.clone()
    } else {
        Vec::new()
    };

    let text: Vec<Line> = if preview_lines.is_empty() {
        vec![
            Line::from(""),
            Line::from(Span::styled(
                "  No preview available",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  Select a session to preview",
                Style::default().fg(Color::DarkGray),
            )),
        ]
    } else {
        preview_lines
            .iter()
            .map(|line| {
                if line.starts_with("User:") {
                    Line::from(vec![
                        Span::styled(
                            "User: ",
                            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(line.strip_prefix("User: ").unwrap_or(line)),
                    ])
                } else if line.starts_with("Assistant:") {
                    Line::from(vec![
                        Span::styled(
                            "Assistant: ",
                            Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(line.strip_prefix("Assistant: ").unwrap_or(line)),
                    ])
                } else {
                    Line::from(Span::raw(line.as_str()))
                }
            })
            .collect()
    };

    let title = if let Some(entry) = app.selected_session() {
        let source = if entry.is_claude { "Claude Code" } else { "Flint" };
        format!(" {} - {} ", source, entry.meta.title)
    } else {
        " Preview ".to_string()
    };

    let paragraph = Paragraph::new(text)
        .block(
            Block::default()
                .title(Span::styled(
                    title,
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_set(ASCII_BORDER)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .wrap(Wrap { trim: false });

    f.render_widget(paragraph, area);
}

/// Run the resume TUI.
/// Returns `Ok(Some(session_path))` if a session was selected, `Ok(None)` if cancelled.
pub fn run(config: &Config, working_dir: &std::path::Path) -> Result<Option<(PathBuf, SessionMeta)>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(config, working_dir);
    let result = run_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<Option<(PathBuf, SessionMeta)>> {
    // Initial clear to prevent artifacts
    terminal.clear()?;

    loop {
        // Load preview if needed
        app.load_preview();

        // Force full redraw to prevent Windows buffer issues
        terminal.draw(|f| draw(f, app))?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            match key.code {
                KeyCode::Esc => return Ok(None),
                KeyCode::Up | KeyCode::Char('k') => {
                    app.move_up();
                    // Force redraw after navigation
                    terminal.draw(|f| draw(f, app))?;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    app.move_down();
                    // Force redraw after navigation
                    terminal.draw(|f| draw(f, app))?;
                }
                KeyCode::Enter => {
                    if let Some(entry) = app.selected_session() {
                        return Ok(Some((entry.path.clone(), entry.meta.clone())));
                    }
                }
                _ => {}
            }
        }
    }
}
