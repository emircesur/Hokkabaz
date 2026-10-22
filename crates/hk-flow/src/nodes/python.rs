//! Python mode of the Code node: runs the user's code in a `python3` subprocess (desktop only).
//!
//! Unlike the JavaScript mode this is NOT sandboxed beyond the process boundary (it has the
//! user's file and network access), so it is off unless the host opts in
//! ([`NodeHooks::allow_python`](crate::hooks::NodeHooks) or `HOKKABAZ_ALLOW_PYTHON=1`).
//! Items go in as JSON on stdin and the result comes back as JSON on stdout; the script sees
//! `_items` (all items mode) or `_item` (each item mode), `_input.all()/first()/last()`, and
//! returns a list of `{"json": {...}}` (all items) or one `{"json": {...}}` (each item).

use std::time::Duration;

use serde_json::{Value, json};

const LIMIT: Duration = Duration::from_secs(60);
const MAX_OUT: usize = 32 * 1024 * 1024;

/// Reads `{"mode","items"}` from stdin; user code is indented into `__hk_main`.
const HARNESS: &str = r#"
import sys, json
__hk_req = json.loads(sys.stdin.buffer.read().decode("utf-8"))
_items = __hk_req["items"]
class _Input:
    def __init__(self, items): self._i = items
    def all(self): return self._i
    def first(self): return self._i[0] if self._i else None
    def last(self): return self._i[-1] if self._i else None
_input = _Input(_items)
_item = _items[0] if _items else None
def __hk_main():
@@CODE@@
    return None
if __hk_req["mode"] == "each":
    __out = []
    for __idx, __it in enumerate(_items):
        _item = __it
        _input = _Input(_items)
        __out.append(__hk_main())
else:
    __out = __hk_main()
sys.stdout.write("\n@@HK-RESULT@@" + json.dumps(__out, default=str))
"#;

fn script(code: &str) -> String {
    let body: String = if code.trim().is_empty() {
        "    pass".to_owned()
    } else {
        code.lines()
            .map(|l| format!("    {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    HARNESS.replace("@@CODE@@", &body)
}

pub(crate) fn allowed(hook: bool) -> bool {
    hook || std::env::var_os("HOKKABAZ_ALLOW_PYTHON").is_some_and(|v| v == "1")
}

/// Run `code`; `each` selects per-item mode. Returns the script's JSON result.
#[cfg(not(target_os = "android"))]
pub(crate) async fn run(code: &str, each: bool, items: Vec<Value>) -> Result<Value, String> {
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;

    let program = if cfg!(windows) { "python" } else { "python3" };
    let mut child = Command::new(program)
        .args(["-I", "-c", &script(code)])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            format!("cannot start {program}: {e}. Install Python 3 to use Python in the Code node")
        })?;
    let req = json!({"mode": if each { "each" } else { "all" }, "items": items}).to_string();
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let write = tokio::spawn(async move {
        let _ = stdin.write_all(req.as_bytes()).await;
    });
    let out = tokio::time::timeout(LIMIT, child.wait_with_output())
        .await
        .map_err(|_| "Python code timed out after 60s".to_owned())?
        .map_err(|e| e.to_string())?;
    let _ = write.await;
    if out.stdout.len() > MAX_OUT {
        return Err("Python output is too large".into());
    }
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        // The traceback's last line is the useful one (`NameError: ...`).
        let last = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("Python exited with an error");
        return Err(last.trim().to_owned());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (_, result) = text
        .rsplit_once("@@HK-RESULT@@")
        .ok_or("Python code did not return a result")?;
    serde_json::from_str(result).map_err(|e| format!("Python result is not JSON: {e}"))
}

#[cfg(target_os = "android")]
pub(crate) async fn run(_code: &str, _each: bool, _items: Vec<Value>) -> Result<Value, String> {
    Err("Python is not available on Android. Use JavaScript, or run this flow on a desktop/server instance.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn python_available() -> bool {
        std::process::Command::new(if cfg!(windows) { "python" } else { "python3" })
            .arg("--version")
            .output()
            .is_ok()
    }

    #[tokio::test]
    async fn all_items_and_each_item_modes() {
        if !python_available() {
            return;
        }
        let items = vec![json!({"json": {"n": 1}}), json!({"json": {"n": 2}})];
        let all = run(
            "return [{'json': {'sum': sum(i['json']['n'] for i in _items)}}]",
            false,
            items.clone(),
        )
        .await
        .unwrap();
        assert_eq!(all, json!([{"json": {"sum": 3}}]));
        let each = run(
            "_item['json']['double'] = _item['json']['n'] * 2\nreturn _item",
            true,
            items,
        )
        .await
        .unwrap();
        assert_eq!(
            each,
            json!([{"json": {"n": 1, "double": 2}}, {"json": {"n": 2, "double": 4}}])
        );
    }

    #[tokio::test]
    async fn errors_report_the_last_traceback_line() {
        if !python_available() {
            return;
        }
        let e = run("return undefined_name", false, vec![])
            .await
            .unwrap_err();
        assert!(e.contains("NameError"), "{e}");
    }
}
