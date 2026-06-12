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

/// Strip trailing endpoint path from a base URL.
/// e.g. "https://example.com/v1/chat/completions" -> "https://example.com"
fn strip_endpoint(url: &str) -> &str {
    ["/v1/messages", "/v1/chat/completions", "/v1/completions"]
        .iter()
        .find_map(|suffix| url.strip_suffix(suffix))
        .unwrap_or(url)
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
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
        ],
        _ => vec![
            "OPENAI_API_KEY",
            "OPENAI_AUTH_TOKEN",
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

/// Build a provider from type and model, resolving API key from env vars.
/// API key is optional — when not found, requests are sent without authentication
/// (useful for self-hosted / local models that don't require a key).
pub fn build_provider(provider_type: &str, model: &str) -> Result<Box<dyn Provider>> {
    let (key, from_auth_token) = find_api_key(provider_type).unwrap_or_default();

    match provider_type {
        "anthropic" => {
            let mut p = AnthropicProvider::new(key, model).bearer_auth(from_auth_token);
            if let Some(base) = find_base_url("anthropic") {
                p = p.base_url(strip_endpoint(&base));
            }
            Ok(Box::new(p))
        }
        "openai" => {
            let mut p = OpenAIProvider::new(key, model);
            if let Some(base) = find_base_url("openai") {
                p = p.base_url(strip_endpoint(&base));
            }
            Ok(Box::new(p))
        }
        other => Err(anyhow::anyhow!("unknown provider: {}", other)),
    }
}
