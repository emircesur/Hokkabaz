//! Scripted ACP agent over stdio, used by `tests/spawn.rs`. Per prompt: message chunk, tool
//! call, permission request, outcome chunk, tool update, `end_turn`.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn send(v: &Value) {
    let mut o = std::io::stdout().lock();
    let _ = writeln!(o, "{v}");
    let _ = o.flush();
}

fn update(u: Value) {
    send(
        &json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"fake-1","update":u}}),
    );
}

fn chunk(t: &str) {
    update(json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":t}}));
}

fn main() {
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines().map_while(Result::ok);
    let greeting = std::env::var("FAKE_ACP_GREETING").unwrap_or_default();
    while let Some(line) = lines.next() {
        let Ok(m) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = m.get("id").cloned();
        match m["method"].as_str() {
            Some("initialize") => send(&json!({"jsonrpc":"2.0","id":id,"result":
                {"protocolVersion":1,"agentCapabilities":{},"authMethods":[],"agentInfo":{"name":"fake-acp-agent","version":"1"}}})),
            Some("session/new") => {
                send(&json!({"jsonrpc":"2.0","id":id,"result":{"sessionId":"fake-1"}}))
            }
            Some("session/prompt") => {
                let text = m["params"]["prompt"][0]["text"]
                    .as_str()
                    .unwrap_or_default();
                chunk(&format!("{greeting} echo: {text}"));
                update(
                    json!({"sessionUpdate":"tool_call","toolCallId":"t1","title":"Run ls","kind":"execute","status":"pending"}),
                );
                send(
                    &json!({"jsonrpc":"2.0","id":900,"method":"session/request_permission","params":{
                    "sessionId":"fake-1","toolCall":{"toolCallId":"t1","title":"Run ls"},
                    "options":[{"optionId":"ok","name":"Allow","kind":"allow_once"},{"optionId":"no","name":"Reject","kind":"reject_once"}]}}),
                );
                let mut verdict = "no response";
                for l in lines.by_ref() {
                    let Ok(r) = serde_json::from_str::<Value>(&l) else {
                        continue;
                    };
                    if r["id"] == 900 {
                        verdict = if r["result"]["outcome"]["optionId"] == "ok" {
                            "allowed"
                        } else {
                            "denied"
                        };
                        break;
                    }
                }
                chunk(&format!(" [{verdict}]"));
                update(
                    json!({"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed"}),
                );
                send(&json!({"jsonrpc":"2.0","id":id,"result":{"stopReason":"end_turn"}}));
            }
            _ => {}
        }
    }
}
