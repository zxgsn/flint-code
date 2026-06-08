//! First-run setup wizard.
//!
//! Shown when no API key is detected. Walks the user through
//! selecting a provider and entering credentials, then saves
//! to `.env`.

use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};
use std::io;
use std::path::Path;

// ── Provider definitions ────────────────────────────────────────────────────

pub(super) struct ProviderDef {
    pub(super) name: &'static str,
    pub(super) label: &'static str,
    pub(super) env_key: &'static str,
    pub(super) env_base: &'static str,
    pub(super) env_model: &'static str,
    default_base: &'static str,
    default_model: &'static str,
    description: &'static str,
}

const PROVIDERS: &[ProviderDef] = &[
    ProviderDef {
        name: "openai",
        label: "OpenAI / OpenAI-compatible",
        env_key: "OPENAI_API_KEY",
        env_base: "OPENAI_BASE_URL",
        env_model: "FLINT_MODEL",
        default_base: "https://api.openai.com/v1",
        default_model: "gpt-4o",
        description: "OpenAI, Azure, or any OpenAI-compatible API",
    },
    ProviderDef {
        name: "anthropic",
        label: "Anthropic (Claude)",
        env_key: "ANTHROPIC_API_KEY",
        env_base: "ANTHROPIC_BASE_URL",
        env_model: "FLINT_MODEL",
        default_base: "https://api.anthropic.com",
        default_model: "claude-sonnet-4-20250514",
        description: "Anthropic Claude models",
    },
];

// ── UI State ────────────────────────────────────────────────────────────────

enum SetupStep {
    /// Select a provider from the list
    SelectProvider,
    /// Enter credentials for the selected provider
    EnterCredentials { provider_idx: usize },
}

struct SetupApp {
    step: SetupStep,
    list_state: ListState,
    /// Input fields for the current provider
    fields: Vec<InputField>,
    focused_field: usize,
    saved: bool,
    error_msg: Option<String>,
}

struct InputField {
    label: &'static str,
    value: String,
    is_secret: bool,
    placeholder: &'static str,
}

impl SetupApp {
    fn new() -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        Self {
            step: SetupStep::SelectProvider,
            list_state,
            fields: Vec::new(),
            focused_field: 0,
            saved: false,
            error_msg: None,
        }
    }

    fn select_provider(&mut self, idx: usize) {
        let p = &PROVIDERS[idx];
        // Pre-populate from current environment variables
        let current_key = std::env::var(p.env_key).unwrap_or_default();
        let current_base = std::env::var(p.env_base).ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| p.default_base.to_string());
        let current_model = std::env::var(p.env_model).ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| p.default_model.to_string());

        self.fields = vec![
            InputField {
                label: "API Key",
                value: current_key,
                is_secret: true,
                placeholder: "sk-...",
            },
            InputField {
                label: "Base URL",
                value: current_base,
                is_secret: false,
                placeholder: p.default_base,
            },
            InputField {
                label: "Model",
                value: current_model,
                is_secret: false,
                placeholder: p.default_model,
            },
        ];
        self.focused_field = 0;
        self.step = SetupStep::EnterCredentials { provider_idx: idx };
    }

    fn go_back(&mut self) {
        self.step = SetupStep::SelectProvider;
        self.error_msg = None;
    }

    fn current_field(&mut self) -> Option<&mut InputField> {
        self.fields.get_mut(self.focused_field)
    }

    fn next_field(&mut self) {
        if self.focused_field < self.fields.len() - 1 {
            self.focused_field += 1;
        }
    }

    fn prev_field(&mut self) {
        if self.focused_field > 0 {
            self.focused_field -= 1;
        }
    }

    fn validate_and_save(&mut self, env_path: &Path) -> Result<()> {
        let api_key = self.fields[0].value.trim();
        if api_key.is_empty() {
            self.error_msg = Some("API Key cannot be empty".to_string());
            return Ok(());
        }

        let provider_idx = match self.step {
            SetupStep::EnterCredentials { provider_idx } => provider_idx,
            _ => return Ok(()),
        };
        let p = &PROVIDERS[provider_idx];

        let base_url = self.fields[1].value.trim();
        let model = self.fields[2].value.trim();

        // Build .env content
        let mut lines = Vec::new();
        lines.push(format!("# flint configuration — generated by setup wizard"));
        lines.push(format!("FLINT_PROVIDER={}", p.name));
        lines.push(format!("{}={}", p.env_key, api_key));
        if !base_url.is_empty() && base_url != p.default_base {
            lines.push(format!("{}={}", p.env_base, base_url));
        }
        if !model.is_empty() && model != p.default_model {
            lines.push(format!("{}={}", p.env_model, model));
        }

        // Append to existing .env or create new
        let content = if env_path.exists() {
            let existing = std::fs::read_to_string(env_path)?;
            // Remove old entries for this provider
            let filtered: String = existing
                .lines()
                .filter(|line| {
                    !line.starts_with(&format!("{}=", p.env_key))
                        && !line.starts_with(&format!("{}=", p.env_base))
                        && !line.starts_with(&format!("{}=", p.env_model))
                        && !line.starts_with("FLINT_PROVIDER=")
                        && !line.starts_with("FLINT_MODEL=")
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("{}\n{}\n", filtered.trim_end(), lines.join("\n"))
        } else {
            format!("{}\n", lines.join("\n"))
        };

        std::fs::write(env_path, content)?;
        self.saved = true;
        Ok(())
    }
}

// ── Drawing ─────────────────────────────────────────────────────────────────

fn draw(f: &mut Frame, app: &mut SetupApp) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3),  // title
            Constraint::Min(8),    // content
            Constraint::Length(1),  // status bar
        ])
        .split(f.area());

    // Title
    let title = Paragraph::new("flint setup")
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(title, chunks[0]);

    match app.step {
        SetupStep::SelectProvider => draw_provider_list(f, app, chunks[1]),
        SetupStep::EnterCredentials { provider_idx } => {
            draw_credentials_form(f, app, provider_idx, chunks[1])
        }
    }

    // Status bar
    let status = match app.step {
        SetupStep::SelectProvider => Line::from(vec![
            Span::styled(" ↑↓", Style::default().fg(Color::Yellow)),
            Span::raw(" navigate  "),
            Span::styled("Enter", Style::default().fg(Color::Yellow)),
            Span::raw(" select  "),
            Span::styled("q", Style::default().fg(Color::Yellow)),
            Span::raw(" quit"),
        ]),
        SetupStep::EnterCredentials { .. } => Line::from(vec![
            Span::styled("Tab", Style::default().fg(Color::Yellow)),
            Span::raw(" next field  "),
            Span::styled("S-Tab", Style::default().fg(Color::Yellow)),
            Span::raw(" prev field  "),
            Span::styled("Enter", Style::default().fg(Color::Yellow)),
            Span::raw(" save  "),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::raw(" back"),
        ]),
    };
    let status_bar = Paragraph::new(status);
    f.render_widget(status_bar, chunks[2]);
}

fn draw_provider_list(f: &mut Frame, app: &mut SetupApp, area: ratatui::layout::Rect) {
    let items: Vec<ListItem> = PROVIDERS
        .iter()
        .map(|p| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("  {:<28}", p.label),
                    Style::default().fg(Color::White),
                ),
                Span::styled(p.description, Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .title(" Select provider ")
                .borders(Borders::ALL),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    f.render_stateful_widget(list, area, &mut app.list_state);
}

fn draw_credentials_form(
    f: &mut Frame,
    app: &mut SetupApp,
    provider_idx: usize,
    area: ratatui::layout::Rect,
) {
    let p = &PROVIDERS[provider_idx];

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            std::iter::once(Constraint::Length(2)) // provider name
                .chain(app.fields.iter().map(|_| Constraint::Length(3))) // fields
                .chain(std::iter::once(Constraint::Length(2))) // error/success
                .chain(std::iter::once(Constraint::Min(0))) // spacer
                .collect::<Vec<_>>(),
        )
        .split(area);

    // Provider name
    let header = Paragraph::new(format!("  {}", p.label)).style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    f.render_widget(header, chunks[0]);

    // Input fields
    for (i, field) in app.fields.iter().enumerate() {
        let is_focused = i == app.focused_field;
        let border_style = if is_focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let display_value = if field.is_secret && !field.value.is_empty() {
            "*".repeat(field.value.len())
        } else if field.value.is_empty() {
            field.placeholder.to_string()
        } else {
            field.value.clone()
        };

        let text_style = if field.value.is_empty() {
            Style::default().fg(Color::DarkGray)
        } else if field.is_secret {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::White)
        };

        let input = Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(display_value, text_style),
            if is_focused {
                Span::styled("▌", Style::default().fg(Color::Cyan))
            } else {
                Span::raw("")
            },
        ]))
        .block(
            Block::default()
                .title(format!(" {} ", field.label))
                .borders(Borders::ALL)
                .border_style(border_style),
        );
        f.render_widget(input, chunks[1 + i]);
    }

    // Error message or hint
    let msg_area = chunks[1 + app.fields.len()];
    if let Some(err) = &app.error_msg {
        let err_para = Paragraph::new(format!("  ✗ {}", err))
            .style(Style::default().fg(Color::Red));
        f.render_widget(err_para, msg_area);
    }
}

// ── Public entry point ──────────────────────────────────────────────────────

/// Find a provider definition by name.
pub(super) fn find_provider(name: &str) -> Option<&'static ProviderDef> {
    PROVIDERS.iter().find(|p| p.name == name)
}

/// Run the setup wizard. Returns `Ok(true)` if a provider was configured.
pub fn run(env_path: &Path) -> Result<bool> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = SetupApp::new();
    let result = run_loop(&mut terminal, &mut app, env_path);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    match result {
        Ok(()) => {
            if app.saved {
                println!("✓ Provider configured. Credentials saved to {}", env_path.display());
                Ok(true)
            } else {
                println!("Setup cancelled.");
                Ok(false)
            }
        }
        Err(e) => Err(e),
    }
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut SetupApp,
    env_path: &Path,
) -> Result<()> {
    loop {
        terminal.draw(|f| draw(f, app))?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            match app.step {
                SetupStep::SelectProvider => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Up | KeyCode::Char('k') => {
                        app.list_state.select(Some(
                            app.list_state.selected().unwrap_or(0).saturating_sub(1),
                        ));
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let next = app.list_state.selected().unwrap_or(0) + 1;
                        if next < PROVIDERS.len() {
                            app.list_state.select(Some(next));
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(idx) = app.list_state.selected() {
                            app.select_provider(idx);
                        }
                    }
                    _ => {}
                },
                SetupStep::EnterCredentials { .. } => match key.code {
                    KeyCode::Esc => app.go_back(),
                    KeyCode::Tab => app.next_field(),
                    KeyCode::BackTab => app.prev_field(),
                    KeyCode::Enter => {
                        app.validate_and_save(env_path)?;
                        if app.saved {
                            return Ok(());
                        }
                    }
                    KeyCode::Backspace => {
                        if let Some(field) = app.current_field() {
                            field.value.pop();
                            app.error_msg = None;
                        }
                    }
                    KeyCode::Char(c) => {
                        if let Some(field) = app.current_field() {
                            field.value.push(c);
                            app.error_msg = None;
                        }
                    }
                    _ => {}
                },
            }
        }
    }
}
