//! Minimal incremental Server-Sent-Events decoder.
//!
//! Only `data:` fields matter for chat completions; comments (`: keep-alive`),
//! `event:`, `id:` and `retry:` lines are ignored.

/// Splits an arbitrary byte stream into SSE `data` payloads.
#[derive(Debug, Default)]
pub struct SseDecoder {
    buf: Vec<u8>,
    data: Vec<String>,
}

impl SseDecoder {
    /// Feed bytes; returns every event completed by them (joined `data:` lines).
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        // Lines are cut on raw `\n`, so a multi-byte char split across network
        // chunks is never decoded half-way.
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.line(&String::from_utf8_lossy(&line), &mut out);
        }
        out
    }

    /// End of body: flush a trailing unterminated line/event.
    pub fn finish(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.buf.is_empty() {
            let line = String::from_utf8_lossy(&std::mem::take(&mut self.buf)).into_owned();
            self.line(line.trim_end_matches('\r'), &mut out);
        }
        self.line("", &mut out);
        out
    }

    fn line(&mut self, line: &str, out: &mut Vec<String>) {
        if line.is_empty() {
            if !self.data.is_empty() {
                out.push(self.data.join("\n"));
                self.data.clear();
            }
        } else if let Some(rest) = line.strip_prefix("data:") {
            self.data
                .push(rest.strip_prefix(' ').unwrap_or(rest).to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_across_chunks_and_skips_comments() {
        let mut d = SseDecoder::default();
        assert!(d.push(b": keep-alive\n\ndata: {\"a\"").is_empty());
        let ev = d.push(b":1}\r\n\r\ndata: [DONE]\n\n");
        assert_eq!(ev, vec!["{\"a\":1}".to_owned(), "[DONE]".to_owned()]);
    }

    #[test]
    fn flushes_unterminated_tail() {
        let mut d = SseDecoder::default();
        assert!(d.push(b"data: x").is_empty());
        assert_eq!(d.finish(), vec!["x".to_owned()]);
    }
}
