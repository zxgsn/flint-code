//! Interactive model selection TUI.
//!
//! Launched via `/model` in the REPL. Shows provider-specific model list
//! with recent custom models, delete/rename support, and a custom input field.

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

/// Check if a model name is a built-in preset for the given provider.
pub(crate) fn is_preset(provider: &str, model: &str) -> bool {
    models_for_provider(provider).iter().any(|p| p.name == model)
}

// ── Item kind in the merged list ────────────────────────────────────────────

#[derive(PartialEq)]
enum ItemKind {
    Preset,
    Recent,
}

struct ListItem_ {
    name: String,
    description: String,
    kind: ItemKind,
}

// ── App state ───────────────────────────────────────────────────────────────

struct App {
    items: Vec<ListItem_>,
    list_state: ListState,
    current_model: String,
    recent_models: Vec<String>,
    custom_input: String,
    input_mode: bool,    // true = typing custom model name
    edit_mode: bool,     // true = renaming a recent model
    edit_input: String,  // buffer for rename
    edit_target: usize,  // index in items being edited
    selected: Option<String>,
    is_custom: bool,
    /// Set to true when recent_models were modified (delete/rename).
    recent_changed: bool,
}

impl App {
    fn new(provider: &str, current_model: &str, recent_models: &[String]) -> Self {
        let presets = models_for_provider(provider);
        let mut list_state = ListState::default();

        // Build merged list: presets → recent (deduped against presets) → custom
        let mut items: Vec<ListItem_> = Vec::new();

        for p in &presets {
            items.push(ListItem_ {
                name: p.name.clone(),
                description: p.description.clone(),
                kind: ItemKind::Preset,
            });
        }

        for name in recent_models {
            if !items.iter().any(|i| &i.name == name) {
                items.push(ListItem_ {
                    name: name.clone(),
                    description: "recent".into(),
                    kind: ItemKind::Recent,
                });
            }
        }

        // If current model is not in the list yet, insert it at the boundary
        if !items.iter().any(|i| i.name == current_model) {
            items.insert(
                presets.len().min(items.len()),
                ListItem_ {
                    name: current_model.to_string(),
                    description: "current".into(),
                    kind: ItemKind::Recent,
                },
            );
        }

        // Pre-select current model
        let idx = items.iter().position(|i| i.name == current_model);
        list_state.select(idx.or(Some(0)));

        Self {
            items,
            list_state,
            current_model: current_model.to_string(),
            recent_models: recent_models.to_vec(),
            custom_input: String::new(),
            input_mode: false,
            edit_mode: false,
            edit_input: String::new(),
            edit_target: 0,
            selected: None,
            is_custom: false,
            recent_changed: false,
        }
    }

    /// Number of non-custom items (presets + recent).
    fn item_count(&self) -> usize {
        self.items.len()
    }

    fn move_up(&mut self) {
        if self.input_mode || self.edit_mode {
            return;
        }
        if let Some(i) = self.list_state.selected() {
            self.list_state.select(Some(i.saturating_sub(1)));
        }
    }

    fn move_down(&mut self) {
        if self.input_mode || self.edit_mode {
            return;
        }
        if let Some(i) = self.list_state.selected() {
            let max = self.item_count(); // points to "Custom" item
            let next = (i + 1).min(max);
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
        if self.edit_mode {
            self.finish_edit();
            return;
        }
        if let Some(i) = self.list_state.selected() {
            if i < self.items.len() {
                self.selected = Some(self.items[i].name.clone());
            } else {
                // "Custom" item selected — enter input mode
                self.input_mode = true;
            }
        }
    }

    fn toggle_input_mode(&mut self) {
        if self.edit_mode {
            return;
        }
        if let Some(i) = self.list_state.selected() {
            if i >= self.items.len() {
                self.input_mode = !self.input_mode;
            }
        }
    }

    /// Delete the selected recent model.
    fn delete_selected(&mut self) {
        if self.input_mode || self.edit_mode {
            return;
        }
        let Some(i) = self.list_state.selected() else { return; };
        if i >= self.items.len() { return; }
        if self.items[i].kind != ItemKind::Recent { return; }

        let name = self.items.remove(i).name;
        self.recent_models.retain(|m| *m != name);
        self.recent_changed = true;

        // Adjust selection
        if i >= self.items.len() && !self.items.is_empty() {
            self.list_state.select(Some(self.items.len() - 1));
        } else if self.items.is_empty() {
            self.list_state.select(Some(0));
        }
    }

    /// Enter edit mode for the selected recent model.
    fn start_edit(&mut self) {
        if self.input_mode || self.edit_mode {
            return;
        }
        let Some(i) = self.list_state.selected() else { return; };
        if i >= self.items.len() { return; }
        if self.items[i].kind != ItemKind::Recent { return; }

        self.edit_input = self.items[i].name.clone();
        self.edit_target = i;
        self.edit_mode = true;
    }

    /// Finish editing — apply the rename.
    fn finish_edit(&mut self) {
        if !self.edit_mode {
            return;
        }
        let new_name = self.edit_input.trim().to_string();
        if new_name.is_empty() || new_name == self.items[self.edit_target].name {
            self.edit_mode = false;
            self.edit_input.clear();
            return;
        }

        // Update recent_models list
        let old_name = self.items[self.edit_target].name.clone();
        if let Some(pos) = self.recent_models.iter().position(|m| *m == old_name) {
            self.recent_models[pos] = new_name.clone();
        }

        // Update items list
        self.items[self.edit_target].name = new_name;
        self.recent_changed = true;
        self.edit_mode = false;
        self.edit_input.clear();
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
            Constraint::Length(3),  // custom input / edit input
            Constraint::Length(1),  // status bar
        ])
        .split(f.area());

    // Title
    let title_text = format!(
        "Select model — {} (current: {})",
        provider, app.current_model
    );
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
        .items
        .iter()
        .map(|item| {
            let is_current = item.name == app.current_model;
            let marker = if is_current { " ● " } else { "   " };
            let name_style = match item.kind {
                ItemKind::Recent => Style::default().fg(Color::Yellow),
                _ => Style::default().fg(Color::White),
            };
            let desc_style = match item.kind {
                ItemKind::Recent => Style::default().fg(Color::DarkGray),
                _ => Style::default().fg(Color::DarkGray),
            };
            ListItem::new(Line::from(vec![
                Span::styled(marker, if is_current {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default()
                }),
                Span::styled(format!("{:<36}", item.name), name_style),
                Span::styled(&item.description, desc_style),
            ]))
        })
        .collect();

    // Add "Custom model..." item at the end
    items.push(ListItem::new(Line::from(vec![
        Span::raw("   "),
        Span::styled("✎ Custom model...", Style::default().fg(Color::Yellow)),
    ])));

    let list_title = if app.edit_mode {
        " Models (Enter confirm, Esc cancel edit) "
    } else {
        " Models (↑↓ select, d delete, e rename, Enter confirm) "
    };
    let list = List::new(items)
        .block(Block::default().title(list_title).borders(Borders::ALL))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    f.render_stateful_widget(list, chunks[1], &mut app.list_state);

    // Bottom input field — edit mode or custom input
    if app.edit_mode {
        let input_widget = Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(&app.edit_input, Style::default().fg(Color::Cyan)),
            Span::styled("▌", Style::default().fg(Color::Cyan)),
        ]))
        .block(
            Block::default()
                .title(" Rename model ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );
        f.render_widget(input_widget, chunks[2]);
    } else {
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
    }

    // Status bar
    let status = if app.input_mode {
        Line::from(vec![
            Span::styled("Enter", Style::default().fg(Color::Yellow)),
            Span::raw(" confirm  "),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::raw(" cancel"),
        ])
    } else if app.edit_mode {
        Line::from(vec![
            Span::styled("Enter", Style::default().fg(Color::Yellow)),
            Span::raw(" save  "),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::raw(" cancel"),
        ])
    } else {
        Line::from(vec![
            Span::styled(" ↑↓", Style::default().fg(Color::Yellow)),
            Span::raw(" navigate  "),
            Span::styled("Enter", Style::default().fg(Color::Yellow)),
            Span::raw(" select  "),
            Span::styled("d", Style::default().fg(Color::Red)),
            Span::raw(" delete  "),
            Span::styled("e", Style::default().fg(Color::Yellow)),
            Span::raw(" rename  "),
            Span::styled("Tab", Style::default().fg(Color::Yellow)),
            Span::raw(" custom  "),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::raw(" cancel"),
        ])
    };
    f.render_widget(Paragraph::new(status), chunks[3]);
}

// ── Public entry point ──────────────────────────────────────────────────────

/// Run the model selection TUI.
///
/// Returns `Ok(Some((model_name, is_custom, recent_models)))` if a model was
/// selected. `recent_models` is the (possibly updated) list of recent models
/// that the caller should persist.
pub fn run(
    provider: &str,
    current_model: &str,
    recent_models: &[String],
) -> Result<Option<(String, bool, Vec<String>)>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(provider, current_model, recent_models);
    let result = run_loop(&mut terminal, &mut app, provider);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    // Return the model choice along with the (possibly modified) recent list
    result.map(|opt| {
        opt.map(|(model, is_custom)| {
            // If the user selected a model, ensure it's in recent
            let mut recent = app.recent_models;
            if is_custom || !is_preset(provider, &model) {
                if !recent.contains(&model) {
                    recent.push(model.clone());
                }
            }
            (model, is_custom, recent)
        })
    })
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
            } else if app.edit_mode {
                match key.code {
                    KeyCode::Esc => {
                        app.edit_mode = false;
                        app.edit_input.clear();
                    }
                    KeyCode::Enter => {
                        app.finish_edit();
                    }
                    KeyCode::Backspace => {
                        app.edit_input.pop();
                    }
                    KeyCode::Char(c) => {
                        app.edit_input.push(c);
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
                    KeyCode::Char('d') => {
                        app.delete_selected();
                    }
                    KeyCode::Char('e') => {
                        app.start_edit();
                    }
                    _ => {}
                }
            }
        }
    }
}
