//! Skill loading and management.
//!
//! A skill is a `.md` file with optional YAML frontmatter:
//!
//! ```markdown
//! ---
//! name: code-review
//! description: Review code for bugs
//! ---
//! You are a code reviewer. Check for bugs and edge cases.
//! ```
//!
//! Skills are loaded from directories specified in config.
//!
//! ## Two-tier loading
//!
//! - **Metadata** (`SkillMeta`): name + description only, for listing and matching.
//!   Loaded at startup, cheap to scan.
//! - **Full content** (`Skill`): includes prompt body, loaded on demand when
//!   a skill is matched or explicitly requested.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ── Skill metadata (lightweight) ──────────────────────────────────────────

/// Lightweight skill metadata for listing and matching.
/// Does not include the prompt body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub source: PathBuf,
}

// ── Full skill ────────────────────────────────────────────────────────────

/// A loaded skill with full prompt content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    /// Unique identifier (derived from filename if not in frontmatter).
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// The prompt content (everything after frontmatter).
    pub prompt: String,
    /// Source file path (for display).
    pub source: PathBuf,
}

impl Skill {
    /// Render this skill for injection into the system prompt.
    pub fn render(&self) -> String {
        format!("# Skill: {}\n\n{}", self.name, self.prompt)
    }
}

// ── Metadata loading (fast, no prompt body) ────────────────────────────────

/// Load skill metadata from a directory (only frontmatter, no prompt body).
pub fn load_metas(dir: &Path) -> Vec<SkillMeta> {
    let mut metas = Vec::new();

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return metas,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(meta) = load_meta_from_dir(&path) {
                metas.push(meta);
            }
        } else if path.extension().map_or(false, |e| e == "md") {
            match load_single_meta(&path) {
                Ok(meta) => metas.push(meta),
                Err(e) => {
                    tracing::warn!("skip skill {}: {}", path.display(), e);
                }
            }
        }
    }

    metas.sort_by(|a, b| a.name.cmp(&b.name));
    metas
}

/// Load skill metadata from multiple directories (later dirs override).
pub fn load_metas_from_dirs(dirs: &[PathBuf]) -> Vec<SkillMeta> {
    use std::collections::BTreeMap;

    let mut map: BTreeMap<String, SkillMeta> = BTreeMap::new();
    for dir in dirs {
        for meta in load_metas(dir) {
            map.insert(meta.name.clone(), meta);
        }
    }
    map.into_values().collect()
}

// ── Full content loading (on demand) ──────────────────────────────────────

/// Load a single skill by name from the given directories.
/// Returns the first match (later dirs have priority).
pub fn load_skill_by_name(name: &str, dirs: &[PathBuf]) -> Option<Skill> {
    for dir in dirs.iter().rev() {
        // Check direct .md file
        let path = dir.join(format!("{}.md", name));
        if path.is_file() {
            if let Ok(skill) = load_single_skill(&path) {
                if skill.name == name {
                    return Some(skill);
                }
            }
        }
        // Check subdirectory with SKILL.md / index.md / <name>.md
        let sub = dir.join(name);
        if sub.is_dir() {
            if let Some(skill) = load_skill_from_dir(&sub) {
                if skill.name == name {
                    return Some(skill);
                }
            }
        }
    }
    None
}

/// Load all skills from a directory (with full prompt content).
pub fn load_skills(dir: &Path) -> Vec<Skill> {
    let mut skills = Vec::new();

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return skills,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(skill) = load_skill_from_dir(&path) {
                skills.push(skill);
            }
        } else if path.extension().map_or(false, |e| e == "md") {
            match load_single_skill(&path) {
                Ok(skill) => skills.push(skill),
                Err(e) => {
                    tracing::warn!("skip skill {}: {}", path.display(), e);
                }
            }
        }
    }

    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// Load skills from multiple directories (later dirs override earlier ones).
pub fn load_skills_from_dirs(dirs: &[PathBuf]) -> Vec<Skill> {
    use std::collections::BTreeMap;

    let mut map: BTreeMap<String, Skill> = BTreeMap::new();
    for dir in dirs {
        for skill in load_skills(dir) {
            map.insert(skill.name.clone(), skill);
        }
    }
    map.into_values().collect()
}

// ── Directory-based skill loading ──────────────────────────────────────────

/// Try to load skill metadata from a subdirectory.
/// Looks for SKILL.md, index.md, or <dirname>.md.
fn load_meta_from_dir(dir: &Path) -> Option<SkillMeta> {
    let candidates = skill_file_candidates(dir);
    for path in candidates {
        if path.is_file() {
            if let Ok(meta) = load_single_meta(&path) {
                return Some(meta);
            }
        }
    }
    None
}

/// Try to load a full skill from a subdirectory.
fn load_skill_from_dir(dir: &Path) -> Option<Skill> {
    let candidates = skill_file_candidates(dir);
    for path in candidates {
        if path.is_file() {
            if let Ok(skill) = load_single_skill(&path) {
                return Some(skill);
            }
        }
    }
    None
}

/// Returns candidate skill files in a directory, in priority order.
fn skill_file_candidates(dir: &Path) -> Vec<PathBuf> {
    let dirname = dir.file_name().unwrap_or_default().to_string_lossy();
    vec![
        dir.join("SKILL.md"),
        dir.join("index.md"),
        dir.join(format!("{}.md", dirname)),
    ]
}

// ── Ensure directory exists ───────────────────────────────────────────────

/// Ensure a skill directory exists, creating it if needed.
/// Returns Ok(true) if created, Ok(false) if already existed.
pub fn ensure_dir(dir: &Path) -> std::io::Result<bool> {
    if dir.exists() {
        return Ok(false);
    }
    std::fs::create_dir_all(dir)?;
    Ok(true)
}

// ── Internal parsing ──────────────────────────────────────────────────────

/// Parse a single `.md` file into metadata only (no prompt body).
fn load_single_meta(path: &Path) -> anyhow::Result<SkillMeta> {
    let content = std::fs::read_to_string(path)?;

    let (name, description, _) = if content.starts_with("---") {
        parse_frontmatter(&content, path)
    } else {
        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        (name, String::new(), String::new())
    };

    Ok(SkillMeta {
        name,
        description,
        source: path.to_path_buf(),
    })
}

/// Parse a single `.md` file into a full Skill.
fn load_single_skill(path: &Path) -> anyhow::Result<Skill> {
    let content = std::fs::read_to_string(path)?;

    let (name, description, prompt) = if content.starts_with("---") {
        parse_frontmatter(&content, path)
    } else {
        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        (name, String::new(), content)
    };

    Ok(Skill {
        name,
        description,
        prompt,
        source: path.to_path_buf(),
    })
}

/// Parse YAML frontmatter delimited by `---`.
///
/// Returns (name, description, prompt_body).
fn parse_frontmatter(content: &str, path: &Path) -> (String, String, String) {
    let rest = &content[3..]; // skip opening ---
    let Some(end) = rest.find("\n---") else {
        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        return (name, String::new(), content.to_string());
    };

    let frontmatter = &rest[..end];
    let body_start = end + 4; // skip \n---
    let prompt = rest[body_start..].trim_start_matches('\n').to_string();

    let mut name = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let mut description = String::new();

    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("name:") {
            name = val.trim().to_string();
        } else if let Some(val) = line.strip_prefix("description:") {
            description = val.trim().to_string();
        }
    }

    (name, description, prompt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_frontmatter() {
        let content = "---\nname: test-skill\ndescription: A test\n---\nHello world";
        let (name, desc, prompt) = parse_frontmatter(content, Path::new("test.md"));
        assert_eq!(name, "test-skill");
        assert_eq!(desc, "A test");
        assert_eq!(prompt, "Hello world");
    }

    #[test]
    fn test_no_frontmatter() {
        let content = "Just a prompt\nno frontmatter";
        let (name, desc, prompt) = parse_frontmatter(content, Path::new("myskill.md"));
        assert_eq!(name, "myskill");
        assert_eq!(desc, "");
        assert_eq!(prompt, content);
    }

    #[test]
    fn test_frontmatter_no_name() {
        let content = "---\ndescription: Some desc\n---\nThe prompt";
        let (name, desc, prompt) = parse_frontmatter(content, Path::new("fallback.md"));
        assert_eq!(name, "fallback");
        assert_eq!(desc, "Some desc");
        assert_eq!(prompt, "The prompt");
    }
}
