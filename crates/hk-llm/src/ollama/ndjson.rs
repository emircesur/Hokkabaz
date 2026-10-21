//! Newline-delimited JSON framing and the generic stream driver shared by
//! chat and pull responses.

use std::collections::VecDeque;
use std::fmt::Display;
use std::time::Duration;

use bytes::Bytes;
use futures::stream::BoxStream;
use futures::{Stream, StreamExt, stream};
use tokio_util::sync::CancellationToken;

use crate::{LlmError, Result};

/// Splits a byte stream into lines. Buffers raw bytes so a multi-byte UTF-8
/// character split across network chunks stays intact.
#[derive(Debug, Default)]
pub(super) struct LineDecoder {
    buf: Vec<u8>,
}

impl LineDecoder {
    /// Feed a chunk; returns the complete non-empty lines it finished.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            push_line(&mut out, &line);
        }
        out
    }

    /// Body ended; flush an unterminated last line.
    pub fn finish(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        let rest = std::mem::take(&mut self.buf);
        push_line(&mut out, &rest);
        out
    }
}

fn push_line(out: &mut Vec<String>, raw: &[u8]) {
    let s = String::from_utf8_lossy(raw);
    let s = s.trim();
    if !s.is_empty() {
        out.push(s.to_owned());
    }
}

/// Per-endpoint line interpreter.
pub(super) trait LineParser: Send + 'static {
    type Out: Send + 'static;

    /// Interpret one JSON line.
    fn feed(&mut self, line: &str) -> Result<Vec<Self::Out>>;

    /// True once the terminal item was produced; remaining input is ignored.
    fn is_done(&self) -> bool;

    /// Body ended without a terminal line.
    fn finish_eof(&mut self) -> Result<Vec<Self::Out>>;
}

struct State<S, P: LineParser> {
    src: S,
    lines: LineDecoder,
    parser: P,
    queue: VecDeque<P::Out>,
    pending_err: Option<LlmError>,
    cancel: CancellationToken,
    idle: Duration,
    ended: bool,
}

impl<S, P: LineParser> State<S, P> {
    fn feed_all(&mut self, lines: Vec<String>) {
        for l in lines {
            match self.parser.feed(&l) {
                Ok(evs) => self.queue.extend(evs),
                Err(e) => {
                    self.pending_err = Some(e);
                    self.ended = true;
                    return;
                }
            }
            if self.parser.is_done() {
                self.ended = true;
                return;
            }
        }
    }
}

/// Drive `body` through `parser`. Errors are yielded in-band as the last
/// item; `idle` bounds the wait for the next network chunk.
pub(super) fn decode<S, E, P>(
    body: S,
    parser: P,
    cancel: CancellationToken,
    idle: Duration,
) -> BoxStream<'static, Result<P::Out>>
where
    S: Stream<Item = std::result::Result<Bytes, E>> + Send + Unpin + 'static,
    E: Display + Send + 'static,
    P: LineParser,
{
    let st = State {
        src: body,
        lines: LineDecoder::default(),
        parser,
        queue: VecDeque::new(),
        pending_err: None,
        cancel,
        idle,
        ended: false,
    };
    stream::unfold(st, |mut st| async move {
        loop {
            if let Some(ev) = st.queue.pop_front() {
                return Some((Ok(ev), st));
            }
            if let Some(e) = st.pending_err.take() {
                return Some((Err(e), st));
            }
            if st.ended {
                return None;
            }
            let next = tokio::select! {
                biased;
                () = st.cancel.cancelled() => {
                    st.ended = true;
                    st.pending_err = Some(LlmError::Cancelled);
                    continue;
                }
                r = tokio::time::timeout(st.idle, st.src.next()) => r,
            };
            match next {
                Err(_) => {
                    st.ended = true;
                    st.pending_err = Some(LlmError::Network("stream idle timeout".into()));
                }
                Ok(Some(Ok(bytes))) => {
                    let lines = st.lines.push(&bytes);
                    st.feed_all(lines);
                }
                Ok(Some(Err(e))) => {
                    st.ended = true;
                    st.pending_err = Some(LlmError::Network(e.to_string()));
                }
                Ok(None) => {
                    let lines = st.lines.finish();
                    st.feed_all(lines);
                    if !st.ended {
                        st.ended = true;
                        match st.parser.finish_eof() {
                            Ok(evs) => st.queue.extend(evs),
                            Err(e) => st.pending_err = Some(e),
                        }
                    }
                }
            }
        }
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_split_across_chunks_and_unterminated_tail() {
        let mut d = LineDecoder::default();
        let s = "{\"a\":\"dünya\"}\n{\"b\":1}";
        let b = s.as_bytes();
        let cut = s.find('ü').unwrap() + 1; // inside the 2-byte char
        assert!(d.push(&b[..cut]).is_empty());
        assert_eq!(d.push(&b[cut..]), vec!["{\"a\":\"dünya\"}"]);
        assert_eq!(d.finish(), vec!["{\"b\":1}"]);
    }
}
