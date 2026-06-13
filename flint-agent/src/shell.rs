//! Cross-platform shell detection.
//!
//! On Windows, prefers Git Bash (`sh.exe`) over `cmd.exe` because cmd.exe
//! mangles quotes, braces, and URLs in command strings.

use std::path::PathBuf;

/// Find the best shell to use on Windows.
/// Prefers Git Bash (`sh.exe`) over `cmd.exe`.
/// Returns the path to the shell executable.
///
/// On non-Windows, always returns `"sh"`.
pub fn find_shell() -> PathBuf {
    if cfg!(target_os = "windows") {
        find_shell_on_windows()
    } else {
        PathBuf::from("sh")
    }
}

/// Check if the given shell path is a Unix-style shell (sh/bash)
/// as opposed to cmd.exe.
pub fn is_unix_shell(shell: &PathBuf) -> bool {
    shell
        .file_name()
        .and_then(|f| f.to_str())
        .map_or(false, |name| {
            name == "sh" || name == "sh.exe" || name == "bash" || name == "bash.exe"
        })
}

/// Convert Windows backslash paths to forward slashes for Unix-style shells.
///
/// Detects patterns like `D:\path\to\file` and converts to `D:/path/to/file`.
/// This prevents bash from interpreting `\a`, `\2` etc. as escape sequences.
pub fn fix_windows_paths(cmd: &str) -> std::borrow::Cow<str> {
    // Quick check: does the command contain a Windows drive letter like "D:\"?
    let bytes = cmd.as_bytes();
    let has_drive = bytes.windows(3).any(|w| {
        w[0].is_ascii_alphabetic() && w[1] == b':' && w[2] == b'\\'
    });
    if !has_drive {
        return std::borrow::Cow::Borrowed(cmd);
    }
    // Replace backslashes with forward slashes in Windows paths
    let mut result = String::with_capacity(cmd.len());
    let mut chars = cmd.chars().peekable();
    let mut in_windows_path = false;
    while let Some(c) = chars.next() {
        if c.is_ascii_alphabetic() && chars.peek() == Some(&':') {
            result.push(c);
            result.push(':');
            chars.next(); // consume ':'
            in_windows_path = true;
            // Convert leading backslashes
            while chars.peek() == Some(&'\\') {
                result.push('/');
                chars.next();
            }
        } else if in_windows_path && c == '\\' {
            // Continue converting backslashes within the path
            result.push('/');
        } else {
            if in_windows_path && c.is_whitespace() {
                in_windows_path = false;
            }
            result.push(c);
        }
    }
    std::borrow::Cow::Owned(result)
}

/// Expand Windows `%VAR%` environment variable syntax to `$VAR` for Unix shells.
///
/// `%USERPROFILE%` → `$USERPROFILE`, `%PATH%` → `$PATH`, etc.
/// Git Bash inherits Windows env vars, so `$USERPROFILE` works but `%USERPROFILE%` does not.
pub fn expand_windows_env_vars(cmd: &str) -> std::borrow::Cow<str> {
    if !cmd.contains('%') {
        return std::borrow::Cow::Borrowed(cmd);
    }
    let mut result = String::with_capacity(cmd.len());
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            // Read until next '%' to get the variable name
            let mut var_name = String::new();
            loop {
                match chars.next() {
                    Some('%') if !var_name.is_empty() => {
                        // Found closing % — expand to $VAR
                        result.push('$');
                        result.push_str(&var_name);
                        break;
                    }
                    Some(ch) if var_name.len() < 64 => {
                        var_name.push(ch);
                    }
                    _ => {
                        // No closing % or too long — treat as literal
                        result.push('%');
                        result.push_str(&var_name);
                        break;
                    }
                }
            }
        } else {
            result.push(c);
        }
    }
    std::borrow::Cow::Owned(result)
}

/// Apply all Windows→Unix fixes for commands running in Git Bash.
/// Combines env var expansion and path conversion.
pub fn fix_for_unix_shell(cmd: &str) -> String {
    let expanded = expand_windows_env_vars(cmd);
    let fixed = fix_windows_paths(&expanded);
    fixed.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fix_windows_paths() {
        // Basic Windows path
        assert_eq!(
            fix_windows_paths("cd D:\\ai\\2026\\code\\flint"),
            "cd D:/ai/2026/code/flint"
        );
        // No Windows path — unchanged
        assert_eq!(
            fix_windows_paths("ls -la /tmp"),
            "ls -la /tmp"
        );
        // Mixed
        assert_eq!(
            fix_windows_paths("cp D:\\file.txt /tmp/"),
            "cp D:/file.txt /tmp/"
        );
        // Multiple paths
        assert_eq!(
            fix_windows_paths("diff D:\\a\\1.txt D:\\b\\2.txt"),
            "diff D:/a/1.txt D:/b/2.txt"
        );
        // No backslash after drive letter
        assert_eq!(
            fix_windows_paths("echo D:"),
            "echo D:"
        );
    }

    #[test]
    fn test_expand_windows_env_vars() {
        // Basic expansion
        assert_eq!(
            expand_windows_env_vars("echo %USERPROFILE%\\.flint\\.env"),
            "echo $USERPROFILE\\.flint\\.env"
        );
        // Multiple vars
        assert_eq!(
            expand_windows_env_vars("%USERPROFILE%\\%APPDATA%"),
            "$USERPROFILE\\$APPDATA"
        );
        // No percent signs — unchanged
        assert_eq!(
            expand_windows_env_vars("echo hello"),
            "echo hello"
        );
        // Single percent — literal
        assert_eq!(
            expand_windows_env_vars("echo 50%"),
            "echo 50%"
        );
    }

    #[test]
    fn test_fix_for_unix_shell() {
        // Combined: env var + path fix
        assert_eq!(
            fix_for_unix_shell("echo %USERPROFILE% > D:\\test\\out.txt"),
            "echo $USERPROFILE > D:/test/out.txt"
        );
    }
}

fn find_shell_on_windows() -> PathBuf {
    // Check PATH for sh.exe (Git Bash puts itself on PATH)
    if let Ok(path_env) = std::env::var("PATH") {
        for dir in path_env.split(';') {
            let sh = PathBuf::from(dir).join("sh.exe");
            if sh.exists() {
                return sh;
            }
            let sh = PathBuf::from(dir).join("sh");
            if sh.exists() {
                return sh;
            }
        }
    }
    // Common Git Bash locations
    for candidate in &[
        "C:\\Program Files\\Git\\bin\\sh.exe",
        "C:\\Program Files (x86)\\Git\\bin\\sh.exe",
    ] {
        let p = PathBuf::from(candidate);
        if p.exists() {
            return p;
        }
    }
    // Fallback: cmd.exe
    PathBuf::from("cmd")
}
