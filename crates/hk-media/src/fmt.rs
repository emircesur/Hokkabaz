//! Deterministic number and filtergraph text formatting.

/// Shortest stable decimal (max 3 places) so graph strings are reproducible.
pub(crate) fn num(v: f64) -> String {
    let mut t = format!("{v:.3}");
    if t.contains('.') {
        while t.ends_with('0') {
            t.pop();
        }
        if t.ends_with('.') {
            t.pop();
        }
    }
    if t == "-0" { "0".to_string() } else { t }
}

/// Escape a literal for use as an option value inside a filtergraph (two ffmpeg escape levels).
pub(crate) fn escape_value(v: &str) -> String {
    let mut l1 = String::new();
    for c in v.chars() {
        if matches!(c, '\\' | '\'' | ':') {
            l1.push('\\');
        }
        l1.push(c);
    }
    let mut l2 = String::new();
    for c in l1.chars() {
        if matches!(c, '\\' | '\'' | '[' | ']' | ',' | ';') {
            l2.push('\\');
        }
        l2.push(c);
    }
    l2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(num(1.0), "1");
        assert_eq!(num(1.5), "1.5");
        assert_eq!(num(0.12349), "0.123");
        assert_eq!(num(-0.0001), "0");
        assert_eq!(num(100.0), "100");
    }

    #[test]
    fn escaping() {
        assert_eq!(escape_value("a b.ass"), "a b.ass");
        assert_eq!(escape_value(r"C:\x'y"), r"C\\:\\\\x\\\'y");
    }
}
