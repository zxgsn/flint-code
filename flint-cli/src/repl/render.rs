//! Terminal markdown renderer.
//!
//! Converts markdown text to ANSI-formatted terminal output.
//! Handles: headers, bold, italic, inline code, code blocks,
//! lists, blockquotes, links, and horizontal rules.

use std::io::Write;

// ── ANSI helpers ───────────────────────────────────────────────────────

fn reset() -> &'static str { "\x1b[0m" }
fn bold() -> &'static str { "\x1b[1m" }
fn dim() -> &'static str { "\x1b[2m" }
fn italic() -> &'static str { "\x1b[3m" }
fn underline() -> &'static str { "\x1b[4m" }
fn fg_cyan() -> &'static str { "\x1b[36m" }
fn fg_yellow() -> &'static str { "\x1b[33m" }
fn fg_green() -> &'static str { "\x1b[32m" }
fn fg_red() -> &'static str { "\x1b[31m" }
fn fg_magenta() -> &'static str { "\x1b[35m" }
fn fg_blue() -> &'static str { "\x1b[34m" }
fn bg_dark() -> &'static str { "\x1b[48;5;236m" }
fn fg_gray() -> &'static str { "\x1b[38;5;245m" }

// ── Public API ─────────────────────────────────────────────────────────

/// Render markdown text to the terminal with ANSI formatting.
/// Writes directly to the given writer (typically stdout or a string buffer).
pub fn render_markdown<W: Write>(out: &mut W, text: &str) {
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    let mut in_code_block = false;
    let mut code_lang = String::new();

    while i < lines.len() {
        let line = lines[i];

        // ── Fenced code block ──────────────────────────────────────────
        if line.trim_start().starts_with("```") {
            if in_code_block {
                // End of code block
                writeln!(out, "{}{}│{} ", fg_gray(), dim(), reset()).ok();
                in_code_block = false;
                i += 1;
                continue;
            }
            // Start of code block
            in_code_block = true;
            code_lang = line.trim_start().trim_start_matches('`').trim().to_string();
            let lang_label = if code_lang.is_empty() { String::new() } else { format!(" {}{}{}", fg_cyan(), code_lang, fg_gray()) };
            writeln!(out, "{}{}┌───{}{}", fg_gray(), dim(), lang_label, reset()).ok();
            i += 1;
            continue;
        }

        if in_code_block {
            // Inside code block — render with background
            writeln!(out, "{}{}│{} {}{}{}", fg_gray(), dim(), reset(), dim(), line, reset()).ok();
            i += 1;
            continue;
        }

        // ── Horizontal rule ────────────────────────────────────────────
        if is_horizontal_rule(line) {
            writeln!(out, "{}{}{}", dim(), "─".repeat(60), reset()).ok();
            i += 1;
            continue;
        }

        // ── Headers ────────────────────────────────────────────────────
        if let Some(level) = header_level(line) {
            let content = line.trim_start().trim_start_matches('#').trim();
            let (color, prefix) = match level {
                1 => (fg_yellow(), "█ "),
                2 => (fg_cyan(), "▓ "),
                3 => (fg_blue(), "▒ "),
                _ => (fg_magenta(), "░ "),
            };
            writeln!(out, "{}{}{}{} {}{}", color, bold(), prefix, content, reset(), reset()).ok();
            i += 1;
            continue;
        }

        // ── Blockquote ─────────────────────────────────────────────────
        if line.trim_start().starts_with('>') {
            let content = line.trim_start().trim_start_matches('>').trim();
            writeln!(out, "{}  │ {}{}{}", dim(), fg_gray(), render_inline(content), reset()).ok();
            i += 1;
            continue;
        }

        // ── Unordered list ─────────────────────────────────────────────
        if let Some(content) = parse_list_item(line) {
            writeln!(out, "  {}•{} {}", fg_cyan(), reset(), render_inline(content)).ok();
            i += 1;
            continue;
        }

        // ── Ordered list ───────────────────────────────────────────────
        if let Some((num, content)) = parse_ordered_list_item(line) {
            writeln!(out, "  {}{}{}.{} {}", fg_cyan(), num, ".", reset(), render_inline(content)).ok();
            i += 1;
            continue;
        }

        // ── Empty line ─────────────────────────────────────────────────
        if line.trim().is_empty() {
            writeln!(out).ok();
            i += 1;
            continue;
        }

        // ── Regular paragraph ──────────────────────────────────────────
        // Collect continuation lines for word-wrapping
        let mut para = String::from(line);
        while i + 1 < lines.len() && !lines[i + 1].trim().is_empty()
            && !lines[i + 1].trim_start().starts_with('#')
            && !lines[i + 1].trim_start().starts_with("```")
            && !lines[i + 1].trim_start().starts_with('>')
            && !is_horizontal_rule(lines[i + 1])
            && parse_list_item(lines[i + 1]).is_none()
            && parse_ordered_list_item(lines[i + 1]).is_none()
        {
            i += 1;
            para.push(' ');
            para.push_str(lines[i].trim());
        }
        writeln!(out, "{}", render_inline(&para)).ok();
        i += 1;
    }

    // Close unclosed code block
    if in_code_block {
        writeln!(out, "{}{}└───{}", fg_gray(), dim(), reset()).ok();
    }
}

/// Render a single line to a string (for use in other contexts).
pub fn render_markdown_line(text: &str) -> String {
    render_inline(text)
}

// ── Inline rendering ───────────────────────────────────────────────────

fn render_inline(text: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // ── Inline code ────────────────────────────────────────────────
        if chars[i] == '`' {
            let end = find_closing(&chars, i + 1, '`');
            if end.is_some() {
                let code: String = chars[i + 1..end.unwrap()].iter().collect();
                out.push_str(bg_dark());
                out.push_str(fg_yellow());
                out.push(' ');
                out.push_str(&code);
                out.push(' ');
                out.push_str(reset());
                i = end.unwrap() + 1;
                continue;
            }
        }

        // ── Bold (**text**) ────────────────────────────────────────────
        if i + 1 < len && chars[i] == '*' && chars[i + 1] == '*' {
            let end = find_double_closing(&chars, i + 2, '*');
            if end.is_some() {
                let content: String = chars[i + 2..end.unwrap()].iter().collect();
                out.push_str(bold());
                out.push_str(&content);
                out.push_str(reset());
                i = end.unwrap() + 2;
                continue;
            }
        }

        // ── Bold alternative (__text__) ────────────────────────────────
        if i + 1 < len && chars[i] == '_' && chars[i + 1] == '_' {
            let end = find_double_closing(&chars, i + 2, '_');
            if end.is_some() {
                let content: String = chars[i + 2..end.unwrap()].iter().collect();
                out.push_str(bold());
                out.push_str(&content);
                out.push_str(reset());
                i = end.unwrap() + 2;
                continue;
            }
        }

        // ── Italic (*text*) ────────────────────────────────────────────
        if chars[i] == '*' && (i == 0 || chars[i - 1] == ' ') {
            let end = find_closing(&chars, i + 1, '*');
            if end.is_some() && end.unwrap() > i + 1 {
                let content: String = chars[i + 1..end.unwrap()].iter().collect();
                out.push_str(italic());
                out.push_str(&content);
                out.push_str(reset());
                i = end.unwrap() + 1;
                continue;
            }
        }

        // ── Strikethrough (~~text~~) ───────────────────────────────────
        if i + 1 < len && chars[i] == '~' && chars[i + 1] == '~' {
            let end = find_double_closing(&chars, i + 2, '~');
            if end.is_some() {
                let content: String = chars[i + 2..end.unwrap()].iter().collect();
                out.push_str(dim());
                out.push_str(&format!("̶{}", content.chars().map(|c| format!("{}\u{0336}", c)).collect::<String>()));
                out.push_str(reset());
                i = end.unwrap() + 2;
                continue;
            }
        }

        // ── Link [text](url) ───────────────────────────────────────────
        if chars[i] == '[' {
            if let Some((text_part, url_part, end_idx)) = parse_link(&chars, i) {
                out.push_str(underline());
                out.push_str(fg_blue());
                out.push_str(&text_part);
                out.push_str(reset());
                out.push_str(&format!(" {}({}){}", fg_gray(), url_part, reset()));
                i = end_idx;
                continue;
            }
        }

        // ── Regular character ──────────────────────────────────────────
        out.push(chars[i]);
        i += 1;
    }

    out
}

// ── Helpers ────────────────────────────────────────────────────────────

fn header_level(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return None;
    }
    let level = trimmed.chars().take_while(|&c| c == '#').count();
    if level > 0 && level <= 6 && trimmed.len() > level && trimmed.as_bytes()[level] == b' ' {
        Some(level)
    } else {
        None
    }
}

fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    (trimmed.starts_with("---") || trimmed.starts_with("***") || trimmed.starts_with("___"))
        && trimmed.chars().all(|c| c == '-' || c == '*' || c == '_' || c == ' ')
        && trimmed.chars().filter(|c| *c != ' ').count() >= 3
}

fn parse_list_item(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if (trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ "))
        && trimmed.len() > 2
    {
        Some(&trimmed[2..])
    } else {
        None
    }
}

fn parse_ordered_list_item(line: &str) -> Option<(String, &str)> {
    let trimmed = line.trim_start();
    let bytes = trimmed.as_bytes();
    let mut digits = 0;
    while digits < bytes.len() && bytes[digits].is_ascii_digit() {
        digits += 1;
    }
    if digits > 0 && digits + 1 < bytes.len() && bytes[digits] == b'.' && bytes[digits + 1] == b' ' {
        Some((trimmed[..digits].to_string(), &trimmed[digits + 2..]))
    } else {
        None
    }
}

fn find_closing(chars: &[char], start: usize, ch: char) -> Option<usize> {
    for i in start..chars.len() {
        if chars[i] == ch {
            return Some(i);
        }
    }
    None
}

fn find_double_closing(chars: &[char], start: usize, ch: char) -> Option<usize> {
    for i in start..chars.len() - 1 {
        if chars[i] == ch && chars[i + 1] == ch {
            return Some(i);
        }
    }
    None
}

fn parse_link(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    // Find closing ]
    let close_bracket = find_closing(chars, start + 1, ']')?;
    if close_bracket + 1 >= chars.len() || chars[close_bracket + 1] != '(' {
        return None;
    }
    let close_paren = find_closing(chars, close_bracket + 2, ')')?;
    let text: String = chars[start + 1..close_bracket].iter().collect();
    let url: String = chars[close_bracket + 2..close_paren].iter().collect();
    Some((text, url, close_paren + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header() {
        let mut buf = Vec::new();
        render_markdown(&mut buf, "# Hello\n## World");
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("Hello"));
        assert!(out.contains("World"));
    }

    #[test]
    fn test_code_block() {
        let mut buf = Vec::new();
        render_markdown(&mut buf, "```rust\nfn main() {}\n```");
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("fn main()"));
        assert!(out.contains("rust"));
    }

    #[test]
    fn test_bold() {
        let out = render_inline("hello **world** end");
        assert!(out.contains("\x1b[1mworld\x1b[0m"));
    }

    #[test]
    fn test_inline_code() {
        let out = "use `println!` here";
        let rendered = render_inline(out);
        assert!(rendered.contains("println!"));
    }

    #[test]
    fn test_list() {
        let mut buf = Vec::new();
        render_markdown(&mut buf, "- item 1\n- item 2");
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("•"));
        assert!(out.contains("item 1"));
    }
}
