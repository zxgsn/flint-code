//! System prompt building and skill matching.

use flint_config::Feature;
use std::path::Path;

pub const DEFAULT_SYSTEM: &str = "\
You are flint -- a fast, focused coding agent.

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

/// Detect the current OS for system prompt injection.
fn detect_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "Linux"
    }
}

/// Build system prompt with environment info and skill metadata (progressive disclosure).
pub fn build_system_prompt(
    base: &str,
    config: &flint_config::Config,
    working_dir: &Path,
) -> String {
    let mut prompt = base.to_string();

    // Inject environment context so the LLM uses correct commands
    prompt.push_str(&format!(
        "\n\n## Environment\nOS: {}\nShell: {}\nWorking directory: {}",
        detect_os(),
        if cfg!(target_os = "windows") { "cmd.exe (use PowerShell for advanced tasks)" } else { "sh" },
        working_dir.display(),
    ));

    if !config.features.is_enabled(Feature::Skills) {
        return prompt;
    }

    let metas = config.load_skill_metas(Some(working_dir));
    if metas.is_empty() {
        return prompt;
    }

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

/// Find a skill matching the user's input by name, [use:] marker, or description keywords.
pub fn match_skill(
    input: &str,
    config: &flint_config::Config,
    working_dir: &Path,
) -> Option<flint_config::Skill> {
    if !config.features.is_enabled(Feature::Skills) {
        return None;
    }

    let metas = config.load_skill_metas(Some(working_dir));
    let input_lower = input.to_lowercase();

    // Explicit /skill <name>
    if let Some(name) = input.strip_prefix("/skill ") {
        let name = name.trim();
        return config.load_skill_by_name(name, Some(working_dir));
    }

    // [use: <name>] marker
    if let Some(start) = input.find("[use:") {
        let rest = &input[start + 5..];
        if let Some(end) = rest.find(']') {
            let name = rest[..end].trim();
            return config.load_skill_by_name(name, Some(working_dir));
        }
    }

    // Match by name or description keywords
    for meta in &metas {
        let name_lower = meta.name.to_lowercase();
        let desc_lower = meta.description.to_lowercase();

        if input_lower.contains(&name_lower) {
            return config.load_skill_by_name(&meta.name, Some(working_dir));
        }

        if !meta.description.is_empty() {
            let keywords: Vec<&str> = desc_lower
                .split_whitespace()
                .filter(|w| w.len() > 3)
                .collect();
            if keywords.iter().any(|kw| input_lower.contains(kw)) {
                return config.load_skill_by_name(&meta.name, Some(working_dir));
            }
        }
    }

    None
}
