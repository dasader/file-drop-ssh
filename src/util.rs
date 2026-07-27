//! Pure string helpers. No Win32 here so these stay unit-testable.

/// Wrap in double quotes, escaping embedded quotes — used for the *remote*
/// portion of ssh/scp commands (the remote shell parses these).
pub fn shell_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\\\""))
}

/// Collapse whitespace to `_` and drop anything that is not alphanumeric,
/// `.`, `_`, or `-`.
pub fn sanitize_filename(name: &str) -> String {
    let mut s = String::new();
    let mut prev_ws = false;
    for c in name.chars() {
        if c.is_whitespace() {
            if !prev_ws {
                s.push('_');
            }
        } else if c.is_alphanumeric() || c == '.' || c == '_' || c == '-' {
            s.push(c);
        }
        prev_ws = c.is_whitespace();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_escapes() {
        assert_eq!(shell_quote("/a/b"), "\"/a/b\"");
        assert_eq!(shell_quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(shell_quote("with space"), "\"with space\"");
    }

    #[test]
    fn sanitize_collapses_and_filters() {
        assert_eq!(sanitize_filename("my file.png"), "my_file.png");
        assert_eq!(sanitize_filename("a   b"), "a_b");
        assert_eq!(sanitize_filename("we!@#ird$.txt"), "weird.txt");
        assert_eq!(sanitize_filename("keep-dash_under.ext"), "keep-dash_under.ext");
        // Only whitespace runs collapse — an existing '_' next to a space stays.
        assert_eq!(sanitize_filename("a_ b"), "a__b");
        assert_eq!(sanitize_filename(" lead trail "), "_lead_trail_");
    }
}
