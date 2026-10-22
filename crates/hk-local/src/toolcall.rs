//! Small local models call tools in-text: `<tool_call>{"name": ..., "arguments": {...}}</tool_call>`
//! (the Hermes / Qwen convention). This splits a token stream into visible text and calls, holding
//! back only what could still turn out to be a tag.

use serde_json::Value;

const OPEN: &str = "<tool_call>";
const CLOSE: &str = "</tool_call>";

#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    Text(String),
    Call { name: String, arguments: Value },
}

#[derive(Debug, Default)]
pub struct ToolStreamParser {
    buf: String,
    in_call: bool,
}

/// Longest suffix of `s` that is a proper prefix of `tag`.
fn partial_tag_len(s: &str, tag: &str) -> usize {
    (1..tag.len().min(s.len() + 1)).rev().find(|&n| s.is_char_boundary(s.len() - n) && tag.starts_with(&s[s.len() - n..])).unwrap_or(0)
}

fn parse_call(body: &str) -> Option<Out> {
    let v: Value = serde_json::from_str(body.trim()).ok()?;
    let name = v.get("name")?.as_str()?.to_owned();
    let arguments = match v.get("arguments").or_else(|| v.get("parameters")) {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::String(s.clone())),
        Some(other) => other.clone(),
        None => Value::Object(Default::default()),
    };
    Some(Out::Call { name, arguments })
}

impl ToolStreamParser {
    pub fn push(&mut self, piece: &str) -> Vec<Out> {
        self.buf.push_str(piece);
        let mut out = Vec::new();
        loop {
            if self.in_call {
                let Some(end) = self.buf.find(CLOSE) else { break };
                let body: String = self.buf.drain(..end + CLOSE.len()).collect();
                self.in_call = false;
                match parse_call(&body[..body.len() - CLOSE.len()]) {
                    Some(c) => out.push(c),
                    // Unparseable call: show it so the user (and the model's next turn) can see it.
                    None => out.push(Out::Text(format!("{OPEN}{body}"))),
                }
            } else if let Some(start) = self.buf.find(OPEN) {
                if start > 0 {
                    out.push(Out::Text(self.buf.drain(..start).collect()));
                }
                self.buf.drain(..OPEN.len());
                self.in_call = true;
            } else {
                let keep = partial_tag_len(&self.buf, OPEN);
                let emit = self.buf.len() - keep;
                if emit > 0 {
                    out.push(Out::Text(self.buf.drain(..emit).collect()));
                }
                break;
            }
        }
        out
    }

    /// End of generation: flush what was held back. An unterminated call is shown as text.
    pub fn finish(&mut self) -> Vec<Out> {
        let rest = std::mem::take(&mut self.buf);
        let was = std::mem::replace(&mut self.in_call, false);
        match (was, rest.is_empty()) {
            (_, true) => Vec::new(),
            (true, false) => match parse_call(&rest) {
                Some(c) => vec![c],
                None => vec![Out::Text(format!("{OPEN}{rest}"))],
            },
            (false, false) => vec![Out::Text(rest)],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(chunks: &[&str]) -> Vec<Out> {
        let mut p = ToolStreamParser::default();
        let mut v: Vec<Out> = chunks.iter().flat_map(|c| p.push(c)).collect();
        v.extend(p.finish());
        // merge adjacent text for comparison
        let mut merged: Vec<Out> = Vec::new();
        for o in v {
            match (merged.last_mut(), o) {
                (Some(Out::Text(a)), Out::Text(b)) => a.push_str(&b),
                (_, o) => merged.push(o),
            }
        }
        merged
    }

    #[test]
    fn plain_text_passes_through_even_with_lt() {
        assert_eq!(run(&["a < b ", "and <t", "ag>"]), vec![Out::Text("a < b and <tag>".into())]);
    }

    #[test]
    fn call_split_across_tokens() {
        let r = run(&["Sure.\n<tool", "_call>{\"name\": \"read\", ", "\"arguments\": {\"path\": \"a\"}}</tool_", "call>"]);
        assert_eq!(r, vec![Out::Text("Sure.\n".into()), Out::Call { name: "read".into(), arguments: json!({"path": "a"}) }]);
    }

    #[test]
    fn two_calls_and_string_arguments() {
        let r = run(&["<tool_call>{\"name\":\"a\",\"arguments\":\"{\\\"x\\\":1}\"}</tool_call><tool_call>{\"name\":\"b\"}</tool_call>"]);
        assert_eq!(r, vec![Out::Call { name: "a".into(), arguments: json!({"x": 1}) }, Out::Call { name: "b".into(), arguments: json!({}) }]);
    }

    #[test]
    fn broken_call_is_shown_as_text() {
        let r = run(&["<tool_call>nope</tool_call>"]);
        assert_eq!(r, vec![Out::Text("<tool_call>nope</tool_call>".into())]);
    }
}
