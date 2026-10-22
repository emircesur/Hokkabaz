//! In-memory stand-in for `concat --mcp` (test only). Speaks the same newline-JSON-RPC MCP over a
//! duplex stream and models the subset of Concat's edit semantics the bridge relies on, taken from
//! concat-project `commands/mod.rs`. It proves the bridge's sequencing against OUR reading of that
//! contract; it is not a substitute for running the real binary.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

#[derive(Clone, Default)]
pub struct FakeOpts {
    /// Writes (project location, export output, frame output) must be under one of these.
    pub roots: Vec<PathBuf>,
    /// Never answer `tools/call` (timeout test).
    pub hang: bool,
}

#[derive(Clone, Debug)]
struct Clip {
    id: String,
    track: String,
    kind: &'static str,
    media: String,
    start: f64,
    duration: f64,
    source_start: f64,
    scale: f64,
    offset_x: f64,
    offset_y: f64,
    opacity: f64,
    text: Option<Value>,
    keys: Vec<(String, f64, f64)>,
}

#[derive(Clone, Debug)]
struct Media {
    id: String,
    path: String,
    duration: f64,
}

#[derive(Clone, Debug, Default)]
struct Project {
    name: String,
    width: u32,
    height: u32,
    tracks: Vec<String>, // top to bottom
    clips: Vec<Clip>,
    media: Vec<Media>,
    n_clip: u32,
    n_media: u32,
    n_track: u32,
}

#[derive(Default)]
pub struct State {
    projects: BTreeMap<String, Project>,
    jobs: u32,
    status_calls: HashMap<String, u32>,
    /// Every tools/call seen, for assertions.
    log: Vec<(String, Value)>,
}

pub type Log = Arc<Mutex<State>>;

fn media_len(path: &str) -> f64 {
    if path.contains("broll") { 8.0 } else { 100.0 }
}

/// Spawns the fake on one end of a duplex pipe; returns the client end and a handle for asserts.
pub fn spawn(opts: FakeOpts) -> (DuplexStream, Log) {
    let (client, server) = tokio::io::duplex(1 << 20);
    let state: Log = Arc::default();
    let st = state.clone();
    tokio::spawn(async move {
        let (r, mut w) = tokio::io::split(server);
        let mut lines = BufReader::new(r).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let (Some(id), Some(method)) = (
                msg.get("id").cloned(),
                msg.get("method").and_then(Value::as_str),
            ) else {
                continue; // notification
            };
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            let reply = match method {
                "initialize" => Some(json!({
                    "protocolVersion": params["protocolVersion"],
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "concat", "version": "0.2.8-fake" },
                })),
                "tools/call" if opts.hang => None,
                "tools/call" => Some(call(&st, &opts, &params)),
                _ => None,
            };
            let Some(result) = reply else { continue };
            let mut out = json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
            out.push('\n');
            if w.write_all(out.as_bytes()).await.is_err() {
                break;
            }
        }
    });
    (client, state)
}

pub fn calls(state: &Log) -> Vec<(String, Value)> {
    state.lock().unwrap().log.clone()
}

fn ok(v: &Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": serde_json::to_string_pretty(v).unwrap() }],
        "isError": false,
        "structuredContent": if v.is_object() { v.clone() } else { Value::Null },
    })
    .as_object()
    .map(|m| {
        let mut m = m.clone();
        if m["structuredContent"].is_null() {
            m.remove("structuredContent");
        }
        Value::Object(m)
    })
    .unwrap()
}

fn err(code: &str, msg: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": format!("{msg} (code: {code})") }], "isError": true })
}

fn call(state: &Log, opts: &FakeOpts, params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or("").to_owned();
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let mut st = state.lock().unwrap();
    st.log.push((name.clone(), args.clone()));
    match run(&mut st, opts, &name, &args) {
        Ok(v) => ok(&v),
        Err((code, msg)) => err(code, &msg),
    }
}

type Res = Result<Value, (&'static str, String)>;

fn under_roots(opts: &FakeOpts, path: &str) -> Result<(), (&'static str, String)> {
    if opts.roots.is_empty()
        || opts
            .roots
            .iter()
            .any(|r| std::path::Path::new(path).starts_with(r))
    {
        return Ok(());
    }
    let roots = opts
        .roots
        .iter()
        .map(|r| r.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err((
        "refused",
        format!("{path} is outside where this API writes ({roots})"),
    ))
}

fn proj<'a>(st: &'a mut State, args: &Value) -> Result<&'a mut Project, (&'static str, String)> {
    let p = args["path"].as_str().unwrap_or("");
    st.projects
        .get_mut(p)
        .ok_or(("notOpen", format!("{p} is not open")))
}

fn run(st: &mut State, opts: &FakeOpts, name: &str, a: &Value) -> Res {
    match name {
        "concat_version" => Ok(json!({
            "apiVersion": "0.2", "concat": "0.2.8-fake", "capabilities": ["events", "json-rpc"],
            "mcp": { "mode": "headless", "writesUnder": opts.roots },
        })),
        "project_list" => Ok(Value::Array(
            st.projects
                .iter()
                .map(|(p, pr)| json!({ "path": p, "name": pr.name, "width": pr.width, "height": pr.height, "rateNum": 30, "rateDen": 1, "openedAt": 0 }))
                .collect(),
        )),
        "project_create" => {
            let loc = a["location"].as_str().unwrap_or("");
            under_roots(opts, loc)?;
            let path = format!("{loc}/{}", a["name"].as_str().unwrap_or(""));
            if st.projects.contains_key(&path) {
                return Err(("refused", "folder already holds a project".into()));
            }
            let v = &a["video"];
            let p = Project {
                name: a["name"].as_str().unwrap_or("").into(),
                width: v["width"].as_u64().unwrap_or(1920) as u32,
                height: v["height"].as_u64().unwrap_or(1080) as u32,
                tracks: vec!["t1".into()],
                n_track: 1,
                ..Project::default()
            };
            let s = summary(&p, None);
            st.projects.insert(path, p);
            Ok(s)
        }
        "project_open" => {
            let p = proj(st, a)?;
            Ok(summary(p, None))
        }
        "project_save" | "project_close" => {
            proj(st, a)?;
            Ok(json!({}))
        }
        "timeline_read" => Ok(summary(proj(st, a)?, None)),
        "project_get" => Ok(raw_view(proj(st, a)?)),
        "media_import" => {
            let file = a["file"].as_str().unwrap_or("").to_owned();
            let p = proj(st, a)?;
            if let Some(m) = p.media.iter().find(|m| m.path == file) {
                let _ = m;
                return Ok(summary(p, None));
            }
            p.n_media += 1;
            let id = format!("m{}", p.n_media);
            p.media.push(Media { id: id.clone(), path: file.clone(), duration: media_len(&file) });
            Ok(summary(p, Some(id)))
        }
        "edit_undo" | "edit_redo" => Ok(summary(proj(st, a)?, None)),
        "captions_add" => {
            let p = proj(st, a)?;
            let mut created = None;
            let style = a.get("style").cloned().unwrap_or(json!({}));
            let mut next = p.clone();
            for c in a["captions"].as_array().cloned().unwrap_or_default() {
                let mut s = style.clone();
                s["content"] = c["text"].clone();
                let op = json!({
                    "op": "addTextClip", "trackId": null, "above": true, "start": c["start"],
                    "duration": c["end"].as_f64().unwrap() - c["start"].as_f64().unwrap(),
                    "style": s, "offsetY": a.get("offsetY").cloned().unwrap_or(json!(0.35)),
                });
                created = apply(&mut next, &op)?;
            }
            *p = next;
            Ok(summary(p, created))
        }
        "export_start" => {
            under_roots(opts, a["output"].as_str().unwrap_or(""))?;
            proj(st, a)?;
            st.jobs += 1;
            let job = format!("j{}", st.jobs);
            Ok(json!({ "job": job, "path": a["path"], "output": a["output"] }))
        }
        "export_status" => {
            let job = a["job"].as_str().unwrap_or("").to_owned();
            let n = st.status_calls.entry(job.clone()).or_default();
            *n += 1;
            Ok(if *n < 3 {
                json!({ "job": job, "path": "/p", "output": "/o.mp4", "state": "running", "stage": "video", "frame": *n * 10, "total": 30, "fraction": f64::from(*n) / 3.0 })
            } else {
                json!({ "job": job, "path": "/p", "output": "/o.mp4", "state": "done", "frame": 30, "total": 30, "fraction": 1.0, "width": 1080, "height": 1920 })
            })
        }
        "export_cancel" => Ok(json!({})),
        "preview_frame" => {
            under_roots(opts, a["output"].as_str().unwrap_or(""))?;
            proj(st, a)?;
            Ok(json!({ "path": a["output"], "width": a["width"].as_u64().unwrap_or(1280), "height": a["height"].as_u64().unwrap_or(720) }))
        }
        "effects_list" => Ok(json!([{ "id": "concat.warm", "name": "Warm", "kind": "filter" }])),
        other => {
            let op = tool_to_op(other, a).ok_or(("invalid", format!("unknown tool {other}")))?;
            let p = proj(st, a)?;
            let mut next = p.clone();
            let created = apply(&mut next, &op)?;
            *p = next;
            Ok(summary(p, created))
        }
    }
}

fn tool_to_op(tool: &str, a: &Value) -> Option<Value> {
    let op = match tool {
        "edit_apply" => return Some(a["command"].clone()),
        "clip_add" => "addClip",
        "clip_trim" => "trimClip",
        "clips_split" => "splitClips",
        "clips_move" => "moveClips",
        "clips_remove" => "removeClips",
        "clip_update" => "updateClip",
        "clip_transform" => "setClipTransform",
        "clip_frame" => "frameClip",
        "clip_speed" => "setClipSpeed",
        "title_add" => "addTextClip",
        "keyframe_set" => "setClipKey",
        "keyframe_clear" => "clearClipKey",
        "effect_layer_add" => "addLayerClip",
        "track_add" => "addTrack",
        _ => return None,
    };
    let mut m: Map<String, Value> = a.as_object()?.clone();
    m.remove("path");
    m.insert("op".into(), op.into());
    Some(Value::Object(m))
}

fn f(v: &Value, k: &str) -> f64 {
    v[k].as_f64().unwrap_or(0.0)
}

fn mint(p: &mut Project) -> String {
    p.n_clip += 1;
    format!("c{}", p.n_clip)
}

fn apply(p: &mut Project, op: &Value) -> Result<Option<String>, (&'static str, String)> {
    let kind = op["op"].as_str().unwrap_or("");
    let mut created = None;
    match kind {
        "batch" => {
            let mut next = p.clone();
            for c in op["commands"].as_array().cloned().unwrap_or_default() {
                if let Some(id) = apply(&mut next, &c)? {
                    created = Some(id);
                }
            }
            *p = next;
        }
        "addTrack" => {
            p.n_track += 1;
            let id = format!("t{}", p.n_track);
            if op["bottom"].as_bool().unwrap_or(false) {
                p.tracks.push(id.clone());
            } else {
                p.tracks.insert(0, id.clone());
            }
            created = Some(id);
        }
        "addClip" => {
            let mid = op["mediaId"].as_str().unwrap_or("");
            let m = p
                .media
                .iter()
                .find(|m| m.id == mid)
                .ok_or(("refused", "media is gone".to_owned()))?
                .clone();
            let t = op["trackId"].as_str().unwrap_or("");
            if !p.tracks.iter().any(|x| x == t) {
                return Err(("refused", "track is gone".into()));
            }
            let id = mint(p);
            p.clips.push(Clip {
                id: id.clone(),
                track: t.into(),
                kind: "video",
                media: m.id,
                start: f(op, "start").max(0.0),
                duration: m.duration,
                source_start: 0.0,
                scale: 1.0,
                offset_x: 0.0,
                offset_y: 0.0,
                opacity: 1.0,
                text: None,
                keys: vec![],
            });
            created = Some(id);
        }
        "trimClip" => {
            let id = op["clipId"].as_str().unwrap_or("");
            let delta = f(op, "delta");
            let media_len: HashMap<String, f64> =
                p.media.iter().map(|m| (m.id.clone(), m.duration)).collect();
            if let Some(c) = p.clips.iter_mut().find(|c| c.id == id) {
                if op["edge"] == "start" {
                    c.start += delta;
                    c.source_start += delta;
                    c.duration -= delta;
                } else {
                    c.duration += delta;
                    if let Some(l) = media_len.get(&c.media) {
                        c.duration = c.duration.min(l - c.source_start);
                    }
                }
                c.duration = c.duration.max(1.0 / 60.0);
            }
        }
        "moveClips" => {
            for m in op["moves"].as_array().cloned().unwrap_or_default() {
                let id = m["clipId"].as_str().unwrap_or("");
                if let Some(c) = p.clips.iter_mut().find(|c| c.id == id) {
                    c.start = f(&m, "start").max(0.0);
                    if let Some(t) = m["trackId"]
                        .as_str()
                        .filter(|t| p.tracks.iter().any(|x| x == t))
                    {
                        c.track = t.into();
                    }
                }
            }
        }
        "splitClips" => {
            let time = f(op, "time");
            let ids: Vec<String> = op["clipIds"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
            for id in ids {
                let Some(i) = p.clips.iter().position(|c| c.id == id) else {
                    continue;
                };
                let c = p.clips[i].clone();
                if time > c.start + 1.0 / 60.0 && time < c.start + c.duration - 1.0 / 60.0 {
                    let cut = time - c.start;
                    let new_id = mint(p);
                    let mut tail = c.clone();
                    tail.id = new_id;
                    tail.start = time;
                    tail.duration = c.duration - cut;
                    tail.source_start = c.source_start + cut;
                    p.clips[i].duration = cut;
                    p.clips.push(tail);
                }
            }
        }
        "removeClips" => {
            let ids: Vec<String> = op["clipIds"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
            let removed: Vec<Clip> = p
                .clips
                .iter()
                .filter(|c| ids.contains(&c.id))
                .cloned()
                .collect();
            p.clips.retain(|c| !ids.contains(&c.id));
            if op["ripple"].as_bool().unwrap_or(false) {
                for c in &mut p.clips {
                    let shift: f64 = removed
                        .iter()
                        .filter(|r| r.track == c.track && r.start + r.duration <= c.start + 1e-6)
                        .map(|r| r.duration)
                        .sum();
                    c.start -= shift;
                }
            }
        }
        "addTextClip" => {
            let start = f(op, "start");
            let dur = op["duration"].as_f64().unwrap_or(4.0);
            let free = |p: &Project, t: &str| {
                !p.clips
                    .iter()
                    .any(|c| c.track == t && c.start < start + dur && start < c.start + c.duration)
            };
            let has_video =
                |p: &Project, t: &str| p.clips.iter().any(|c| c.track == t && c.kind == "video");
            let track = match op["trackId"].as_str() {
                Some(t) => t.to_owned(),
                None => match p
                    .tracks
                    .iter()
                    .find(|t| !has_video(p, t) && free(p, t))
                    .cloned()
                {
                    Some(t) => t,
                    None => {
                        p.n_track += 1;
                        let id = format!("t{}", p.n_track);
                        p.tracks.insert(0, id.clone());
                        id
                    }
                },
            };
            let id = mint(p);
            p.clips.push(Clip {
                id: id.clone(),
                track,
                kind: "text",
                media: String::new(),
                start,
                duration: dur,
                source_start: 0.0,
                scale: 1.0,
                offset_x: 0.0,
                offset_y: op["offsetY"].as_f64().unwrap_or(0.0),
                opacity: 1.0,
                text: Some(op["style"].clone()),
                keys: vec![],
            });
            created = Some(id);
        }
        "setClipKey" => {
            let id = op["clipId"].as_str().unwrap_or("");
            let (prop, at, value) = (
                op["property"].as_str().unwrap_or("").to_owned(),
                f(op, "at"),
                f(op, "value"),
            );
            if let Some(c) = p.clips.iter_mut().find(|c| c.id == id) {
                c.keys
                    .retain(|k| !(k.0 == prop && (k.1 - at).abs() < 0.002));
                c.keys.push((prop, at, value));
            }
        }
        "updateClip" => {
            let id = op["clipId"].as_str().unwrap_or("");
            if let Some(c) = p.clips.iter_mut().find(|c| c.id == id) {
                if let Some(o) = op["patch"]["opacity"].as_f64() {
                    c.opacity = o;
                }
            }
        }
        "setClipTransform" => {
            let id = op["clipId"].as_str().unwrap_or("");
            if let Some(c) = p.clips.iter_mut().find(|c| c.id == id) {
                if let Some(v) = op["scale"].as_f64() {
                    c.scale = v;
                }
                if let Some(v) = op["offsetX"].as_f64() {
                    c.offset_x = v;
                }
                if let Some(v) = op["offsetY"].as_f64() {
                    c.offset_y = v;
                }
            }
        }
        "frameClip" => {
            let id = op["clipId"].as_str().unwrap_or("");
            let (w, h) = (f64::from(p.width), f64::from(p.height));
            if let Some(c) = p.clips.iter_mut().find(|c| c.id == id) {
                let (sw, sh) = (1920.0, 1080.0);
                let fit = (w / sw).min(h / sh);
                c.scale = (w / (sw * fit)).max(h / (sh * fit));
            }
        }
        other => return Err(("invalid", format!("fake does not model `{other}`"))),
    }
    Ok(created)
}

fn track_json(p: &Project) -> Vec<Value> {
    p.tracks
        .iter()
        .map(|t| json!({ "id": t, "visible": true, "muted": false }))
        .collect()
}

fn clip_raw(c: &Clip) -> Value {
    let mut v = json!({
        "id": c.id, "trackId": c.track, "mediaId": c.media, "kind": c.kind, "name": "",
        "start": c.start, "duration": c.duration, "sourceStart": c.source_start, "speed": 1.0,
        "scale": c.scale, "offsetX": c.offset_x, "offsetY": c.offset_y, "opacity": c.opacity,
        "keys": c.keys.iter().map(|k| json!({ "property": k.0, "at": k.1, "value": k.2 })).collect::<Vec<_>>(),
    });
    if let Some(t) = &c.text {
        v["text"] = t.clone();
    }
    v
}

fn media_raw(p: &Project) -> Vec<Value> {
    p.media
        .iter()
        .map(|m| json!({ "id": m.id, "path": m.path, "name": m.path, "kind": "video", "duration": m.duration, "width": 1920, "height": 1080, "hasAudio": true }))
        .collect()
}

fn raw_view(p: &Project) -> Value {
    json!({
        "project": {
            "activeTimelineId": "tl1",
            "timelines": [{ "id": "tl1", "name": "Main", "tracks": track_json(p), "clips": p.clips.iter().map(clip_raw).collect::<Vec<_>>() }],
            "media": media_raw(p),
        },
        "canUndo": true, "canRedo": false,
        "settings": { "name": p.name, "width": p.width, "height": p.height, "rateNum": 30, "rateDen": 1 },
    })
}

/// Same shape as concat-mcp `summary::view`.
fn summary(p: &Project, created: Option<String>) -> Value {
    let mut clips: Vec<&Clip> = p.clips.iter().collect();
    clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    let dur = clips
        .iter()
        .map(|c| c.start + c.duration)
        .fold(0.0, f64::max);
    let clip_json: Vec<Value> = clips
        .iter()
        .map(|c| {
            let mut v = json!({ "id": c.id, "kind": c.kind, "track": c.track, "start": c.start, "duration": c.duration, "end": c.start + c.duration });
            if !c.media.is_empty() {
                v["media"] = json!(c.media);
                v["sourceStart"] = json!(c.source_start);
            }
            if let Some(t) = &c.text {
                v["text"] = t["content"].clone();
            }
            if c.offset_y != 0.0 {
                v["offsetY"] = json!(c.offset_y);
            }
            if !c.keys.is_empty() {
                v["keyframed"] = json!(c.keys.iter().map(|k| k.0.clone()).collect::<std::collections::BTreeSet<_>>());
            }
            v
        })
        .collect();
    let mut s = json!({
        "project": { "name": p.name, "width": p.width, "height": p.height, "fps": 30.0 },
        "canUndo": true, "canRedo": false,
        "timeline": {
            "id": "tl1", "name": "Main", "duration": dur,
            "tracks": p.tracks.iter().enumerate().map(|(i, t)| json!({ "id": t, "index": i })).collect::<Vec<_>>(),
            "clips": clip_json,
        },
        "media": media_raw(p),
    });
    if let Some(c) = created {
        s["createdId"] = json!(c);
    }
    s
}
