//! Provider factory and environment loading.
//!
//! Resolves provider type and API key from config, env vars, and CLI args.

use anyhow::Result;
use flint_provider::anthropic::AnthropicProvider;
use flint_provider::openai::OpenAIProvider;
use flint_provider::Provider;

// ── Home directory ─────────────────────────────────────────────────────────

pub fn home_dir() -> Option<std::path::PathBuf> {
    dirs::home_dir()
}

// ── .env loading ───────────────────────────────────────────────────────────

/// Load a .env file, overriding existing environment variables.
/// Unlike `dotenvy::from_path`, this always overwrites.
pub fn load_env_override(path: &std::path::Path) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, val)) = line.split_once('=') {
            let key = key.trim();
            let val = val.trim().trim_matches('"').trim_matches('\'');
            std::env::set_var(key, val);
        }
    }
}

/// Resolve where to save .env: project-level if in a git repo, else global.
pub fn resolve_env_path(working_dir: &std::path::Path) -> std::path::PathBuf {
    if working_dir.join(".flint.toml").exists() || working_dir.join(".git").exists() {
        working_dir.join(".env")
    } else {
        home_dir()
            .map(|h| h.join(".flint").join(".env"))
            .unwrap_or_else(|| working_dir.join(".env"))
    }
}

// ── Provider factory ───────────────────────────────────────────────────────

/// Strip trailing endpoint path from a base URL, keeping at most the `/v1` prefix.
/// e.g. "https://example.com/v1/chat/completions" -> "https://example.com/v1"
///      "https://example.com/v1/images/generations" -> "https://example.com/v1"
///      "https://example.com" -> "https://example.com"
fn strip_endpoint(url: &str) -> &str {
    // First try known full endpoint suffixes
    let known = ["/v1/messages", "/v1/chat/completions", "/v1/completions"];
    for suffix in &known {
        if let Some(stripped) = url.strip_suffix(suffix) {
            return stripped;
        }
    }
    // Then strip any path after /v1 (e.g. /v1/images/generations -> /v1)
    if let Some(pos) = url.find("/v1/") {
        return &url[..pos + 3]; // keep "/v1"
    }
    url
}

/// Find an API key from env vars, trying provider-specific then generic fallbacks.
/// Returns `(key, from_auth_token)` where `from_auth_token` is true when the key
/// came from an `*_AUTH_TOKEN` env var (indicating Bearer auth should be used).
fn find_api_key(provider: &str) -> Option<(String, bool)> {
    let keys = match provider {
        "anthropic" => vec![
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "OPENAI_API_KEY",
            "OPENAI_AUTH_TOKEN",
        ],
        "openai" => vec![
            "OPENAI_API_KEY",
            "OPENAI_AUTH_TOKEN",
            "AGNES_API_KEY",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
        ],
        _ => vec![
            "OPENAI_API_KEY",
            "OPENAI_AUTH_TOKEN",
            "AGNES_API_KEY",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
        ],
    };
    for k in keys {
        if let Ok(val) = std::env::var(k) {
            if !val.is_empty() {
                let from_auth_token = k.ends_with("_AUTH_TOKEN");
                return Some((val, from_auth_token));
            }
        }
    }
    None
}

/// Find a base URL from env vars, trying provider-specific then generic fallbacks.
fn find_base_url(provider: &str) -> Option<String> {
    let vars = match provider {
        "anthropic" => vec!["ANTHROPIC_BASE_URL", "OPENAI_BASE_URL"],
        "openai" => vec!["OPENAI_BASE_URL", "ANTHROPIC_BASE_URL"],
        _ => vec!["OPENAI_BASE_URL", "ANTHROPIC_BASE_URL"],
    };
    for v in vars {
        if let Ok(val) = std::env::var(v) {
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

/// Find a model-specific base URL from config.
/// Checks if the model name starts with any key in model_base_urls.
fn find_model_base_url(model: &str, model_base_urls: &std::collections::HashMap<String, String>) -> Option<String> {
    // First try exact match
    if let Some(url) = model_base_urls.get(model) {
        if !url.is_empty() {
            return Some(url.clone());
        }
    }
    // Then try prefix match (e.g., "mimo" matches "mimo-v2.5-pro")
    for (prefix, url) in model_base_urls {
        if !prefix.is_empty() && !url.is_empty() && model.starts_with(prefix.as_str()) {
            return Some(url.clone());
        }
    }
    None
}

/// Find a model-specific API key from config.
/// Checks if the model name starts with any key in model_api_keys.
fn find_model_api_key(model: &str, model_api_keys: &std::collections::HashMap<String, String>) -> Option<String> {
    // First try exact match
    if let Some(key) = model_api_keys.get(model) {
        if !key.is_empty() {
            return Some(key.clone());
        }
    }
    // Then try prefix match (e.g., "agnes" matches "agnes-2.0-flash")
    for (prefix, key) in model_api_keys {
        if !prefix.is_empty() && !key.is_empty() && model.starts_with(prefix.as_str()) {
            return Some(key.clone());
        }
    }
    None
}

/// Build a provider from type and model, resolving API key from env vars.
/// API key is optional — when not found, requests are sent without authentication
/// (useful for self-hosted / local models that don't require a key).
pub fn build_provider(provider_type: &str, model: &str) -> Result<Box<dyn Provider>> {
    build_provider_with_config(provider_type, model, &std::collections::HashMap::new(), &std::collections::HashMap::new())
}

/// Build a provider with model-specific base URL and API key overrides.
/// model_base_urls maps model name prefixes to base URLs.
/// model_api_keys maps model name prefixes to API keys.
pub fn build_provider_with_config(
    provider_type: &str,
    model: &str,
    model_base_urls: &std::collections::HashMap<String, String>,
    model_api_keys: &std::collections::HashMap<String, String>,
) -> Result<Box<dyn Provider>> {
    // Check for model-specific API key first, then fall back to env vars
    let (key, from_auth_token) = find_model_api_key(model, model_api_keys)
        .map(|k| (k, false))
        .or_else(|| find_api_key(provider_type))
        .unwrap_or_default();

    // Check for model-specific base URL first, then fall back to env vars
    let base_url = find_model_base_url(model, model_base_urls)
        .or_else(|| find_base_url(provider_type));

    match provider_type {
        "anthropic" => {
            let mut p = AnthropicProvider::new(key, model).bearer_auth(from_auth_token);
            if let Some(base) = base_url {
                p = p.base_url(strip_endpoint(&base));
            }
            Ok(Box::new(p))
        }
        "openai" => {
            let mut p = OpenAIProvider::new(key, model);
            if let Some(base) = base_url {
                p = p.base_url(strip_endpoint(&base));
            }
            Ok(Box::new(p))
        }
        other => Err(anyhow::anyhow!("unknown provider: {}", other)),
    }
}

// ── .env update ────────────────────────────────────────────────────────────

/// Update or add key=value pairs in a .env file.
/// Preserves comments and ordering; appends new keys at the end.
pub fn update_env_file(path: &std::path::Path, updates: &[(String, String)]) {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<String> = content.lines().map(String::from).collect();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Update existing keys
    for line in &mut lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, _)) = trimmed.split_once('=') {
            let key = key.trim().to_string();
            if let Some((_, val)) = updates.iter().find(|(k, _)| *k == key) {
                *line = format!("{}={}", key, val);
                seen.insert(key);
            }
        }
    }

    // Append new keys
    for (key, val) in updates {
        if !seen.contains(key) {
            lines.push(format!("{}={}", key, val));
        }
    }

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, lines.join("\n"));
}
