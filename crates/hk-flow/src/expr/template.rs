//! Splits `text {{ expr }} text` templates. The scanner is string- and brace-aware so
//! `{{ {a: {b: 1}}.a }}` and `{{ "}}" }}` parse like in n8n's editor.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    Text(String),
    /// Trimmed JS expression source.
    Expr(String),
}

/// An unterminated `{{` is kept as literal text (n8n renders it verbatim).
pub fn parse_template(s: &str) -> Vec<Part> {
    let b = s.as_bytes();
    let mut parts = Vec::new();
    let mut text_start = 0;
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'{' && b[i + 1] == b'{' {
            match find_close(b, i + 2) {
                Some(end) => {
                    if text_start < i {
                        parts.push(Part::Text(s[text_start..i].to_owned()));
                    }
                    let body = s[i + 2..end].trim();
                    if !body.is_empty() {
                        parts.push(Part::Expr(body.to_owned()));
                    }
                    i = end + 2;
                    text_start = i;
                    continue;
                }
                None => break,
            }
        }
        i += 1;
    }
    if text_start < s.len() {
        parts.push(Part::Text(s[text_start..].to_owned()));
    }
    parts
}

/// Index of the `}}` closing an expression that starts at `from`.
fn find_close(b: &[u8], from: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = from;
    while i < b.len() {
        match b[i] {
            q @ (b'\'' | b'"' | b'`') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'{' => depth += 1,
            b'}' => {
                if depth > 0 {
                    depth -= 1;
                } else if b.get(i + 1) == Some(&b'}') {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Part {
        Part::Text(s.into())
    }
    fn e(s: &str) -> Part {
        Part::Expr(s.into())
    }

    #[test]
    fn splits() {
        assert_eq!(parse_template("a {{ $json.x }} b"), [t("a "), e("$json.x"), t(" b")]);
        assert_eq!(parse_template("{{1}}{{2}}"), [e("1"), e("2")]);
        assert_eq!(parse_template("plain"), [t("plain")]);
        assert_eq!(parse_template("open {{ x"), [t("open {{ x")]);
    }

    #[test]
    fn braces_and_strings() {
        assert_eq!(parse_template("{{ {a:{b:1}}.a.b }}"), [e("{a:{b:1}}.a.b")]);
        assert_eq!(parse_template("{{ \"}}\" + 'x' }}"), [e("\"}}\" + 'x'")]);
    }
}
