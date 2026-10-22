//! Spawn a render, stream `-progress pipe:1` output, cancel, keep stderr tail for errors.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

use crate::error::{MediaError, Result};
use crate::graph::RenderSpec;
use crate::tools::Tools;

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub frame: u64,
    /// Seconds of output written so far.
    pub out_time: f64,
    /// Encode speed multiple (1.0 = realtime); 0 until known.
    pub speed: f64,
    /// 0..=1 against the spec's expected duration.
    pub fraction: f64,
    pub finished: bool,
}

/// Incremental parser for ffmpeg's `key=value` progress blocks (terminated by `progress=`).
#[derive(Debug)]
pub(crate) struct ProgressParser {
    total: f64,
    cur: Progress,
}

impl ProgressParser {
    pub(crate) fn new(total: f64) -> Self {
        Self {
            total,
            cur: Progress::default(),
        }
    }

    /// Feed one line; returns a snapshot at the end of each block.
    pub(crate) fn feed(&mut self, line: &str) -> Option<Progress> {
        let (k, v) = line.trim().split_once('=')?;
        let v = v.trim();
        match k {
            "frame" => self.cur.frame = v.parse().unwrap_or(self.cur.frame),
            // Despite the name, out_time_ms is microseconds too; prefer _us.
            "out_time_us" | "out_time_ms" => {
                if let Ok(us) = v.parse::<i64>() {
                    self.cur.out_time = (us.max(0) as f64) / 1_000_000.0;
                }
            }
            "speed" => {
                if let Ok(x) = v.trim_end_matches('x').trim().parse() {
                    self.cur.speed = x;
                }
            }
            "progress" => {
                self.cur.finished = v == "end";
                self.cur.fraction = if self.cur.finished || self.total <= 0.0 {
                    1.0f64.min(if self.cur.finished { 1.0 } else { 0.0 })
                } else {
                    (self.cur.out_time / self.total).clamp(0.0, 1.0)
                };
                return Some(self.cur);
            }
            _ => {}
        }
        None
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenderReport {
    pub output: PathBuf,
    pub expected_duration: f64,
}

/// Running render. Dropping the handle cancels the render (ffmpeg is killed).
#[derive(Debug)]
pub struct RenderHandle {
    pub(crate) progress: watch::Receiver<Progress>,
    pub(crate) join: JoinHandle<Result<RenderReport>>,
    pub(crate) cancel: Option<oneshot::Sender<()>>,
}

impl RenderHandle {
    /// Latest progress; `changed().await` on it to follow updates.
    pub fn progress(&self) -> watch::Receiver<Progress> {
        self.progress.clone()
    }

    /// Ask the render to stop; `wait` then returns `Cancelled`.
    pub fn cancel(&mut self) {
        if let Some(tx) = self.cancel.take() {
            let _ = tx.send(());
        }
    }

    pub async fn wait(self) -> Result<RenderReport> {
        // Keep the sender alive until the task ends; dropping it would cancel.
        let Self { join, cancel: _keep, .. } = self;
        join.await
            .map_err(|e| MediaError::Io(std::io::Error::other(e.to_string())))?
    }
}

/// Start a render with `HK_FFMPEG` / PATH defaults.
pub async fn render(spec: RenderSpec) -> Result<RenderHandle> {
    Tools::from_env().render(spec).await
}

impl Tools {
    /// Validate + build the graph, then spawn ffmpeg. Errors in the spec surface here, before spawn.
    pub async fn render(&self, spec: RenderSpec) -> Result<RenderHandle> {
        let built = spec.build()?;
        spawn(self, built.args, built.duration, spec.output)
    }
}

#[cfg(not(target_os = "android"))]
fn spawn(tools: &Tools, args: Vec<String>, total: f64, output: PathBuf) -> Result<RenderHandle> {
    use std::collections::VecDeque;
    use std::process::Stdio;
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncBufReadExt, BufReader};

    const TAIL: usize = 40;
    let bin = tools.ffmpeg.clone();
    tracing::debug!(bin = %bin.display(), "spawn render");
    let mut child = tokio::process::Command::new(&bin)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| crate::proc::spawn_err(&bin, e))?;
    let stdout = child.stdout.take().ok_or_else(|| MediaError::Parse("no stdout pipe".into()))?;
    let stderr = child.stderr.take().ok_or_else(|| MediaError::Parse("no stderr pipe".into()))?;

    let (tx, rx) = watch::channel(Progress::default());
    let out_task = tokio::spawn(async move {
        let mut parser = ProgressParser::new(total);
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(p) = parser.feed(&line) {
                tx.send_replace(p);
            }
        }
        tx
    });
    let tail: Arc<Mutex<VecDeque<String>>> = Arc::default();
    let tail_w = Arc::clone(&tail);
    let err_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let mut t = tail_w.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if t.len() == TAIL {
                t.pop_front();
            }
            t.push_back(line);
        }
    });

    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
    let join = tokio::spawn(async move {
        // Both an explicit cancel and a dropped handle (sender gone) end up here.
        let outcome = tokio::select! {
            status = child.wait() => Some(status?),
            _ = cancel_rx => None,
        };
        let status = match outcome {
            Some(s) => s,
            None => {
                let _ = child.kill().await;
                let _ = out_task.await;
                let _ = err_task.await;
                return Err(MediaError::Cancelled);
            }
        };
        let tx = out_task.await.ok();
        let _ = err_task.await;
        if !status.success() {
            let tail = tail.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            return Err(MediaError::Failed {
                bin: bin.display().to_string(),
                code: status.code(),
                stderr_tail: tail.iter().cloned().collect::<Vec<_>>().join("\n"),
            });
        }
        if let Some(tx) = tx {
            tx.send_modify(|p| {
                p.fraction = 1.0;
                p.finished = true;
            });
        }
        Ok(RenderReport {
            output,
            expected_duration: total,
        })
    });

    Ok(RenderHandle {
        progress: rx,
        join,
        cancel: Some(cancel_tx),
    })
}

#[cfg(target_os = "android")]
fn spawn(_tools: &Tools, _args: Vec<String>, _total: f64, _output: PathBuf) -> Result<RenderHandle> {
    Err(MediaError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_blocks() {
        let mut p = ProgressParser::new(10.0);
        let block = "frame=50\nfps=25.0\nout_time_us=2000000\nout_time=00:00:02.000000\nspeed=2.5x\nprogress=continue\n";
        let snaps: Vec<Progress> = block.lines().filter_map(|l| p.feed(l)).collect();
        assert_eq!(snaps.len(), 1);
        let s = snaps[0];
        assert_eq!((s.frame, s.out_time, s.speed), (50, 2.0, 2.5));
        assert!((s.fraction - 0.2).abs() < 1e-9);
        assert!(!s.finished);
        let end = ["frame=250", "out_time_us=10040000", "speed=N/A", "progress=end"]
            .iter()
            .filter_map(|l| p.feed(l))
            .last()
            .unwrap();
        assert!(end.finished);
        assert_eq!(end.fraction, 1.0);
        assert_eq!(end.speed, 2.5, "N/A keeps previous speed");
    }

    #[test]
    fn unknown_total_gives_zero_until_end() {
        let mut p = ProgressParser::new(0.0);
        let s = ["out_time_us=1000000", "progress=continue"].iter().filter_map(|l| p.feed(l)).last().unwrap();
        assert_eq!(s.fraction, 0.0);
    }
}
