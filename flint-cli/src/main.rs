//! flint — Easy-to-hack agent harness in Rust.

mod config_ui;
mod input;
mod model_ui;
mod provider;
pub mod session_import;
mod setup_ui;
mod tools;

use anyhow::Result;
use clap::{Parser, Subcommand};
use flint_agent::{run_turn, Session, ToolContext, ToolRegistry};
use flint_config::Feature;
use flint_provider::Provider;
use std::io::Write;

// ── CLI args ──────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "flint", about = "Easy-to-hack agent harness in Rust")]
struct Cli {
    /// Initial prompt (runs once and exits, or starts REPL if omitted)
    prompt: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,

    /// Working directory
    #[arg(global = true, long, default_value = ".")]
    dir: String,
}

#[derive(Subcommand)]
enum Commands {
    /// Open interactive settings panel
    Config,
    /// Configure provider (interactive if no args, or automated with args)
    Setup(SetupArgs),
    /// Run the agent (default when no subcommand is given)
    Agent(AgentArgs),
}

#[derive(Parser)]
pub struct SetupArgs {
    /// Provider name: openai | anthropic
    #[arg(long)]
    provider: Option<String>,
    /// API key
    #[arg(long)]
    key: Option<String>,
    /// Base URL (for OpenAI-compatible endpoints)
    #[arg(long)]
    base_url: Option<String>,
    /// Model name
    #[arg(long)]
    model: Option<String>,
}

#[derive(Parser)]
pub struct AgentArgs {
    /// Initial prompt (skips REPL, runs once and exits)
    prompt: Option<String>,

    /// LLM provider: anthropic | openai
    #[arg(long)]
    provider: Option<String>,

    /// Model name
    #[arg(long)]
    model: Option<String>,

    /// System prompt
    #[arg(long)]
    system: Option<String>,
}

// ── Default system prompt ─────────────────────────────────────────────────

const DEFAULT_SYSTEM: &str = "\
You are flint — a fast, focused coding agent.

## Principles
- Be concise. No filler.
- Do the task. Don't explain what you're about to do unless asked.
- One good answer beats five mediocre ones.
- If unsure, ask. Don't guess.

## Tools
You have: read, write, bash, grep, glob. Use them. Don't simulate.

## Working Directory
All file operations are relative to the working directory provided. Stay within it.

## Skills
Skills are reusable prompt modules (.md files). When asked to install or create a skill:
- Use the project's skill directory (the working directory's skill path).
- If unsure where skills live, check the project config or ask.
- Never write skills to unrelated directories.

## Style
- Short responses for simple questions.
- Code over prose.
- No apologies, no disclaimers, no \"I'll help you with that\".";

// ── Slash commands ────────────────────────────────────────────────────────

enum SlashAction {
    Config,
    Setup,
    Model(Option<String>),
    Skill(Option<String>),
    Clear,
    Help,
    Status,
    Skills,
    Resume(Option<String>),
    Quit,
    Unknown(String),
}

fn parse_slash(input: &str) -> Option<SlashAction> {
    let input = input.trim();
    if !input.starts_with('/') {
        return None;
    }
    let cmd = input[1..].split_whitespace().next()?;
    Some(match cmd {
        "config" => SlashAction::Config,
        "setup" => SlashAction::Setup,
        "model" => {
            let arg = input[1..].split_whitespace().nth(1);
            SlashAction::Model(arg.map(|s| s.to_string()))
        }
        "skill" => {
            let arg = input[1..].split_whitespace().nth(1);
            SlashAction::Skill(arg.map(|s| s.to_string()))
        }
        "clear" => SlashAction::Clear,
        "help" | "h" | "?" => SlashAction::Help,
        "status" => SlashAction::Status,
        "skills" => SlashAction::Skills,
        "resume" => {
            let arg = input[1..].split_whitespace().nth(1);
            SlashAction::Resume(arg.map(|s| s.to_string()))
        }
        "quit" | "exit" | "q" => SlashAction::Quit,
        other => SlashAction::Unknown(other.to_string()),
    })
}

fn print_help() {
    println!(
        "\
Built-in commands:
  /config       Open interactive settings panel
  /setup        Configure provider credentials
  /model        Open model selection (interactive)
  /model <name> Switch to a specific model
  /skills       List available skills
  /skill <name> Load and show a specific skill
  /resume       List saved sessions (flint + Claude Code)
  /resume <id>  Restore a saved session
  /clear        Clear conversation history
  /status       Show current config status
  /help         Show this help message
  /quit         Exit the REPL

Keyboard shortcuts:
  Ctrl+A/E      Move to beginning/end of line
  Ctrl+U/K      Delete to beginning/end of line
  Ctrl+W        Delete previous word
  Ctrl+L        Clear screen
  Ctrl+Z        Undo
  Ctrl+J        Insert newline (multiline input)
  Up/Down       Navigate command history
  Left/Right    Move cursor
  Tab           Autocomplete slash commands

Anything else is sent to the LLM as a message.

Skills are auto-injected when your message matches a skill name or description.
You can also explicitly request a skill with [use: <skill-name>] in your message."
    );
}

fn print_skills(config: &flint_config::Config, working_dir: &std::path::Path) {
    let metas = config.load_skill_metas(Some(working_dir));
    if metas.is_empty() {
        println!("No skills found. Add .md files to:");
        for dir in config.skill_dirs(Some(working_dir)) {
            println!("  {}", dir.display());
        }
        println!();
        return;
    }

    let active_names: Vec<&str> = config
        .features
        .skills
        .active
        .iter()
        .map(|s| s.as_str())
        .collect();
    let filter_active = !active_names.is_empty();

    println!(
        "Skills{}:\n",
        if filter_active {
            " (filtered)"
        } else {
            " (all active)"
        }
    );
    for meta in &metas {
        let status = if !filter_active || active_names.contains(&meta.name.as_str()) {
            "\u{2713}"
        } else {
            "\u{2717}"
        };
        let desc = if meta.description.is_empty() {
            String::new()
        } else {
            format!(" \u{2014} {}", meta.description)
        };
        println!("  {} {}{}", status, meta.name, desc);
    }
    println!();
}

fn print_status(config: &flint_config::Config, working_dir: &std::path::Path) {
    let features = &config.features;
    let skill_count = if features.skills.enabled {
        config.load_all_skills(Some(working_dir)).len()
    } else {
        0
    };
    let skill_info = if features.skills.enabled {
        format!("{} loaded", skill_count)
    } else {
        "disabled".to_string()
    };

    println!(
        "\
Provider : {} / {}
Session  : {} (persistence: {})
Skills   : {}

Features:
  {} Skills       {} Memory
  {} Compaction   {} Permissions

Skill directories:",
        config.provider.r#type,
        config.provider.model,
        config.session.path.display(),
        if config.session.persistence { "on" } else { "off" },
        skill_info,
        if features.skills.enabled { "\u{2713}" } else { "\u{2717}" },
        if features.memory.enabled { "\u{2713}" } else { "\u{2717}" },
        if features.compaction.enabled {
            "\u{2713}"
        } else {
            "\u{2717}"
        },
        if features.permissions.enabled {
            "\u{2713}"
        } else {
            "\u{2717}"
        },
    );
    for dir in config.skill_dirs(Some(working_dir)) {
        let exists = dir.exists();
        let marker = if exists { "\u{2713}" } else { "\u{2717}" };
        println!("  {} {}", marker, dir.display());
    }
}

fn print_resume_sessions(config: &flint_config::Config, working_dir: &std::path::Path) {
    let session_dir = &config.session.path;
    let mut all_sessions: Vec<(String, flint_agent::SessionMeta, bool)> = Vec::new();

    // Load flint sessions
    match flint_agent::Session::list_sessions(session_dir) {
        Ok(sessions) => {
            for meta in sessions {
                all_sessions.push((meta.id.clone(), meta, false));
            }
        }
        Err(_) => {}
    }

    // Load Claude Code sessions
    match session_import::list_claude_sessions(working_dir) {
        Ok(sessions) => {
            for (_, meta) in sessions {
                all_sessions.push((meta.id.clone(), meta, true));
            }
        }
        Err(_) => {}
    }

    if all_sessions.is_empty() {
        println!("No saved sessions found.");
        println!();
        return;
    }

    // Sort by updated_at descending
    all_sessions.sort_by(|a, b| b.1.updated_at.cmp(&a.1.updated_at));

    println!("Saved sessions:\n");
    for (i, (_, meta, is_claude)) in all_sessions.iter().enumerate() {
        let updated = meta.updated_at.split('T').next().unwrap_or(&meta.updated_at);
        let source = if *is_claude { "[Claude Code]" } else { "[Flint]" };
        println!(
            "  {}. {} {} {} ({}) [{} msgs]",
            i + 1,
            &meta.id[..8.min(meta.id.len())],
            source,
            meta.title,
            updated,
            meta.message_count
        );
    }
    println!("\nUse /resume <id> to restore a session");
    println!();
}

fn load_session(path: &std::path::Path) -> Result<(flint_agent::Session, flint_agent::SessionMeta)> {
    flint_agent::Session::load(path)
}

// ── System prompt with skills (progressive disclosure) ────────────────────

fn build_system_prompt(
    base: &str,
    config: &flint_config::Config,
    working_dir: &std::path::Path,
) -> String {
    if !config.features.is_enabled(Feature::Skills) {
        return base.to_string();
    }

    // Only load metadata (cheap), not full prompt content
    let metas = config.load_skill_metas(Some(working_dir));
    if metas.is_empty() {
        return base.to_string();
    }

    let mut prompt = base.to_string();
    prompt.push_str("\n\n## Available Skills\n\n");
    prompt.push_str("The following skills are available. To use a skill, say ");
    prompt.push_str("`[use: <skill-name>]` in your response.\n\n");

    for meta in &metas {
        prompt.push_str(&format!(
            "- **{}**: {}\n",
            meta.name,
            if meta.description.is_empty() {
                "(no description)"
            } else {
                &meta.description
            }
        ));
    }

    prompt.push_str("\nThe system will inject the full skill content automatically ");
    prompt.push_str("when you reference it. Use skills when they match the user's intent.\n");

    prompt
}

/// Find a skill matching the user's input.
/// Checks skill names and description keywords.
fn match_skill(
    input: &str,
    config: &flint_config::Config,
    working_dir: &std::path::Path,
) -> Option<flint_config::Skill> {
    if !config.features.is_enabled(Feature::Skills) {
        return None;
    }

    let metas = config.load_skill_metas(Some(working_dir));
    let input_lower = input.to_lowercase();

    // Check for explicit /skill <name> command
    if let Some(name) = input.strip_prefix("/skill ") {
        let name = name.trim();
        return config.load_skill_by_name(name, Some(working_dir));
    }

    // Check for [use: <name>] marker in input
    if let Some(start) = input.find("[use:") {
        let rest = &input[start + 5..];
        if let Some(end) = rest.find(']') {
            let name = rest[..end].trim();
            return config.load_skill_by_name(name, Some(working_dir));
        }
    }

    // Match by skill name or description keywords
    for meta in &metas {
        let name_lower = meta.name.to_lowercase();
        let desc_lower = meta.description.to_lowercase();

        // Exact name match
        if input_lower.contains(&name_lower) {
            return config.load_skill_by_name(&meta.name, Some(working_dir));
        }

        // Description keyword match (only if description is non-empty)
        if !meta.description.is_empty() {
            let keywords: Vec<&str> = desc_lower
                .split_whitespace()
                .filter(|w| w.len() > 3) // Skip short words
                .collect();
            let matched = keywords.iter().any(|kw| input_lower.contains(kw));
            if matched {
                return config.load_skill_by_name(&meta.name, Some(working_dir));
            }
        }
    }

    None
}

// ── Config-aware tracing init ─────────────────────────────────────────────

fn init_tracing(level: &str) {
    use tracing::Level;
    let level = match level {
        "trace" => Level::TRACE,
        "debug" => Level::DEBUG,
        "info" => Level::INFO,
        "warn" => Level::WARN,
        "error" => Level::ERROR,
        _ => Level::WARN,
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive(level.into()),
        )
        .init();
}

// ── Subcommand handlers ───────────────────────────────────────────────────

fn cmd_config(working_dir: &std::path::Path) -> Result<()> {
    let config = flint_config::load(Some(working_dir))?;
    let save_path = config.save_path(Some(working_dir));
    config_ui::run(config, &save_path)?;
    Ok(())
}

fn cmd_setup(args: SetupArgs, working_dir: &std::path::Path) -> Result<()> {
    let env_path = provider::resolve_env_path(working_dir);

    // If all args provided, do automated setup
    if let (Some(provider), Some(key)) = (&args.provider, &args.key) {
        return setup_auto(&env_path, provider, key, args.base_url.as_deref(), args.model.as_deref());
    }

    // Otherwise launch interactive TUI
    let configured = setup_ui::run(&env_path)?;
    if configured {
        print_post_setup_guidance(&env_path);
    }
    Ok(())
}

fn setup_auto(
    env_path: &std::path::Path,
    provider: &str,
    api_key: &str,
    base_url: Option<&str>,
    model: Option<&str>,
) -> Result<()> {
    let p = setup_ui::find_provider(provider).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown provider '{}'. Supported: openai, anthropic",
            provider
        )
    })?;

    let mut lines = vec![
        "# flint configuration — generated by `flint setup`".to_string(),
        format!("FLINT_PROVIDER={}", p.name),
        format!("{}={}", p.env_key, api_key),
    ];

    if let Some(url) = base_url {
        lines.push(format!("{}={}", p.env_base, url));
    }
    if let Some(m) = model {
        lines.push(format!("{}={}", p.env_model, m));
    }

    // Merge with existing .env
    let content = if env_path.exists() {
        let existing = std::fs::read_to_string(env_path)?;
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

    std::fs::write(env_path, &content)?;

    println!("\u{2713} Provider configured:");
    println!("  Provider : {}", p.label);
    println!(
        "  API Key  : {}...{}",
        &api_key[..4.min(api_key.len())],
        &api_key[api_key.len().saturating_sub(4)..]
    );
    if let Some(url) = base_url {
        println!("  Base URL : {}", url);
    }
    if let Some(m) = model {
        println!("  Model    : {}", m);
    }
    println!("  Saved to : {}", env_path.display());
    print_post_setup_guidance(env_path);
    Ok(())
}

fn print_post_setup_guidance(env_path: &std::path::Path) {
    println!();
    println!("Next steps:");
    println!("  \u{2022} Run `flint` to start the interactive REPL");
    println!("  \u{2022} Run `flint \"your prompt\"` for a one-shot query");
    println!("  \u{2022} Run `flint config` to adjust features and settings");
    println!(
        "  \u{2022} Edit {} to change credentials",
        env_path.display()
    );
    println!();
}

async fn cmd_agent(args: AgentArgs, working_dir: &std::path::Path) -> Result<()> {
    // Load .env in priority order: global ~/.flint/.env -> project .env -> cwd .env
    let global_env = provider::home_dir().map(|h| h.join(".flint").join(".env"));
    let project_env = working_dir.join(".env");
    let cwd_env = std::env::current_dir().ok().map(|d| d.join(".env"));

    if let Some(ref path) = global_env {
        if path.exists() {
            provider::load_env_override(path);
        }
    }
    if project_env.exists() {
        provider::load_env_override(&project_env);
    }
    if let Some(ref path) = cwd_env {
        if path.exists() && path.as_path() != project_env.as_path() {
            provider::load_env_override(path);
        }
    }

    let env_path = provider::resolve_env_path(working_dir);
    let mut config = flint_config::load(Some(working_dir))?;

    // Resolve provider type: CLI arg > env var > config default
    let env_provider = std::env::var("FLINT_PROVIDER").ok();
    let env_model = std::env::var("FLINT_MODEL").ok();

    let provider_type = args
        .provider
        .clone()
        .or_else(|| env_provider.clone())
        .unwrap_or_else(|| config.provider.r#type.clone());
    let model = args
        .model
        .clone()
        .or_else(|| env_model.clone())
        .unwrap_or_else(|| config.provider.model.clone());

    // Try to build provider; on failure, launch setup wizard
    let mut prov: Box<dyn Provider> = match provider::build_provider(&provider_type, &model) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{}\n", e);
            eprintln!("Launching setup wizard...\n");
            let configured = setup_ui::run(&env_path)?;
            if !configured {
                eprintln!("Setup cancelled. Run `flint setup` to configure a provider.");
                eprintln!("  Or set credentials in: {}", env_path.display());
                return Ok(());
            }
            provider::load_env_override(&env_path);
            let new_type = std::env::var("FLINT_PROVIDER").unwrap_or_else(|_| provider_type.clone());
            let new_model = std::env::var("FLINT_MODEL").unwrap_or_else(|_| model.clone());
            provider::build_provider(&new_type, &new_model)?
        }
    };

    config.provider.r#type = provider_type.clone();
    config.provider.model = model.clone();

    let base_system: String = args
        .system
        .clone()
        .or_else(|| config.agent.system_prompt.clone())
        .unwrap_or_else(|| DEFAULT_SYSTEM.to_string());

    let system = build_system_prompt(&base_system, &config, working_dir);

    let mut registry = ToolRegistry::new();
    tools::register_builtins(&mut registry);

    let ctx = ToolContext {
        working_dir: working_dir.to_path_buf(),
    };

    if let Some(prompt) = &args.prompt {
        // One-shot mode
        let mut session = Session::new();
        session.add_user(prompt);
        if let Err(e) = run_turn(prov.as_ref(), &mut session, &registry, &system, &ctx).await {
            eprintln!("\n\u{26a0} Error: {}", e);
            eprintln!("  Run 'flint setup' to reconfigure provider.\n");
            std::process::exit(1);
        }
    } else {
        // REPL mode
        println!(
            "flint v{} \u{2014} {} / {}",
            env!("CARGO_PKG_VERSION"),
            config.provider.r#type,
            config.provider.model
        );
        println!("type /help for commands");

        if config.features.is_enabled(Feature::Skills) {
            let metas = config.load_skill_metas(Some(working_dir));
            if metas.is_empty() {
                println!("Skills: none loaded (add .md files to skill directories)");
            } else {
                println!("Skills: {} available \u{2014} /skills to list", metas.len());
            }
        }

        println!();
        let mut session = Session::new();
        let mut current_session_meta: Option<flint_agent::SessionMeta> = None;
        let mut turn_count: u32 = 0;

        loop {
            print!("\u{276f} ");
            std::io::stdout().flush()?;

            let input = match input::read_line()? {
                input::InputResult::Line(line) => line,
                input::InputResult::Exit => {
                    println!("Bye.");
                    break;
                }
            };
            let input = input.trim().to_string();

            if input.is_empty() {
                continue;
            }

            // Handle slash commands
            if let Some(action) = parse_slash(&input) {
                match action {
                    SlashAction::Quit => break,
                    SlashAction::Help => print_help(),
                    SlashAction::Clear => {
                        session = Session::new();
                        current_session_meta = None;
                        println!("Session cleared.\n");
                    }
                    SlashAction::Resume(arg) => {
                        match arg {
                            Some(id) => {
                                // Find session by ID prefix in flint sessions
                                let session_dir = &config.session.path;
                                let flint_sessions = flint_agent::Session::list_sessions(session_dir)
                                    .unwrap_or_default();
                                let found_flint = flint_sessions.iter().find(|s| s.id.starts_with(&id));

                                // Find session by ID prefix in Claude Code sessions
                                let claude_sessions = session_import::list_claude_sessions(&working_dir)
                                    .unwrap_or_default();
                                let found_claude = claude_sessions.iter().find(|(_, m)| m.id.starts_with(&id));

                                if let Some(meta) = found_flint {
                                    let path = session_dir.join(format!("{}.json", meta.id));
                                    match load_session(&path) {
                                        Ok((loaded_session, loaded_meta)) => {
                                            session = loaded_session;
                                            current_session_meta = Some(loaded_meta.clone());
                                            println!("Resumed session: {} ({})", loaded_meta.title, &loaded_meta.id[..8]);
                                            println!("  Provider: {} / {}", loaded_meta.provider, loaded_meta.model);
                                            println!("  Messages: {}\n", loaded_meta.message_count);
                                        }
                                        Err(e) => {
                                            println!("Error loading session: {}\n", e);
                                        }
                                    }
                                } else if let Some((path, _meta)) = found_claude {
                                    match session_import::import_session(path) {
                                        Ok((loaded_session, loaded_meta)) => {
                                            session = loaded_session;
                                            current_session_meta = Some(loaded_meta.clone());
                                            println!("Resumed Claude Code session: {} ({})", loaded_meta.title, &loaded_meta.id[..8]);
                                            println!("  Provider: {} / {}", loaded_meta.provider, loaded_meta.model);
                                            println!("  Messages: {}\n", loaded_meta.message_count);
                                        }
                                        Err(e) => {
                                            println!("Error loading Claude Code session: {}\n", e);
                                        }
                                    }
                                } else {
                                    println!("Session not found: {}\n", id);
                                }
                            }
                            None => {
                                print_resume_sessions(&config, &working_dir);
                            }
                        }
                    }
                    SlashAction::Config => {
                        cmd_config(&working_dir)?;
                        config = flint_config::load(Some(&working_dir))?;
                        println!();
                    }
                    SlashAction::Setup => {
                        let env_path = working_dir.join(".env");
                        setup_ui::run(&env_path)?;
                        provider::load_env_override(&env_path);
                        let p_type = std::env::var("FLINT_PROVIDER")
                            .unwrap_or_else(|_| config.provider.r#type.clone());
                        let p_model = std::env::var("FLINT_MODEL")
                            .unwrap_or_else(|_| config.provider.model.clone());
                        match provider::build_provider(&p_type, &p_model) {
                            Ok(p) => {
                                prov = p;
                                config.provider.r#type = p_type;
                                config.provider.model = p_model;
                                println!("Provider reloaded.\n");
                            }
                            Err(e) => {
                                println!("Setup incomplete: {}\n", e);
                            }
                        }
                    }
                    SlashAction::Model(name) => match name {
                        Some(m) => {
                            match provider::build_provider(&config.provider.r#type, &m) {
                                Ok(p) => {
                                    prov = p;
                                    config.provider.model = m.clone();
                                    println!("Switched to model: {}\n", m);
                                }
                                Err(e) => {
                                    println!("Failed to switch model: {}\n", e);
                                }
                            }
                        }
                        None => {
                            match model_ui::run(&config.provider.r#type, &config.provider.model) {
                                Ok(Some((m, is_custom))) => {
                                    if is_custom {
                                        let env_path = working_dir.join(".env");
                                        println!(
                                            "Custom model: {} \u{2014} opening provider setup...\n",
                                            m
                                        );
                                        match setup_ui::run(&env_path) {
                                            Ok(true) => {
                                                provider::load_env_override(&env_path);
                                                let p_type = std::env::var("FLINT_PROVIDER")
                                                    .unwrap_or_else(|_| {
                                                        config.provider.r#type.clone()
                                                    });
                                                match provider::build_provider(&p_type, &m) {
                                                    Ok(p) => {
                                                        prov = p;
                                                        config.provider.r#type = p_type;
                                                        config.provider.model = m.clone();
                                                        println!("Switched to model: {}\n", m);
                                                    }
                                                    Err(e) => {
                                                        println!(
                                                            "Failed to switch model: {}\n",
                                                            e
                                                        );
                                                    }
                                                }
                                            }
                                            Ok(false) => {
                                                println!("Setup cancelled. Model not changed.\n");
                                            }
                                            Err(e) => {
                                                println!("Setup error: {}\n", e);
                                            }
                                        }
                                    } else {
                                        match provider::build_provider(
                                            &config.provider.r#type,
                                            &m,
                                        ) {
                                            Ok(p) => {
                                                prov = p;
                                                config.provider.model = m.clone();
                                                println!("Switched to model: {}\n", m);
                                            }
                                            Err(e) => {
                                                println!("Failed to switch model: {}\n", e);
                                            }
                                        }
                                    }
                                }
                                Ok(None) => {
                                    println!("Cancelled.\n");
                                }
                                Err(e) => {
                                    println!("Error: {}\n", e);
                                }
                            }
                        }
                    },
                    SlashAction::Status => {
                        print_status(&config, &working_dir);
                        println!();
                    }
                    SlashAction::Skills => {
                        print_skills(&config, &working_dir);
                    }
                    SlashAction::Skill(name) => {
                        match name {
                            Some(n) => {
                                match config.load_skill_by_name(&n, Some(working_dir)) {
                                    Some(skill) => {
                                        println!("\n\u{2713} Skill: {}", skill.name);
                                        if !skill.description.is_empty() {
                                            println!("  Description: {}", skill.description);
                                        }
                                        println!("  Source: {}", skill.source.display());
                                        println!("\n---\n{}\n---\n", skill.prompt);
                                    }
                                    None => {
                                        println!("Skill '{}' not found. Use /skills to list available skills.\n", n);
                                    }
                                }
                            }
                            None => {
                                print_skills(&config, &working_dir);
                            }
                        }
                    }
                    SlashAction::Unknown(cmd) => {
                        println!(
                            "Unknown command: /{}\nType /help for available commands.\n",
                            cmd
                        );
                    }
                }
                continue;
            }

            // Normal message -> send to LLM
            turn_count += 1;
            eprintln!("\x1b[34m{}>\x1b[0m {}", turn_count, input);

            // Progressive disclosure: dynamically inject matched skill
            let effective_system = if let Some(skill) = match_skill(&input, &config, working_dir) {
                eprintln!(
                    "\x1b[90m  skill: {}\x1b[0m",
                    skill.name
                );
                format!("{}\n\n{}", system, skill.render())
            } else {
                system.clone()
            };

            session.add_user(&input);
            if let Err(e) =
                run_turn(prov.as_ref(), &mut session, &registry, &effective_system, &ctx).await
            {
                eprintln!("\n\x1b[31m\u{26a0} Error:\x1b[0m {}", e);
                eprintln!("  Type /setup to reconfigure provider, or /model to switch model.\n");
            }

            // Auto-save session after each turn
            if config.session.persistence && !session.is_empty() {
                let session_dir = &config.session.path;
                if !session_dir.exists() {
                    let _ = std::fs::create_dir_all(session_dir);
                }

                match &current_session_meta {
                    Some(meta) => {
                        // Update existing session
                        let path = session_dir.join(format!("{}.json", meta.id));
                        if let Err(e) = session.update_save(&path, meta) {
                            eprintln!("Warning: Failed to save session: {}", e);
                        }
                    }
                    None => {
                        // Save new session
                        let path = session_dir.join(format!("{}.json", uuid::Uuid::new_v4()));
                        if let Err(e) = session.save(&path, &config.provider.r#type, &config.provider.model) {
                            eprintln!("Warning: Failed to save session: {}", e);
                        } else {
                            // Load the meta for future updates
                            if let Ok((_, meta)) = flint_agent::Session::load(&path) {
                                current_session_meta = Some(meta);
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

// ── Main ──────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let working_dir = std::fs::canonicalize(&cli.dir)?;

    init_tracing("warn");

    match cli.command {
        Some(Commands::Config) => cmd_config(&working_dir),
        Some(Commands::Setup(args)) => cmd_setup(args, &working_dir),
        Some(Commands::Agent(args)) => cmd_agent(args, &working_dir).await,
        None => {
            cmd_agent(
                AgentArgs {
                    prompt: cli.prompt,
                    provider: None,
                    model: None,
                    system: None,
                },
                &working_dir,
            )
            .await
        }
    }
}
