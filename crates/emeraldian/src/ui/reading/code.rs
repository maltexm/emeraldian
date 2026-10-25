//! Dependency-free code highlighting.

use super::*;

/// Lightweight syntax highlighting for fenced code.
///
/// Keyword, string, comment and number recognition covers what makes code
/// readable at a glance. A full grammar per language would be a large
/// dependency for a feature used a few lines at a time.
pub(super) fn highlight(
    line: &str,
    lang: &str,
    palette: &Palette,
    base: Style,
) -> Vec<Span<'static>> {
    let keywords = keywords_for(lang);
    let comment = comment_prefix(lang);

    if let Some(prefix) = comment
        && line.trim_start().starts_with(prefix)
    {
        return vec![Span::styled(line.to_string(), base.fg(palette.syn_comment))];
    }

    let mut spans = Vec::new();
    let mut current = String::new();
    let mut in_string: Option<char> = None;

    macro_rules! flush {
        ($style:expr) => {
            if !current.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut current), $style));
            }
        };
    }

    for ch in line.chars() {
        if let Some(quote) = in_string {
            current.push(ch);
            if ch == quote {
                flush!(base.fg(palette.syn_string));
                in_string = None;
            }
            continue;
        }

        if ch == '"' || ch == '\'' || ch == '`' {
            flush!(base.fg(palette.text_normal));
            current.push(ch);
            in_string = Some(ch);
            continue;
        }

        if ch.is_alphanumeric() || ch == '_' {
            current.push(ch);
            continue;
        }

        // A word boundary: classify what we collected.
        if !current.is_empty() {
            let style = if keywords.contains(&current.as_str()) {
                base.fg(palette.syn_keyword).add_modifier(Modifier::BOLD)
            } else if current.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                base.fg(palette.syn_number)
            } else if ch == '(' {
                base.fg(palette.syn_function)
            } else if current.chars().next().is_some_and(char::is_uppercase) {
                base.fg(palette.syn_type)
            } else {
                base.fg(palette.text_normal)
            };
            flush!(style);
        }
        spans.push(Span::styled(ch.to_string(), base.fg(palette.syn_punct)));
    }

    if !current.is_empty() {
        let style = if in_string.is_some() {
            base.fg(palette.syn_string)
        } else if keywords.contains(&current.as_str()) {
            base.fg(palette.syn_keyword).add_modifier(Modifier::BOLD)
        } else if current.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            base.fg(palette.syn_number)
        } else {
            base.fg(palette.text_normal)
        };
        spans.push(Span::styled(current, style));
    }

    spans
}

fn keywords_for(lang: &str) -> &'static [&'static str] {
    const RUST: &[&str] = &[
        "fn", "let", "mut", "pub", "use", "mod", "struct", "enum", "impl", "trait", "for", "while",
        "loop", "if", "else", "match", "return", "self", "Self", "const", "static", "async",
        "await", "move", "where", "type", "as", "in", "ref", "dyn", "crate", "true", "false",
    ];
    const PYTHON: &[&str] = &[
        "def", "class", "import", "from", "return", "if", "elif", "else", "for", "while", "try",
        "except", "finally", "with", "as", "lambda", "yield", "async", "await", "pass", "raise",
        "in", "not", "and", "or", "None", "True", "False", "self",
    ];
    const JS: &[&str] = &[
        "function",
        "const",
        "let",
        "var",
        "return",
        "if",
        "else",
        "for",
        "while",
        "class",
        "extends",
        "import",
        "export",
        "from",
        "async",
        "await",
        "new",
        "this",
        "try",
        "catch",
        "finally",
        "throw",
        "typeof",
        "interface",
        "type",
        "true",
        "false",
        "null",
        "undefined",
    ];
    const SHELL: &[&str] = &[
        "if", "then", "else", "fi", "for", "while", "do", "done", "case", "esac", "function",
        "export", "local", "return", "echo", "cd", "set",
    ];
    const GO: &[&str] = &[
        "func",
        "package",
        "import",
        "var",
        "const",
        "type",
        "struct",
        "interface",
        "return",
        "if",
        "else",
        "for",
        "range",
        "go",
        "defer",
        "chan",
        "select",
        "switch",
        "case",
        "map",
        "nil",
        "true",
        "false",
    ];

    match lang.to_lowercase().as_str() {
        "rust" | "rs" => RUST,
        "python" | "py" => PYTHON,
        "javascript" | "js" | "typescript" | "ts" | "tsx" | "jsx" => JS,
        "bash" | "sh" | "shell" | "zsh" => SHELL,
        "go" => GO,
        _ => &[],
    }
}

fn comment_prefix(lang: &str) -> Option<&'static str> {
    match lang.to_lowercase().as_str() {
        "rust" | "rs" | "javascript" | "js" | "typescript" | "ts" | "go" | "c" | "cpp" | "java" => {
            Some("//")
        }
        "python" | "py" | "bash" | "sh" | "shell" | "zsh" | "yaml" | "yml" | "toml" | "ruby"
        | "rb" => Some("#"),
        "sql" | "lua" | "haskell" => Some("--"),
        _ => None,
    }
}
