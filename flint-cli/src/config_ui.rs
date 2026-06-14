//! Interactive TUI settings panel for flint.
//!
//! Launched via `flint config`. Shows provider info, agent settings,
//! and feature toggles. Features can be toggled with Space, saved with `s`.
//! Swarm feature has a detail page (Enter to open) for sub-agent configuration.

use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use flint_config::{AgentProfile, Config, Feature};
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

// ── Swarm detail page ───────────────────────────────────────────────────────

const SPAWN_MODES: &[&str] = &["terminal", "in-process"];
const MODEL_SELECTIONS: &[&str] = &["auto", "slots", "fixed"];

#[derive(PartialEq)]
enum DetailMode {
    Normal,
    AddProfile,
    EditProfile,
}

struct SwarmDetail {
    // Settings fields
    model: String,
    max_agents: String,
    agent_max_turns: String,
    spawn_mode: String,
    model_selection: String,

    // Agent slot management
    profiles: Vec<AgentProfile>,
    focus: usize,       // 0-4: settings, 5+: slot index (focus - 5)
    mode: DetailMode,

    // Input state for add/edit (model only)
    input_model: String,
    edit_index: usize,  // which slot is being edited

    // Model suggestions from recent_models
    recent_models: Vec<String>,
    suggestions: Vec<usize>,   // indices into recent_models matching input
    suggestion_idx: usize,     // currently highlighted suggestion
}

impl SwarmDetail {
    fn from_config(config: &Config) -> Self {
        // Collect all known models: recent_models + default model + slot models
        let mut recent: Vec<String> = config.provider.recent_models.clone();
        // Add the default swarm model if set and not already in list
        if let Some(ref m) = config.features.swarm.model {
            if !recent.contains(m) {
                recent.push(m.clone());
            }
        }
        // Add models from existing slots
        for agent in &config.features.swarm.agents {
            if !agent.model.is_empty() && !recent.contains(&agent.model) {
                recent.push(agent.model.clone());
            }
        }
        // Add the main provider model
        if !recent.contains(&config.provider.model) {
            recent.push(config.provider.model.clone());
        }

        Self {
            model: config.features.swarm.model.clone().unwrap_or_default(),
            max_agents: config.features.swarm.max_agents.to_string(),
            agent_max_turns: config.features.swarm.agent_max_turns.to_string(),
            spawn_mode: config.features.swarm.spawn_mode.clone(),
            model_selection: config.features.swarm.model_selection.clone(),
            profiles: config.features.swarm.agents.clone(),
            focus: 0,
            mode: DetailMode::Normal,
            input_model: String::new(),
            edit_index: 0,
            recent_models: recent,
            suggestions: Vec::new(),
            suggestion_idx: 0,
        }
    }

    fn apply_to_config(&self, config: &mut Config) {
        config.features.swarm.model = if self.model.trim().is_empty() {
            None
        } else {
            Some(self.model.trim().to_string())
        };
        if let Ok(v) = self.max_agents.trim().parse::<usize>() {
            if v > 0 {
                config.features.swarm.max_agents = v;
            }
        }
        if let Ok(v) = self.agent_max_turns.trim().parse::<u32>() {
            if v > 0 {
                config.features.swarm.agent_max_turns = v;
            }
        }
        config.features.swarm.spawn_mode = self.spawn_mode.clone();
        config.features.swarm.model_selection = self.model_selection.clone();
        config.features.swarm.agents = self.profiles.clone();
    }

    /// Total focusable items: 5 settings + N profiles + 1 add button.
    fn focus_count(&self) -> usize {
        5 + self.profiles.len() + 1
    }

    fn move_up(&mut self) {
        self.focus = self.focus.saturating_sub(1);
    }

    fn move_down(&mut self) {
        if self.focus + 1 < self.focus_count() {
            self.focus += 1;
        }
    }

    fn is_on_setting(&self) -> bool {
        self.focus < 5
    }

    fn is_on_profile(&self) -> bool {
        self.focus >= 5 && self.focus - 5 < self.profiles.len()
    }

    fn is_on_add(&self) -> bool {
        self.focus == 5 + self.profiles.len()
    }

    fn profile_index(&self) -> usize {
        self.focus - 5
    }

    fn handle_char(&mut self, c: char) {
        match self.mode {
            DetailMode::AddProfile | DetailMode::EditProfile => {
                self.input_model.push(c);
                self.update_suggestions();
            }
            DetailMode::Normal => {
                if self.is_on_setting() {
                    match self.focus {
                        0 => self.model.push(c),
                        1 => {
                            if c.is_ascii_digit() {
                                self.max_agents.push(c);
                            }
                        }
                        2 => {
                            if c.is_ascii_digit() {
                                self.agent_max_turns.push(c);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    fn handle_backspace(&mut self) {
        match self.mode {
            DetailMode::AddProfile | DetailMode::EditProfile => {
                self.input_model.pop();
                self.update_suggestions();
            }
            DetailMode::Normal => {
                if self.is_on_setting() {
                    match self.focus {
                        0 => { self.model.pop(); }
                        1 => { self.max_agents.pop(); }
                        2 => { self.agent_max_turns.pop(); }
                        _ => {}
                    }
                }
            }
        }
    }

    fn cycle_focused(&mut self) {
        if self.is_on_setting() {
            match self.focus {
                3 => {
                    let idx = SPAWN_MODES.iter().position(|m| *m == self.spawn_mode).unwrap_or(0);
                    self.spawn_mode = SPAWN_MODES[(idx + 1) % SPAWN_MODES.len()].to_string();
                }
                4 => {
                    let idx = MODEL_SELECTIONS.iter().position(|m| *m == self.model_selection).unwrap_or(0);
                    self.model_selection = MODEL_SELECTIONS[(idx + 1) % MODEL_SELECTIONS.len()].to_string();
                }
                _ => {}
            }
        }
    }

    fn start_add(&mut self) {
        self.input_model.clear();
        self.update_suggestions();
        self.mode = DetailMode::AddProfile;
    }

    fn start_edit(&mut self) {
        if !self.is_on_profile() { return; }
        let idx = self.profile_index();
        self.input_model = self.profiles[idx].model.clone();
        self.edit_index = idx;
        self.update_suggestions();
        self.mode = DetailMode::EditProfile;
    }

    /// Filter recent_models by prefix match with current input.
    fn update_suggestions(&mut self) {
        let prefix = self.input_model.to_lowercase();
        if prefix.is_empty() {
            // Show all when input is empty
            self.suggestions = (0..self.recent_models.len()).collect();
        } else {
            self.suggestions = self.recent_models.iter().enumerate()
                .filter(|(_, m)| m.to_lowercase().starts_with(&prefix))
                .map(|(i, _)| i)
                .collect();
        }
        self.suggestion_idx = 0;
    }

    /// Accept the currently highlighted suggestion into the input.
    fn accept_suggestion(&mut self) {
        if let Some(&idx) = self.suggestions.get(self.suggestion_idx) {
            self.input_model = self.recent_models[idx].clone();
        }
    }

    fn suggestion_up(&mut self) {
        if !self.suggestions.is_empty() {
            self.suggestion_idx = self.suggestion_idx.saturating_sub(1);
        }
    }

    fn suggestion_down(&mut self) {
        if !self.suggestions.is_empty() {
            self.suggestion_idx = (self.suggestion_idx + 1).min(self.suggestions.len() - 1);
        }
    }

    fn confirm_input(&mut self) {
        let model = self.input_model.trim().to_string();
        match self.mode {
            DetailMode::AddProfile => {
                if !model.is_empty() && !self.recent_models.contains(&model) {
                    self.recent_models.push(model.clone());
                }
                self.profiles.push(AgentProfile { model });
                self.focus = 5 + self.profiles.len() - 1;
            }
            DetailMode::EditProfile => {
                if !model.is_empty() && !self.recent_models.contains(&model) {
                    self.recent_models.push(model.clone());
                }
                self.profiles[self.edit_index] = AgentProfile { model };
            }
            _ => {}
        }
        self.mode = DetailMode::Normal;
    }

    fn cancel_input(&mut self) {
        self.mode = DetailMode::Normal;
    }

    fn delete_profile(&mut self) {
        if !self.is_on_profile() { return; }
        let idx = self.profile_index();
        self.profiles.remove(idx);
        // Adjust focus
        if self.profiles.is_empty() {
            self.focus = 5; // land on add button
        } else if idx >= self.profiles.len() {
            self.focus = 5 + self.profiles.len() - 1;
        }
    }

    fn move_profile_up(&mut self) {
        if !self.is_on_profile() { return; }
        let idx = self.profile_index();
        if idx > 0 {
            self.profiles.swap(idx, idx - 1);
            self.focus -= 1;
        }
    }

    fn move_profile_down(&mut self) {
        if !self.is_on_profile() { return; }
        let idx = self.profile_index();
        if idx + 1 < self.profiles.len() {
            self.profiles.swap(idx, idx + 1);
            self.focus += 1;
        }
    }
}

// ── Provider detail page ────────────────────────────────────────────────────

struct ProviderDetail {
    provider_type: String,
    model: String,
    api_key: String,
    base_url: String,
    env_path: std::path::PathBuf,
    focus: usize, // 0: type, 1: model, 2: api_key, 3: base_url
    show_key: bool,
    // Model suggestions
    recent_models: Vec<String>,
    suggestions: Vec<usize>,
    suggestion_idx: usize,
    editing: bool, // true when editing a text field
}

impl ProviderDetail {
    fn from_config(config: &Config, env_path: std::path::PathBuf) -> Self {
        let provider_type = config.provider.r#type.clone();
        let model = config.provider.model.clone();

        // Read current API key and base URL from env
        let api_key_env = match provider_type.as_str() {
            "anthropic" => "ANTHROPIC_API_KEY",
            _ => "OPENAI_API_KEY",
        };
        let base_url_env = match provider_type.as_str() {
            "anthropic" => "ANTHROPIC_BASE_URL",
            _ => "OPENAI_BASE_URL",
        };
        let api_key = std::env::var(api_key_env).unwrap_or_default();
        let base_url = std::env::var(base_url_env).unwrap_or_default();

        let mut recent: Vec<String> = config.provider.recent_models.clone();
        if !recent.contains(&model) {
            recent.push(model.clone());
        }

        Self {
            provider_type,
            model,
            api_key,
            base_url,
            env_path,
            focus: 0,
            show_key: false,
            recent_models: recent,
            suggestions: Vec::new(),
            suggestion_idx: 0,
            editing: false,
        }
    }

    fn apply(&self, config: &mut Config) {
        config.provider.r#type = self.provider_type.clone();
        config.provider.model = self.model.clone();

        // Determine env var names based on provider type
        let key_var = match self.provider_type.as_str() {
            "anthropic" => "ANTHROPIC_API_KEY",
            _ => "OPENAI_API_KEY",
        };
        let url_var = match self.provider_type.as_str() {
            "anthropic" => "ANTHROPIC_BASE_URL",
            _ => "OPENAI_BASE_URL",
        };

        let mut updates = vec![(key_var.to_string(), self.api_key.clone())];
        if !self.base_url.is_empty() {
            updates.push((url_var.to_string(), self.base_url.clone()));
        }
        crate::provider::update_env_file(&self.env_path, &updates);
    }

    fn _field_label(idx: usize) -> &'static str {
        match idx {
            0 => "Type",
            1 => "Model",
            2 => "API Key",
            3 => "Base URL",
            _ => "",
        }
    }

    fn field_value(&self, idx: usize) -> &str {
        match idx {
            0 => &self.provider_type,
            1 => &self.model,
            2 => &self.api_key,
            3 => &self.base_url,
            _ => "",
        }
    }

    fn field_value_mut(&mut self, idx: usize) -> &mut String {
        match idx {
            0 => &mut self.provider_type,
            1 => &mut self.model,
            2 => &mut self.api_key,
            3 => &mut self.base_url,
            _ => unreachable!(),
        }
    }

    fn cycle_type(&mut self) {
        self.provider_type = match self.provider_type.as_str() {
            "openai" => "anthropic".to_string(),
            _ => "openai".to_string(),
        };
        // Update env var names for the new provider type
        self.api_key = {
            let var = match self.provider_type.as_str() {
                "anthropic" => "ANTHROPIC_API_KEY",
                _ => "OPENAI_API_KEY",
            };
            std::env::var(var).unwrap_or_default()
        };
        self.base_url = {
            let var = match self.provider_type.as_str() {
                "anthropic" => "ANTHROPIC_BASE_URL",
                _ => "OPENAI_BASE_URL",
            };
            std::env::var(var).unwrap_or_default()
        };
    }

    fn update_suggestions(&mut self) {
        let field = self.field_value(self.focus);
        if field.is_empty() {
            self.suggestions = (0..self.recent_models.len()).collect();
        } else {
            let lower = field.to_lowercase();
            self.suggestions = self.recent_models
                .iter()
                .enumerate()
                .filter(|(_, m)| m.to_lowercase().contains(&lower))
                .map(|(i, _)| i)
                .collect();
        }
        self.suggestion_idx = 0;
    }
}

// ── App state ───────────────────────────────────────────────────────────────

enum Page {
    Main,
    SwarmDetail,
    ProviderDetail,
}

struct App {
    config: Config,
    features: Vec<FeatureItem>,
    list_state: ListState,
    saved: bool,
    page: Page,
    swarm_detail: SwarmDetail,
    provider_detail: ProviderDetail,
}

impl App {
    fn new(config: Config, env_path: std::path::PathBuf) -> Self {
        let swarm_detail = SwarmDetail::from_config(&config);
        let provider_detail = ProviderDetail::from_config(&config, env_path);
        let features = vec![
            FeatureItem {
                feature: Feature::Provider,
                label: "Provider",
                description: "API key, base URL, model",
                enabled: false, // not a toggle
            },
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
            FeatureItem {
                feature: Feature::AutoPoke,
                label: "Auto-poke",
                description: "Auto follow-up on incomplete todos",
                enabled: config.features.is_enabled(Feature::AutoPoke),
            },
        ];

        let mut list_state = ListState::default();
        list_state.select(Some(0));

        Self {
            config,
            features,
            list_state,
            saved: false,
            page: Page::Main,
            swarm_detail,
            provider_detail,
        }
    }

    fn toggle_selected(&mut self) {
        if let Some(i) = self.list_state.selected() {
            if self.features[i].feature != Feature::Provider {
                self.features[i].enabled = !self.features[i].enabled;
            }
        }
    }

    fn open_selected(&mut self) {
        if let Some(i) = self.list_state.selected() {
            match self.features[i].feature {
                Feature::Swarm => {
                    self.swarm_detail = SwarmDetail::from_config(&self.config);
                    self.page = Page::SwarmDetail;
                }
                Feature::Provider => {
                    self.provider_detail = ProviderDetail::from_config(
                        &self.config,
                        self.provider_detail.env_path.clone(),
                    );
                    self.page = Page::ProviderDetail;
                }
                _ => {}
            }
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
                Feature::Provider => {} // handled by provider_detail
                Feature::Skills => self.config.features.skills.enabled = item.enabled,
                Feature::Memory => self.config.features.memory.enabled = item.enabled,
                Feature::Compaction => self.config.features.compaction.enabled = item.enabled,
                Feature::Permissions => self.config.features.permissions.enabled = item.enabled,
                Feature::Swarm => self.config.features.swarm.enabled = item.enabled,
                Feature::AutoPoke => self.config.features.auto_poke.enabled = item.enabled,
            }
        }
        self.provider_detail.apply(&mut self.config);
        self.swarm_detail.apply_to_config(&mut self.config);
    }
}

// ── Drawing ─────────────────────────────────────────────────────────────────

fn draw_main(f: &mut Frame, app: &mut App) {
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
            let checkbox = if item.feature == Feature::Provider {
                Span::styled(" ⚙ ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            } else if item.enabled {
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
            // Show hint for features with detail pages
            let hint = if matches!(item.feature, Feature::Swarm | Feature::Provider) {
                Span::styled(" (Enter: config)", Style::default().fg(Color::DarkGray))
            } else {
                Span::raw("")
            };
            ListItem::new(Line::from(vec![
                Span::raw(" "),
                checkbox,
                label,
                desc,
                hint,
            ]))
        })
        .collect();

    let features_list = List::new(items)
        .block(
            Block::default()
                .title(" Features (y/n toggle) ")
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
        Span::styled("y/n", Style::default().fg(Color::Yellow)),
        Span::raw(" toggle  "),
        Span::styled("Enter", Style::default().fg(Color::Yellow)),
        Span::raw(" open detail  "),
        Span::styled("s", Style::default().fg(Color::Yellow)),
        Span::raw(" save  "),
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(" quit"),
    ]);
    let status_bar = Paragraph::new(status);
    f.render_widget(status_bar, chunks[4]);
}

fn draw_swarm_detail(f: &mut Frame, app: &mut App) {
    let input_height = if app.swarm_detail.mode != DetailMode::Normal {
        // Need room: 1 input line + up to 5 suggestions + 2 borders = 8
        Constraint::Length(9)
    } else {
        Constraint::Length(0)
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3),  // title
            Constraint::Min(10),   // content
            input_height,           // input (add/edit mode)
            Constraint::Length(1),  // status bar
        ])
        .split(f.area());

    // Title
    let title_text = match app.swarm_detail.mode {
        DetailMode::AddProfile => "Swarm — Add Agent Profile",
        DetailMode::EditProfile => "Swarm — Edit Agent Profile",
        DetailMode::Normal => "Swarm — Sub-agent Configuration",
    };
    let title = Paragraph::new(title_text)
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(title, chunks[0]);

    let detail = &app.swarm_detail;
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(""));

    // ── Settings section ──
    let text_fields: [(&str, &str, &str); 3] = [
        ("Model (sub-agent)", &detail.model, "inherit parent if empty"),
        ("Max agents", &detail.max_agents, "concurrent sub-agents"),
        ("Agent max turns", &detail.agent_max_turns, "LLM turns per agent"),
    ];
    for (i, (label, value, hint)) in text_fields.iter().enumerate() {
        let focused = detail.focus == i && detail.mode == DetailMode::Normal;
        let cursor = if focused && detail.mode == DetailMode::Normal { "▌" } else { "" };
        let ls = if focused { Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD) } else { Style::default().fg(Color::Gray) };
        let vs = if focused { Style::default().fg(Color::White) } else if value.is_empty() { Style::default().fg(Color::DarkGray) } else { Style::default().fg(Color::Green) };
        let dv = if value.is_empty() { "(not set)".to_string() } else { value.to_string() };
        lines.push(Line::from(vec![
            Span::raw("  "), Span::styled(format!("{:<20}", label), ls),
            Span::styled(dv, vs), Span::styled(cursor, Style::default().fg(Color::Cyan)),
        ]));
        lines.push(Line::from(vec![
            Span::raw("  "), Span::styled(format!("{:<20}", ""), Style::default()),
            Span::styled(*hint, Style::default().fg(Color::DarkGray)),
        ]));
        lines.push(Line::from(""));
    }

    // Cycling fields: spawn_mode, model_selection
    let cycling: [(usize, &str, &str, &[&str]); 2] = [
        (3, "Spawn mode", &detail.spawn_mode, SPAWN_MODES),
        (4, "Model selection", &detail.model_selection, MODEL_SELECTIONS),
    ];
    for (idx, label, value, _options) in &cycling {
        let focused = detail.focus == *idx && detail.mode == DetailMode::Normal;
        let ls = if focused { Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD) } else { Style::default().fg(Color::Gray) };
        let vs = if focused { Style::default().fg(Color::White).add_modifier(Modifier::BOLD) } else { Style::default().fg(Color::Green) };
        let desc = match *idx {
            3 => match detail.spawn_mode.as_str() {
                "terminal" => "new OS terminal with full REPL",
                "in-process" => "background tokio task",
                _ => "",
            },
            4 => match detail.model_selection.as_str() {
                "auto" => "agent decides model freely",
                "slots" => "use per-slot model assignments",
                "fixed" => "always use config default, no override",
                _ => "",
            },
            _ => "",
        };
        lines.push(Line::from(vec![
            Span::raw("  "), Span::styled(format!("{:<20}", label), ls),
            Span::styled(format!("< {} >", value), vs),
        ]));
        lines.push(Line::from(vec![
            Span::raw("  "), Span::styled(format!("{:<20}", ""), Style::default()),
            Span::styled(desc, Style::default().fg(Color::DarkGray)),
        ]));
        lines.push(Line::from(""));
    }

    // ── Agent slots section ──
    lines.push(Line::from(Span::styled(
        "  ── Agent Slots ──",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(""));

    if detail.profiles.is_empty() {
        lines.push(Line::from(Span::styled(
            "    (none — press 'a' to add a slot)",
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
        )));
    } else {
        for (i, p) in detail.profiles.iter().enumerate() {
            let focused = detail.focus == 5 + i && detail.mode == DetailMode::Normal;
            let marker = if focused { "▸ " } else { "  " };
            let label = format!("Agent {}", i + 1);
            let model_display = if p.model.is_empty() {
                "(default)".to_string()
            } else {
                p.model.clone()
            };
            let ls = if focused {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Yellow)
            };
            let ms = if focused {
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
            } else if p.model.is_empty() {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::Green)
            };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(marker, if focused { Style::default().fg(Color::Cyan) } else { Style::default() }),
                Span::styled(format!("{:<12}", label), ls),
                Span::styled(model_display, ms),
            ]));
        }
    }

    // Add button
    lines.push(Line::from(""));
    let add_focused = detail.is_on_add() && detail.mode == DetailMode::Normal;
    let add_style = if add_focused {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let add_marker = if add_focused { "▸ " } else { "  " };
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(add_marker, if add_focused { Style::default().fg(Color::Cyan) } else { Style::default() }),
        Span::styled("+ Add profile", add_style),
    ]));

    let block_title = match detail.mode {
        DetailMode::Normal => " Settings & Profiles (↑↓ navigate) ",
        _ => " ",
    };
    let fields_widget = Paragraph::new(lines)
        .block(Block::default().title(block_title).borders(Borders::ALL));
    f.render_widget(fields_widget, chunks[1]);

    // ── Input area (add/edit mode with suggestions) ──
    if detail.mode == DetailMode::AddProfile || detail.mode == DetailMode::EditProfile {
        let slot_label = match detail.mode {
            DetailMode::AddProfile => format!("Agent {} — Model", detail.profiles.len() + 1),
            DetailMode::EditProfile => format!("Agent {} — Model", detail.edit_index + 1),
            _ => String::new(),
        };

        let mut input_lines: Vec<Line> = Vec::new();
        // Input field
        input_lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{:<20}", slot_label), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled(detail.input_model.as_str(), Style::default().fg(Color::White)),
            Span::styled("▌", Style::default().fg(Color::Cyan)),
        ]));

        // Suggestions
        if !detail.suggestions.is_empty() {
            let max_show = 5.min(detail.suggestions.len());
            let start = if detail.suggestion_idx >= max_show {
                detail.suggestion_idx - max_show + 1
            } else {
                0
            };
            for (display_i, &model_i) in detail.suggestions[start..start + max_show].iter().enumerate() {
                let real_i = start + display_i;
                let highlighted = real_i == detail.suggestion_idx;
                let marker = if highlighted { "▸ " } else { "  " };
                let style = if highlighted {
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                input_lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(marker, if highlighted { Style::default().fg(Color::Cyan) } else { Style::default() }),
                    Span::styled(&detail.recent_models[model_i], style),
                ]));
            }
        } else if !detail.input_model.is_empty() {
            input_lines.push(Line::from(Span::styled(
                "    (no matching models)",
                Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
            )));
        }

        let input_widget = Paragraph::new(input_lines)
            .block(Block::default()
                .title(" ↑↓: select suggestion  Tab: accept  Enter: confirm  Esc: cancel ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)));
        f.render_widget(input_widget, chunks[2]);
    } else {
        f.render_widget(Paragraph::new(""), chunks[2]);
    }

    // ── Status bar ──
    let status = match detail.mode {
        DetailMode::AddProfile | DetailMode::EditProfile => Line::from(vec![
            Span::styled(" Tab", Style::default().fg(Color::Yellow)),
            Span::raw(" field  "),
            Span::styled("Enter", Style::default().fg(Color::Yellow)),
            Span::raw(" confirm  "),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::raw(" cancel"),
        ]),
        DetailMode::Normal => Line::from(vec![
            Span::styled(" ↑↓", Style::default().fg(Color::Yellow)),
            Span::raw(" navigate  "),
            Span::styled("Space", Style::default().fg(Color::Yellow)),
            Span::raw(" cycle  "),
            Span::styled("a", Style::default().fg(Color::Yellow)),
            Span::raw(" add slot  "),
            Span::styled("e", Style::default().fg(Color::Yellow)),
            Span::raw(" edit  "),
            Span::styled("d", Style::default().fg(Color::Red)),
            Span::raw(" delete  "),
            Span::styled("J/K", Style::default().fg(Color::Yellow)),
            Span::raw(" reorder  "),
            Span::styled("s", Style::default().fg(Color::Yellow)),
            Span::raw(" save  "),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::raw(" back"),
        ]),
    };
    f.render_widget(Paragraph::new(status), chunks[3]);
}

fn draw_provider_detail(f: &mut Frame, app: &mut App) {
    let pd = &app.provider_detail;
    let input_height = if pd.editing { Constraint::Length(9) } else { Constraint::Length(0) };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3),  // title
            Constraint::Min(8),    // fields
            input_height,          // suggestions (when editing)
            Constraint::Length(1),  // status bar
        ])
        .split(f.area());

    // Title
    let title = Paragraph::new("Provider settings")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(title, chunks[0]);

    // Fields
    let fields = [
        ("Type", pd.provider_type.as_str(), true),   // cycling
        ("Model", pd.model.as_str(), false),
        ("API Key", if pd.show_key { pd.api_key.as_str() } else { if pd.api_key.is_empty() { "" } else { "••••••••" } }, false),
        ("Base URL", pd.base_url.as_str(), false),
    ];

    let field_lines: Vec<Line> = fields.iter().enumerate().map(|(i, (label, value, is_cycle))| {
        let focused = pd.focus == i;
        let label_style = if focused {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let value_style = if focused {
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
        } else if value.is_empty() {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::Green)
        };

        let display_val = if value.is_empty() { "(not set)" } else { *value };
        let cycle_hint = if *is_cycle && focused { " ◄ ►" } else { "" };
        let edit_hint = if !is_cycle && focused && pd.editing { "▌" } else { "" };

        Line::from(vec![
            Span::styled(format!("  {:<12}", label), label_style),
            Span::styled(format!("{}{}", display_val, cycle_hint), value_style),
            Span::styled(edit_hint, Style::default().fg(Color::Cyan)),
        ])
    }).collect();

    let fields_block = Paragraph::new(field_lines)
        .block(Block::default().title(" Settings ").borders(Borders::ALL));
    f.render_widget(fields_block, chunks[1]);

    // Suggestions (when editing a text field)
    if pd.editing && !pd.suggestions.is_empty() {
        let suggestion_items: Vec<ListItem> = pd.suggestions.iter().enumerate().map(|(si, &mi)| {
            let style = if si == pd.suggestion_idx {
                Style::default().bg(Color::DarkGray).fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("  {}", pd.recent_models[mi]), style),
            ]))
        }).collect();

        let suggestions_list = List::new(suggestion_items)
            .block(Block::default().title(" Suggestions (↑↓ select, Tab accept) ").borders(Borders::ALL));
        f.render_widget(suggestions_list, chunks[2]);
    }

    // Status bar
    let status = if pd.editing {
        Line::from(vec![
            Span::styled("Enter", Style::default().fg(Color::Yellow)),
            Span::raw(" confirm  "),
            Span::styled("Tab", Style::default().fg(Color::Yellow)),
            Span::raw(" accept  "),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::raw(" cancel"),
        ])
    } else {
        Line::from(vec![
            Span::styled(" ↑↓", Style::default().fg(Color::Yellow)),
            Span::raw(" navigate  "),
            Span::styled("←→/Space", Style::default().fg(Color::Yellow)),
            Span::raw(" cycle  "),
            Span::styled("Enter", Style::default().fg(Color::Yellow)),
            Span::raw(" edit  "),
            Span::styled("k", Style::default().fg(Color::Yellow)),
            Span::raw(" show/hide key  "),
            Span::styled("s", Style::default().fg(Color::Yellow)),
            Span::raw(" save  "),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::raw(" back"),
        ])
    };
    f.render_widget(Paragraph::new(status), chunks[3]);
}

fn draw(f: &mut Frame, app: &mut App) {
    match app.page {
        Page::Main => draw_main(f, app),
        Page::SwarmDetail => draw_swarm_detail(f, app),
        Page::ProviderDetail => draw_provider_detail(f, app),
    }
}

// ── Public entry point ──────────────────────────────────────────────────────

/// Run the interactive config TUI.
///
/// Returns `Ok(true)` if the user saved changes, `Ok(false)` if they quit without saving.
pub fn run(config: Config, save_path: &Path, env_path: std::path::PathBuf) -> Result<bool> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(config, env_path);
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

            match (&app.page, key.code) {
                // ── Main page ──
                (Page::Main, KeyCode::Char('q') | KeyCode::Esc) => return Ok(()),
                (Page::Main, KeyCode::Char('s')) => {
                    app.saved = true;
                    return Ok(());
                }
                (Page::Main, KeyCode::Up | KeyCode::Char('k')) => app.move_up(),
                (Page::Main, KeyCode::Down | KeyCode::Char('j')) => app.move_down(),
                (Page::Main, KeyCode::Char('y') | KeyCode::Char('n')) => {
                    app.toggle_selected();
                }
                (Page::Main, KeyCode::Enter) => {
                    if let Some(i) = app.list_state.selected() {
                        if matches!(app.features[i].feature, Feature::Swarm | Feature::Provider) {
                            app.open_selected();
                        }
                    }
                }

                // ── Swarm detail page ──
                (Page::SwarmDetail, KeyCode::Esc) => {
                    if app.swarm_detail.mode != DetailMode::Normal {
                        app.swarm_detail.cancel_input();
                    } else {
                        app.page = Page::Main;
                    }
                }
                (Page::SwarmDetail, KeyCode::Char('s')) => {
                    if app.swarm_detail.mode == DetailMode::Normal {
                        app.saved = true;
                        return Ok(());
                    }
                }
                // ── Add/Edit mode ──
                (Page::SwarmDetail, KeyCode::Enter) if app.swarm_detail.mode != DetailMode::Normal => {
                    // If a suggestion is highlighted, accept it first, then confirm
                    if !app.swarm_detail.suggestions.is_empty() {
                        app.swarm_detail.accept_suggestion();
                    }
                    app.swarm_detail.confirm_input();
                }
                (Page::SwarmDetail, KeyCode::Tab) if app.swarm_detail.mode != DetailMode::Normal => {
                    app.swarm_detail.accept_suggestion();
                }
                (Page::SwarmDetail, KeyCode::Up) if app.swarm_detail.mode != DetailMode::Normal => {
                    app.swarm_detail.suggestion_up();
                }
                (Page::SwarmDetail, KeyCode::Down) if app.swarm_detail.mode != DetailMode::Normal => {
                    app.swarm_detail.suggestion_down();
                }
                (Page::SwarmDetail, KeyCode::Char(c)) if app.swarm_detail.mode != DetailMode::Normal => {
                    app.swarm_detail.handle_char(c);
                }
                (Page::SwarmDetail, KeyCode::Backspace) if app.swarm_detail.mode != DetailMode::Normal => {
                    app.swarm_detail.handle_backspace();
                }
                // ── Normal mode ──
                (Page::SwarmDetail, KeyCode::Up | KeyCode::Char('k')) if app.swarm_detail.mode == DetailMode::Normal => {
                    app.swarm_detail.move_up();
                }
                (Page::SwarmDetail, KeyCode::Down | KeyCode::Char('j')) if app.swarm_detail.mode == DetailMode::Normal => {
                    app.swarm_detail.move_down();
                }
                (Page::SwarmDetail, KeyCode::Char(' ') | KeyCode::Enter) if app.swarm_detail.mode == DetailMode::Normal => {
                    app.swarm_detail.cycle_focused();
                }
                (Page::SwarmDetail, KeyCode::Left | KeyCode::Right) if app.swarm_detail.mode == DetailMode::Normal => {
                    app.swarm_detail.cycle_focused();
                }
                (Page::SwarmDetail, KeyCode::Char('a')) if app.swarm_detail.mode == DetailMode::Normal => {
                    app.swarm_detail.start_add();
                }
                (Page::SwarmDetail, KeyCode::Char('e')) if app.swarm_detail.mode == DetailMode::Normal => {
                    app.swarm_detail.start_edit();
                }
                (Page::SwarmDetail, KeyCode::Char('d')) if app.swarm_detail.mode == DetailMode::Normal => {
                    app.swarm_detail.delete_profile();
                }
                (Page::SwarmDetail, KeyCode::Char('J')) if app.swarm_detail.mode == DetailMode::Normal => {
                    app.swarm_detail.move_profile_down();
                }
                (Page::SwarmDetail, KeyCode::Char('K')) if app.swarm_detail.mode == DetailMode::Normal => {
                    app.swarm_detail.move_profile_up();
                }
                (Page::SwarmDetail, KeyCode::Char(c)) if app.swarm_detail.mode == DetailMode::Normal => {
                    app.swarm_detail.handle_char(c);
                }
                (Page::SwarmDetail, KeyCode::Backspace) if app.swarm_detail.mode == DetailMode::Normal => {
                    app.swarm_detail.handle_backspace();
                }

                // ── Provider detail page ──
                (Page::ProviderDetail, KeyCode::Esc) => {
                    if app.provider_detail.editing {
                        app.provider_detail.editing = false;
                        app.provider_detail.suggestions.clear();
                    } else {
                        app.page = Page::Main;
                    }
                }
                (Page::ProviderDetail, KeyCode::Char('s')) if !app.provider_detail.editing => {
                    app.saved = true;
                    return Ok(());
                }
                // ── Edit mode ──
                (Page::ProviderDetail, KeyCode::Enter) if app.provider_detail.editing => {
                    if !app.provider_detail.suggestions.is_empty() {
                        let si = app.provider_detail.suggestion_idx;
                        if si < app.provider_detail.suggestions.len() {
                            let mi = app.provider_detail.suggestions[si];
                            let val = app.provider_detail.recent_models[mi].clone();
                            *app.provider_detail.field_value_mut(app.provider_detail.focus) = val;
                        }
                    }
                    app.provider_detail.editing = false;
                    app.provider_detail.suggestions.clear();
                }
                (Page::ProviderDetail, KeyCode::Tab) if app.provider_detail.editing => {
                    if !app.provider_detail.suggestions.is_empty() {
                        let si = app.provider_detail.suggestion_idx;
                        if si < app.provider_detail.suggestions.len() {
                            let mi = app.provider_detail.suggestions[si];
                            let val = app.provider_detail.recent_models[mi].clone();
                            *app.provider_detail.field_value_mut(app.provider_detail.focus) = val;
                        }
                        app.provider_detail.update_suggestions();
                    }
                }
                (Page::ProviderDetail, KeyCode::Up) if app.provider_detail.editing => {
                    if app.provider_detail.suggestion_idx > 0 {
                        app.provider_detail.suggestion_idx -= 1;
                    }
                }
                (Page::ProviderDetail, KeyCode::Down) if app.provider_detail.editing => {
                    if app.provider_detail.suggestion_idx + 1 < app.provider_detail.suggestions.len() {
                        app.provider_detail.suggestion_idx += 1;
                    }
                }
                (Page::ProviderDetail, KeyCode::Char(c)) if app.provider_detail.editing => {
                    app.provider_detail.field_value_mut(app.provider_detail.focus).push(c);
                    app.provider_detail.update_suggestions();
                }
                (Page::ProviderDetail, KeyCode::Backspace) if app.provider_detail.editing => {
                    app.provider_detail.field_value_mut(app.provider_detail.focus).pop();
                    app.provider_detail.update_suggestions();
                }
                // ── Normal mode ──
                (Page::ProviderDetail, KeyCode::Up | KeyCode::Char('k')) if !app.provider_detail.editing => {
                    app.provider_detail.focus = app.provider_detail.focus.saturating_sub(1);
                }
                (Page::ProviderDetail, KeyCode::Down | KeyCode::Char('j')) if !app.provider_detail.editing => {
                    if app.provider_detail.focus < 3 {
                        app.provider_detail.focus += 1;
                    }
                }
                (Page::ProviderDetail, KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right) if !app.provider_detail.editing => {
                    if app.provider_detail.focus == 0 {
                        app.provider_detail.cycle_type();
                    }
                }
                (Page::ProviderDetail, KeyCode::Enter) if !app.provider_detail.editing => {
                    if app.provider_detail.focus == 0 {
                        app.provider_detail.cycle_type();
                    } else {
                        app.provider_detail.editing = true;
                        app.provider_detail.update_suggestions();
                    }
                }
                (Page::ProviderDetail, KeyCode::Char('k')) if !app.provider_detail.editing => {
                    app.provider_detail.show_key = !app.provider_detail.show_key;
                }
                _ => {}
            }
        }
    }
}
