//! Interactive model selection TUI.
//!
//! Launched via `/model` in the REPL. Shows provider-specific model list
//! with a custom input field for arbitrary model names.

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

// ── Model presets per provider ──────────────────────────────────────────────

struct ModelPreset {
    name: String,
    description: String,
}

fn models_for_provider(provider: &str) -> Vec<ModelPreset> {
    match provider {
        "openai" => vec![
            ModelPreset { name: "gpt-4o".into(), description: "Fast, capable".into() },
            ModelPreset { name: "gpt-4o-mini".into(), description: "Cheapest GPT-4 class".into() },
            ModelPreset { name: "gpt-4-turbo".into(), description: "GPT-4 with tools".into() },
            ModelPreset { name: "o1".into(), description: "Reasoning model".into() },
            ModelPreset { name: "o1-mini".into(), description: "Fast reasoning".into() },
        ],
        "anthropic" => vec![
            ModelPreset { name: "claude-sonnet-4-20250514".into(), description: "Best balance".into() },
            ModelPreset { name: "claude-3-5-haiku-20241022".into(), description: "Fast & cheap".into() },
            ModelPreset { name: "claude-3-opus-20240229".into(), description: "Most capable".into() },
        ],
        _ => vec![],
    }
}

// ── App state ───────────────────────────────────────────────────────────────

struct App {
    presets: Vec<ModelPreset>,
    list_state: ListState,
    current_model: String,
    custom_input: String,
    input_mode: bool, // true = typing custom model name
    selected: Option<String>,
    is_custom: bool, // true if the selected model was entered via custom input
}

impl App {
    fn new(provider: &str, current_model: &str) -> Self {
        let mut presets = models_for_provider(provider);
        let mut list_state = ListState::default();

        // If current model is not in presets, add it at the top
        if !presets.iter().any(|p| p.name == current_model) {
            presets.insert(0, ModelPreset {
                name: current_model.to_string(),
                description: "Current (custom)".into(),
            });
        }

        // Pre-select current model
        let idx = presets.iter().position(|p| p.name == current_model);
        list_state.select(idx.or(Some(0)));

        Self {
            presets,
            list_state,
            current_model: current_model.to_string(),
            custom_input: String::new(),
            input_mode: false,
            selected: None,
            is_custom: false,
        }
    }

    fn move_up(&mut self) {
        if self.input_mode {
            return;
        }
        if let Some(i) = self.list_state.selected() {
            self.list_state.select(Some(i.saturating_sub(1)));
        }
    }

    fn move_down(&mut self) {
        if self.input_mode {
            return;
        }
        if let Some(i) = self.list_state.selected() {
            let next = (i + 1).min(self.presets.len()); // +1 for "custom" item
            self.list_state.select(Some(next));
        }
    }

    fn confirm(&mut self) {
        if self.input_mode {
            if !self.custom_input.trim().is_empty() {
                self.selected = Some(self.custom_input.trim().to_string());
                self.is_custom = true;
            }
            return;
        }
        if let Some(i) = self.list_state.selected() {
            if i < self.presets.len() {
                self.selected = Some(self.presets[i].name.clone());
            } else {
                // "Custom" item selected — enter input mode
                self.input_mode = true;
            }
        }
    }

    fn toggle_input_mode(&mut self) {
        if let Some(i) = self.list_state.selected() {
            if i >= self.presets.len() {
                self.input_mode = !self.input_mode;
            }
        }
    }
}

// ── Drawing ─────────────────────────────────────────────────────────────────

fn draw(f: &mut Frame, app: &mut App, provider: &str) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3),  // title
            Constraint::Min(6),    // model list
            Constraint::Length(3),  // custom input
            Constraint::Length(1),  // status bar
        ])
        .split(f.area());

    // Title — show current model
    let current_in_presets = app.presets.iter().any(|p| p.name == app.current_model);
    let title_text = if current_in_presets {
        format!("Select model — {} (current: {})", provider, app.current_model)
    } else {
        format!("Select model — {} (current: {} [custom])", provider, app.current_model)
    };
    let title = Paragraph::new(title_text)
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(title, chunks[0]);

    // Model list
    let mut items: Vec<ListItem> = app
        .presets
        .iter()
        .map(|p| {
            let is_current = p.name == app.current_model;
            let marker = if is_current { " ● " } else { "   " };
            ListItem::new(Line::from(vec![
                Span::styled(marker, if is_current {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default()
                }),
                Span::styled(
                    format!("{:<32}", p.name),
                    Style::default().fg(Color::White),
                ),
                Span::styled(p.description.clone(), Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();

    // Add "Custom model..." item at the end
    items.push(ListItem::new(Line::from(vec![
        Span::raw("   "),
        Span::styled("✎ Custom model...", Style::default().fg(Color::Yellow)),
    ])));

    let list = List::new(items)
        .block(
            Block::default()
                .title(" Presets (↑↓ select, Enter confirm) ")
                .borders(Borders::ALL),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    f.render_stateful_widget(list, chunks[1], &mut app.list_state);

    // Custom input field
    let input_style = if app.input_mode {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let display = if app.input_mode {
        if app.custom_input.is_empty() {
            "Type model name...".to_string()
        } else {
            app.custom_input.clone()
        }
    } else {
        "Press Enter on 'Custom' to type a model name".to_string()
    };
    let cursor = if app.input_mode { "▌" } else { "" };
    let input_widget = Paragraph::new(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            display,
            if app.input_mode && app.custom_input.is_empty() {
                Style::default().fg(Color::DarkGray)
            } else {
                input_style
            },
        ),
        Span::styled(cursor, Style::default().fg(Color::Cyan)),
    ]))
    .block(
        Block::default()
            .title(" Custom model ")
            .borders(Borders::ALL)
            .border_style(input_style),
    );
    f.render_widget(input_widget, chunks[2]);

    // Status bar
    let status = if app.input_mode {
        Line::from(vec![
            Span::styled("Enter", Style::default().fg(Color::Yellow)),
            Span::raw(" confirm  "),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::raw(" cancel  "),
        ])
    } else {
        Line::from(vec![
            Span::styled(" ↑↓", Style::default().fg(Color::Yellow)),
            Span::raw(" navigate  "),
            Span::styled("Enter", Style::default().fg(Color::Yellow)),
            Span::raw(" select  "),
            Span::styled("Tab", Style::default().fg(Color::Yellow)),
            Span::raw(" edit custom  "),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::raw(" cancel"),
        ])
    };
    f.render_widget(Paragraph::new(status), chunks[3]);
}

// ── Public entry point ──────────────────────────────────────────────────────

/// Run the model selection TUI.
/// Returns `Ok(Some((model_name, is_custom)))` if a model was selected, `Ok(None)` if cancelled.
/// `is_custom` is true when the model was entered via custom input (not a preset).
pub fn run(provider: &str, current_model: &str) -> Result<Option<(String, bool)>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(provider, current_model);
    let result = run_loop(&mut terminal, &mut app, provider);

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
    provider: &str,
) -> Result<Option<(String, bool)>> {
    loop {
        terminal.draw(|f| draw(f, app, provider))?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            if app.input_mode {
                match key.code {
                    KeyCode::Esc => {
                        app.input_mode = false;
                        app.custom_input.clear();
                    }
                    KeyCode::Enter => {
                        app.confirm();
                        if let Some(model) = &app.selected {
                            return Ok(Some((model.clone(), app.is_custom)));
                        }
                    }
                    KeyCode::Backspace => {
                        app.custom_input.pop();
                    }
                    KeyCode::Char(c) => {
                        app.custom_input.push(c);
                    }
                    _ => {}
                }
            } else {
                match key.code {
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Up | KeyCode::Char('k') => app.move_up(),
                    KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                    KeyCode::Enter => {
                        app.confirm();
                        if let Some(model) = &app.selected {
                            return Ok(Some((model.clone(), app.is_custom)));
                        }
                    }
                    KeyCode::Tab => {
                        app.toggle_input_mode();
                    }
                    _ => {}
                }
            }
        }
    }
}
