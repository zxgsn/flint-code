//! Interactive TUI settings panel for flint.
//!
//! Launched via `flint config`. Shows provider info, agent settings,
//! and feature toggles. Features can be toggled with Space, saved with `s`.

use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use flint_config::{Config, Feature};
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

// ── Feature entry for the UI ────────────────────────────────────────────────

struct FeatureItem {
    feature: Feature,
    label: &'static str,
    description: &'static str,
    enabled: bool,
}

// ── App state ───────────────────────────────────────────────────────────────

struct App {
    config: Config,
    features: Vec<FeatureItem>,
    list_state: ListState,
    saved: bool,
}

impl App {
    fn new(config: Config) -> Self {
        let features = vec![
            FeatureItem {
                feature: Feature::Skills,
                label: "Skills",
                description: "Reusable prompt modules",
                enabled: config.features.is_enabled(Feature::Skills),
            },
            FeatureItem {
                feature: Feature::Memory,
                label: "Memory",
                description: "Cross-session learning",
                enabled: config.features.is_enabled(Feature::Memory),
            },
            FeatureItem {
                feature: Feature::Compaction,
                label: "Compaction",
                description: "Context window management",
                enabled: config.features.is_enabled(Feature::Compaction),
            },
            FeatureItem {
                feature: Feature::Permissions,
                label: "Permissions",
                description: "Safety confirmations",
                enabled: config.features.is_enabled(Feature::Permissions),
            },
            FeatureItem {
                feature: Feature::Swarm,
                label: "Swarm",
                description: "Multi-agent coordination",
                enabled: config.features.is_enabled(Feature::Swarm),
            },
        ];

        let mut list_state = ListState::default();
        list_state.select(Some(0));

        Self {
            config,
            features,
            list_state,
            saved: false,
        }
    }

    fn toggle_selected(&mut self) {
        if let Some(i) = self.list_state.selected() {
            self.features[i].enabled = !self.features[i].enabled;
        }
    }

    fn move_up(&mut self) {
        if let Some(i) = self.list_state.selected() {
            if i > 0 {
                self.list_state.select(Some(i - 1));
            }
        }
    }

    fn move_down(&mut self) {
        if let Some(i) = self.list_state.selected() {
            if i < self.features.len() - 1 {
                self.list_state.select(Some(i + 1));
            }
        }
    }

    fn apply_to_config(&mut self) {
        for item in &self.features {
            match item.feature {
                Feature::Skills => self.config.features.skills.enabled = item.enabled,
                Feature::Memory => self.config.features.memory.enabled = item.enabled,
                Feature::Compaction => self.config.features.compaction.enabled = item.enabled,
                Feature::Permissions => self.config.features.permissions.enabled = item.enabled,
                Feature::Swarm => self.config.features.swarm.enabled = item.enabled,
            }
        }
    }
}

// ── Drawing ─────────────────────────────────────────────────────────────────

fn draw(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3),  // title
            Constraint::Length(5),  // provider info
            Constraint::Length(4),  // agent info
            Constraint::Min(8),    // features list
            Constraint::Length(1),  // status bar
        ])
        .split(f.area());

    // Title
    let title = Paragraph::new("flint config")
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(title, chunks[0]);

    // Provider info (read-only)
    let provider_lines = vec![
        Line::from(vec![
            Span::styled("  Type          ", Style::default().fg(Color::Gray)),
            Span::styled(
                app.config.provider.r#type.clone(),
                Style::default().fg(Color::Green),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Model         ", Style::default().fg(Color::Gray)),
            Span::styled(
                app.config.provider.model.clone(),
                Style::default().fg(Color::Green),
            ),
        ]),
    ];
    let provider_block = Paragraph::new(provider_lines)
        .block(Block::default().title(" Provider ").borders(Borders::ALL));
    f.render_widget(provider_block, chunks[1]);

    // Agent info (read-only)
    let agent_lines = vec![
        Line::from(vec![
            Span::styled("  Max turns     ", Style::default().fg(Color::Gray)),
            Span::styled(
                app.config.agent.max_turns.to_string(),
                Style::default().fg(Color::Green),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Max output    ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("{} chars", app.config.agent.max_output_chars),
                Style::default().fg(Color::Green),
            ),
        ]),
    ];
    let agent_block = Paragraph::new(agent_lines)
        .block(Block::default().title(" Agent ").borders(Borders::ALL));
    f.render_widget(agent_block, chunks[2]);

    // Features list (interactive)
    let items: Vec<ListItem> = app
        .features
        .iter()
        .map(|item| {
            let checkbox = if item.enabled {
                Span::styled(" ✓ ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))
            } else {
                Span::styled(" ✗ ", Style::default().fg(Color::DarkGray))
            };
            let label = Span::styled(
                format!("{:<16}", item.label),
                if item.enabled {
                    Style::default().fg(Color::White)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            );
            let desc = Span::styled(
                item.description,
                Style::default().fg(Color::DarkGray),
            );
            ListItem::new(Line::from(vec![
                Span::raw(" "),
                checkbox,
                label,
                desc,
            ]))
        })
        .collect();

    let features_list = List::new(items)
        .block(
            Block::default()
                .title(" Features (Space to toggle) ")
                .borders(Borders::ALL),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    f.render_stateful_widget(features_list, chunks[3], &mut app.list_state);

    // Status bar
    let status = Line::from(vec![
        Span::styled(" ↑↓", Style::default().fg(Color::Yellow)),
        Span::raw(" navigate  "),
        Span::styled("Space", Style::default().fg(Color::Yellow)),
        Span::raw(" toggle  "),
        Span::styled("s", Style::default().fg(Color::Yellow)),
        Span::raw(" save  "),
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(" quit"),
    ]);
    let status_bar = Paragraph::new(status);
    f.render_widget(status_bar, chunks[4]);
}

// ── Public entry point ──────────────────────────────────────────────────────

/// Run the interactive config TUI.
///
/// Returns `Ok(true)` if the user saved changes, `Ok(false)` if they quit without saving.
pub fn run(config: Config, save_path: &Path) -> Result<bool> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(config);
    let result = run_loop(&mut terminal, &mut app);

    // Restore terminal
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
                app.apply_to_config();
                app.config.save(save_path)?;
                println!("✓ Config saved to {}", save_path.display());
                Ok(true)
            } else {
                println!("Config unchanged.");
                Ok(false)
            }
        }
        Err(e) => Err(e),
    }
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    loop {
        terminal.draw(|f| draw(f, app))?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('s') => {
                    app.saved = true;
                    return Ok(());
                }
                KeyCode::Up | KeyCode::Char('k') => app.move_up(),
                KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                KeyCode::Char(' ') | KeyCode::Enter => app.toggle_selected(),
                _ => {}
            }
        }
    }
}
