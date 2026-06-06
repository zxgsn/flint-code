//! Simple markdown renderer for terminal output.
//!
//! Provides basic markdown formatting with ANSI colors:
//! - Headers (colored and bold)
//! - Code blocks (with syntax highlighting hint)
//! - Inline code (colored)
//! - Bold text (bright/bold)
//! - Italic text (dimmed)
//! - Lists (with bullet points)
//! - Links (underlined)

/// Render markdown text to terminal with ANSI formatting.
pub fn render_markdown(text: &str) -> String {
    let mut output = String::new();
    let mut in_code_block = false;
    let mut code_block_lang = String::new();

    for line in text.lines() {
        // Handle code blocks
        if line.starts_with("```") {
            if in_code_block {
                // End of code block
                output.push_str("\x1b[0m"); // Reset color
                output.push_str("│ \x1b[90m```\x1b[0m\n");
                in_code_block = false;
                code_block_lang.clear();
            } else {
                // Start of code block
                in_code_block = true;
                code_block_lang = line[3..].trim().to_string();
                if code_block_lang.is_empty() {
                    output.push_str("│ \x1b[90m```\x1b[0m\n");
                } else {
                    output.push_str(&format!("│ \x1b[90m```{}\x1b[0m\n", code_block_lang));
                }
                output.push_str("\x1b[36m"); // Cyan for code
            }
            continue;
        }

        if in_code_block {
            // Inside code block - just output with cyan color
            output.push_str("│ ");
            output.push_str(line);
            output.push('\n');
            continue;
        }

        // Headers
        if line.starts_with("# ") {
            output.push_str(&format!("\x1b[1;33m{}\x1b[0m\n", &line[2..]));
            continue;
        }
        if line.starts_with("## ") {
            output.push_str(&format!("\x1b[1;32m{}\x1b[0m\n", &line[3..]));
            continue;
        }
        if line.starts_with("### ") {
            output.push_str(&format!("\x1b[1;36m{}\x1b[0m\n", &line[4..]));
            continue;
        }

        // Lists
        if line.starts_with("- ") || line.starts_with("* ") {
            output.push_str(&format!("  • {}\n", render_inline(&line[2..])));
            continue;
        }
        if line.starts_with("  - ") || line.starts_with("  * ") {
            output.push_str(&format!("    ◦ {}\n", render_inline(&line[4..])));
            continue;
        }

        // Numbered lists
        if let Some(pos) = line.find(". ") {
            if line[..pos].chars().all(|c| c.is_ascii_digit()) {
                output.push_str(&format!("  {}. {}\n", &line[..pos], render_inline(&line[pos + 2..])));
                continue;
            }
        }

        // Empty lines
        if line.trim().is_empty() {
            output.push('\n');
            continue;
        }

        // Regular text
        output.push_str(&render_inline(line));
        output.push('\n');
    }

    output
}

/// Render inline markdown elements.
fn render_inline(text: &str) -> String {
    let mut result = String::new();
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            // Inline code
            '`' => {
                let mut code = String::new();
                while let Some(&next) = chars.peek() {
                    if next == '`' {
                        chars.next();
                        break;
                    }
                    code.push(next);
                    chars.next();
                }
                result.push_str(&format!("\x1b[36m{}\x1b[0m", code));
            }
            // Bold
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next();
                    let mut bold = String::new();
                    while let Some(&next) = chars.peek() {
                        if next == '*' {
                            chars.next();
                            if chars.peek() == Some(&'*') {
                                chars.next();
                                break;
                            }
                            bold.push('*');
                            continue;
                        }
                        bold.push(next);
                        chars.next();
                    }
                    result.push_str(&format!("\x1b[1m{}\x1b[0m", bold));
                } else {
                    // Italic
                    let mut italic = String::new();
                    while let Some(&next) = chars.peek() {
                        if next == '*' {
                            chars.next();
                            break;
                        }
                        italic.push(next);
                        chars.next();
                    }
                    result.push_str(&format!("\x1b[3m{}\x1b[0m", italic));
                }
            }
            // Links [text](url)
            '[' => {
                let mut link_text = String::new();
                while let Some(&next) = chars.peek() {
                    if next == ']' {
                        chars.next();
                        break;
                    }
                    link_text.push(next);
                    chars.next();
                }
                if chars.peek() == Some(&'(') {
                    chars.next();
                    let mut url = String::new();
                    while let Some(&next) = chars.peek() {
                        if next == ')' {
                            chars.next();
                            break;
                        }
                        url.push(next);
                        chars.next();
                    }
                    result.push_str(&format!("\x1b[4;34m{}\x1b[0m", link_text));
                } else {
                    result.push('[');
                    result.push_str(&link_text);
                }
            }
            _ => {
                result.push(c);
            }
        }
    }

    result
}

/// Print rendered markdown to stdout.
pub fn print_markdown(text: &str) {
    let rendered = render_markdown(text);
    print!("{}", rendered);
}
