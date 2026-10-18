//! Shared boundary scanning for f-string interpolation. Escapes in literal text
//! are decoded by the lexer; expressions are preserved and parsed independently.

/// `start` is the byte immediately after `{`; returns the matching `}` byte.
pub(crate) fn interpolation_end(source: &str, start: usize) -> Result<usize, String> {
    let bytes = source.as_bytes();
    let mut i = start;
    let mut depth = 1usize;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let raw = i > 0
                    && bytes[i - 1] == b'r'
                    && (i < 2
                        || !super::lexer::is_ident_continue(
                            source[..i - 1].chars().next_back().unwrap_or(' '),
                        ));
                let triple = source[i..].starts_with("\"\"\"");
                let delimiter = if triple { "\"\"\"" } else { "\"" };
                i += delimiter.len();
                let mut closed = false;
                while i < bytes.len() {
                    if !raw && bytes[i] == b'\\' {
                        i += 1;
                        if i < bytes.len() {
                            i += source[i..].chars().next().map_or(0, char::len_utf8);
                        }
                    } else if source[i..].starts_with(delimiter) {
                        i += delimiter.len();
                        closed = true;
                        break;
                    } else {
                        i += source[i..].chars().next().map_or(0, char::len_utf8);
                    }
                }
                if !closed {
                    return Err("unterminated string in f-string expression".into());
                }
            }
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < bytes.len() && !source[i..].starts_with("*/") {
                    i += source[i..].chars().next().map_or(0, char::len_utf8);
                }
                if i == bytes.len() {
                    return Err("unterminated comment in f-string expression".into());
                }
                i += 2;
            }
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(i);
                }
                i += 1;
            }
            _ => i += source[i..].chars().next().map_or(0, char::len_utf8),
        }
    }
    Err("unclosed '{' in f-string".into())
}
