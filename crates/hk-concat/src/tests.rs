//! Bridge tests against the in-memory fake `concat --mcp` (see `fake.rs`). No Concat binary.

use std::sync::Arc;
use std::time::Duration;

use hk_tools::{Risk, Tool, ToolCtx};
use serde_json::{Value, json};

use crate::fake::{self, FakeOpts};
use crate::plan::*;
use crate::*;

const T: Duration = Duration::from_secs(5);

async fn connect(opts: FakeOpts) -> (ConcatClient, crate::fake::Log) {
    let (stream, log) = fake::spawn(opts);
    let (r, w) = tokio::io::split(stream);
    let backend = ProcessBackend::from_streams(r, w, T).await.expect("handshake");
    (ConcatClient::new(Arc::new(backend)), log)
}

fn tr(a: f64, b: f64) -> TimeRange {
    TimeRange { start: a, end: b }
}

fn word(t: &str, a: f64, b: f64) -> Word {
    Word { text: t.into(), start: a, end: b }
}

fn sample_plan() -> StudioClipPlan {
    let mut plan = StudioClipPlan::new("hook", "/home/u/src.mp4", vec![tr(10.0, 14.0), tr(20.0, 23.0)]);
    plan.captions = Some(CaptionTrack {
        words: vec![
            word("so", 10.2, 10.5),
            word("this", 10.6, 10.9),
            word("is", 11.0, 11.2),
            word("cut", 15.0, 15.4), // inside the removed gap: must vanish
            word("great", 20.5, 21.0),
            word("stuff", 21.1, 21.6),
        ],
        style: CaptionStyle {
            font_family: Some("Inter".into()),
            font_size: Some(0.05),
            font_weight: Some(700.0),
            color: Some("#ffee00".into()),
            offset_y: Some(0.3),
            ..CaptionStyle::default()
        },
    });
    plan.crop_keyframes = vec![
        CropKeyframe { time: 0.0, center_x: 0.3, center_y: 0.5, zoom: 1.0 },
        CropKeyframe { time: 4.0, center_x: 0.7, center_y: 0.5, zoom: 1.2 },
        CropKeyframe { time: 7.0, center_x: 0.5, center_y: 0.5, zoom: 1.0 },
    ];
    plan.overlays = vec![
        Overlay::Title { text: "Wait for it".into(), start: 0.5, duration: 2.0, style: None, offset_y: Some(-0.3) },
        Overlay::Media { file: "/home/u/broll.mp4".into(), start: 3.0, duration: 2.0, scale: 0.4, offset_x: 0.1, offset_y: -0.2, opacity: 0.9 },
    ];
    plan
}

#[tokio::test]
async fn handshake_version_and_health() {
    let (c, log) = connect(FakeOpts::default()).await;
    let v = c.health().await.unwrap();
    assert_eq!(v.concat, "0.2.8-fake");
    assert_eq!(v.mode(), Some("headless"));
    let names: Vec<_> = fake::calls(&log).into_iter().map(|c| c.0).collect();
    assert_eq!(names, ["concat_version", "project_list"]);
}

#[tokio::test]
async fn apply_plan_builds_the_expected_timeline() {
    let (c, _) = connect(FakeOpts::default()).await;
    let plan = sample_plan();
    let applied = apply_plan(&c, "/home/u", &plan).await.unwrap();
    assert_eq!(applied.project, "/home/u/hook");
    let tl = &applied.summary.timeline;
    let main: Vec<_> = tl.clips.iter().filter(|c| c.kind == "video" && c.media.as_deref() == Some("m1")).collect();
    assert_eq!(main.len(), 2);
    // Range 0: source 10..14 at 0; range 1: source 20..23 at 4.
    assert_eq!((main[0].start, main[0].duration, main[0].source_start), (0.0, 4.0, Some(10.0)));
    assert_eq!((main[1].start, main[1].duration, main[1].source_start), (4.0, 3.0, Some(20.0)));
    // Captions: one line per cut side, nothing from the removed gap.
    let texts: Vec<_> = tl.clips.iter().filter(|c| c.kind == "text").filter_map(|c| c.text.as_deref()).collect();
    assert!(texts.contains(&"so this is"), "{texts:?}");
    assert!(texts.contains(&"great stuff"), "{texts:?}");
    assert!(texts.contains(&"Wait for it"));
    assert!(!texts.iter().any(|t| t.contains("cut")));
    // Overlay media sits on its own lane, trimmed to 2 s.
    let broll = tl.clips.iter().find(|c| c.media.as_deref() == Some("m2")).expect("b-roll clip");
    assert_eq!((broll.start, broll.duration), (3.0, 2.0));
    assert_ne!(broll.track, main[0].track);
    // Reframe keys landed on both main clips.
    assert!(main.iter().all(|c| c.keyframed.iter().any(|k| k == "offsetX")));
    assert_eq!(applied.range_clips.len(), 2);
}

#[tokio::test]
async fn plan_round_trips_through_a_project() {
    let (c, _) = connect(FakeOpts::default()).await;
    let plan = sample_plan();
    let applied = apply_plan(&c, "/home/u", &plan).await.unwrap();
    let view = c.project_get(&applied.project).await.unwrap();
    let back = plan_from_view(&view, Some(&plan.source)).unwrap();

    assert_eq!(back.source, plan.source);
    assert_eq!(back.video, plan.video);
    assert_eq!(back.kept_ranges, plan.kept_ranges, "cut list survives exactly");

    // Titles: same text/span/offset; captions come back as words that regroup to the same lines.
    let titles: Vec<_> = back.overlays.iter().filter(|o| matches!(o, Overlay::Title { .. })).collect();
    assert_eq!(titles.len(), 1);
    assert!(matches!(titles[0], Overlay::Title { text, start, duration, offset_y, .. }
        if text == "Wait for it" && *start == 0.5 && *duration == 2.0 && *offset_y == Some(-0.3)));
    let media: Vec<_> = back.overlays.iter().filter_map(|o| match o {
        Overlay::Media { file, start, duration, scale, opacity, .. } => Some((file.clone(), *start, *duration, *scale, *opacity)),
        _ => None,
    }).collect();
    assert_eq!(media, vec![("/home/u/broll.mp4".to_string(), 3.0, 2.0, 0.4, 0.9)]);

    let cap = back.captions.as_ref().expect("captions recovered");
    assert_eq!(cap.style.font_family.as_deref(), Some("Inter"));
    assert_eq!(cap.style.color.as_deref(), Some("#ffee00"));
    assert_eq!(cap.style.offset_y, Some(0.3));
    let ranges = normalize_ranges(&back.kept_ranges).unwrap();
    let lines = group_words(&map_words(&cap.words, &ranges), &cap.style);
    let want = group_words(&map_words(&plan.captions.as_ref().unwrap().words, &ranges), &plan.captions.as_ref().unwrap().style);
    assert_eq!(lines.iter().map(|l| l.text.clone()).collect::<Vec<_>>(), want.iter().map(|l| l.text.clone()).collect::<Vec<_>>());
    for (a, b) in lines.iter().zip(&want) {
        assert!((a.start - b.start).abs() < 0.02 && (a.end - b.end).abs() < 0.02, "{a:?} vs {b:?}");
    }

    // Reframe path: sampled at the original key times it matches (edge keys are extra, not different).
    for k in &plan.crop_keyframes {
        let (cx, cy, z) = sample(&back.crop_keyframes, k.time);
        assert!((cx - k.center_x).abs() < 1e-6 && (cy - k.center_y).abs() < 1e-6 && (z - k.zoom).abs() < 1e-6, "t={}: {cx} {cy} {z}", k.time);
    }
}

fn sample(keys: &[CropKeyframe], t: f64) -> (f64, f64, f64) {
    let i = keys.iter().rposition(|k| k.time <= t + 1e-9).unwrap_or(0);
    let a = keys[i];
    match keys.get(i + 1) {
        Some(b) if b.time > a.time + 1e-9 && t > a.time => {
            let u = (t - a.time) / (b.time - a.time);
            (a.center_x + (b.center_x - a.center_x) * u, a.center_y + (b.center_y - a.center_y) * u, a.zoom + (b.zoom - a.zoom) * u)
        }
        _ => (a.center_x, a.center_y, a.zoom),
    }
}

#[tokio::test]
async fn cut_range_removes_a_span_and_closes_the_gap() {
    let (c, _) = connect(FakeOpts::default()).await;
    c.project_create("/home/u", "cut", None).await.unwrap();
    let p = "/home/u/cut";
    let m = c.import_media(p, "/home/u/src.mp4").await.unwrap();
    let t = c.timeline_read(p).await.unwrap().timeline.tracks[0].id.clone();
    c.add_clip(p, &m, &t, 0.0, false).await.unwrap();
    let s = c.cut_range(p, 10.0, 25.0).await.unwrap();
    let total: f64 = s.timeline.clips.iter().map(|c| c.duration).sum();
    assert!((total - 85.0).abs() < 1e-6, "100 s minus 15 s, got {total}");
    assert!((s.timeline.duration - 85.0).abs() < 1e-6, "gap closed");
    assert!(matches!(c.cut_range(p, 5.0, 5.0).await, Err(ConcatError::InvalidArgs(_))));
}

#[tokio::test]
async fn roots_violation_is_typed_and_actionable() {
    let opts = FakeOpts { roots: vec!["/home/u".into()], ..FakeOpts::default() };
    // Server-side refusal (no local policy): parsed from Concat's own sentence.
    let (c, _) = connect(opts.clone()).await;
    let e = c.project_create("/srv/data", "x", None).await.unwrap_err();
    match &e {
        ConcatError::OutsideRoots { path, roots } => {
            assert_eq!(path, "/srv/data");
            assert_eq!(roots, &["/home/u".to_string()]);
        }
        other => panic!("{other:?}"),
    }
    let msg = e.to_string();
    assert!(msg.contains("CONCAT_MCP_ROOTS") && msg.contains("[concat] roots") && msg.contains("/home/u"), "{msg}");

    // Local pre-check: same error without a round trip.
    let (c, log) = connect(opts).await;
    let c = c.with_roots(Some(RootsPolicy::new(vec!["/home/u".into()])));
    assert!(matches!(c.get_frame("/home/u/p", 1.0, "/tmp/f.png", None).await, Err(ConcatError::OutsideRoots { .. })));
    assert!(matches!(
        c.export_start("/home/u/p", &ExportPreset::Draft.spec("/home/u/../etc/o.mp4")).await,
        Err(ConcatError::OutsideRoots { .. })
    ));
    assert!(fake::calls(&log).is_empty());
}

#[tokio::test]
async fn other_refusals_keep_code_and_sentence() {
    let (c, _) = connect(FakeOpts::default()).await;
    let e = c.timeline_read("/nope").await.unwrap_err();
    assert!(matches!(&e, ConcatError::Tool { code: Some(c), message, .. } if c == "notOpen" && message == "/nope is not open"), "{e:?}");
}

#[tokio::test]
async fn export_polls_progress_to_done_and_get_frame_returns_file() {
    let (c, _) = connect(FakeOpts::default()).await;
    c.project_create("/home/u", "e", None).await.unwrap();
    let job = c.export_start("/home/u/e", &ExportPreset::Vertical1080.spec("/home/u/out.mp4")).await.unwrap();
    assert_eq!(job.job, "j1");
    let mut seen = Vec::new();
    let st = c.export_wait(&job.job, Duration::from_millis(5), T, |s| seen.push(s.overall())).await.unwrap();
    assert_eq!(st.state, ExportState::Done);
    assert!(seen.windows(2).all(|w| w[0] <= w[1]) && seen.last() == Some(&1.0), "{seen:?}");
    let f = c.get_frame("/home/u/e", 1.0, "/home/u/e/f.png", Some((320, 180))).await.unwrap();
    assert_eq!((f.width, f.height, f.path.as_str()), (320, 180, "/home/u/e/f.png"));
    // Deadline path.
    let job2 = c.export_start("/home/u/e", &ExportPreset::Draft.spec("/home/u/o2.mp4")).await.unwrap();
    assert!(matches!(c.export_wait(&job2.job, Duration::from_millis(1), Duration::ZERO, |_| {}).await, Err(ConcatError::Timeout(_))));
}

#[tokio::test]
async fn hung_server_times_out_and_closed_server_reports_closed() {
    let (stream, _) = fake::spawn(FakeOpts { hang: true, ..FakeOpts::default() });
    let (r, w) = tokio::io::split(stream);
    let b = ProcessBackend::from_streams(r, w, Duration::from_millis(50)).await.unwrap();
    let c = ConcatClient::new(Arc::new(b));
    assert!(matches!(c.version().await, Err(ConcatError::Timeout(_))));

    // Server end dropped: handshake itself fails as Closed.
    let (client, server) = tokio::io::duplex(1024);
    drop(server);
    let (r, w) = tokio::io::split(client);
    let e = ProcessBackend::from_streams(r, w, T).await.unwrap_err();
    assert!(e.is_connection_lost(), "{e:?}");
}

#[test]
fn tool_text_parsing() {
    let e = ConcatError::from_tool_text("project_open", "Refused: the Concat window has that project open. (code: refused)");
    assert!(matches!(e, ConcatError::Tool { code: Some(c), .. } if c == "refused"));
    let e = ConcatError::from_tool_text("x", "plain message");
    assert!(matches!(e, ConcatError::Tool { code: None, .. }));
}

#[test]
fn config_debug_hides_token_and_policy_modes() {
    let cfg = ConcatConfig { api_address: Some("127.0.0.1:7420".into()), api_token: Some("sekrit-token".into()), ..ConcatConfig::default() };
    assert!(!format!("{cfg:?}").contains("sekrit-token"));
    assert!(cfg.effective_roots().is_none(), "remote server decides its own roots");
    let cfg = ConcatConfig { roots: vec!["/data".into()], ..ConcatConfig::default() };
    assert_eq!(cfg.effective_roots().unwrap().roots(), &[std::path::PathBuf::from("/data")]);
    let toml_like: ConcatConfig = serde_json::from_value(json!({ "path": "/usr/bin/concat", "mode": "headless" })).unwrap();
    assert_eq!(toml_like.mode, ConcatMode::Headless);
    assert_eq!(toml_like.timeout_secs, 300);
}

// ------------------------------------------------------------------------------ agent tools

fn tools_for(c: ConcatClient) -> Vec<Arc<dyn Tool>> {
    concat_tools(ConcatSession::with_client(c))
}

fn tool<'a>(tools: &'a [Arc<dyn Tool>], name: &str) -> &'a Arc<dyn Tool> {
    tools.iter().find(|t| t.spec().name == name).unwrap_or_else(|| panic!("no tool {name}"))
}

#[test]
fn tool_specs_are_valid_and_risks_gate_writes() {
    let (rt, _g) = rt();
    let (c, _) = rt.block_on(connect(FakeOpts::default()));
    let tools = tools_for(c);
    let mut names: Vec<_> = tools.iter().map(|t| t.spec().name).collect();
    assert_eq!(names.len(), 11);
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 11);
    for t in &tools {
        let s = t.spec();
        assert!(s.name.starts_with("concat_"));
        assert_eq!(s.parameters["type"], "object", "{}", s.name);
        assert!(s.description.len() > 40);
        for r in s.parameters["required"].as_array().unwrap() {
            assert!(s.parameters["properties"].get(r.as_str().unwrap()).is_some(), "{}: required `{r}` has no property", s.name);
        }
    }
    let risk = |n: &str, a: Value| tool(&tools, n).risk(&a);
    assert_eq!(risk("concat_status", json!({})), Risk::Safe);
    assert_eq!(risk("concat_project", json!({"action": "read"})), Risk::Safe);
    assert_eq!(risk("concat_project", json!({"action": "create"})), Risk::Ask);
    assert_eq!(risk("concat_cut", json!({"action": "delete"})), Risk::Safe);
    assert_eq!(risk("concat_export", json!({"action": "start"})), Risk::Ask);
    assert_eq!(risk("concat_export", json!({"action": "status"})), Risk::Safe);
    assert_eq!(risk("concat_raw", json!({"tool": "x"})), Risk::Ask);
    assert_eq!(risk("concat_apply_plan", json!({"dry_run": true})), Risk::Safe);
    assert_eq!(risk("concat_apply_plan", json!({})), Risk::Ask);
}

fn rt() -> (tokio::runtime::Runtime, ()) {
    (tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap(), ())
}

#[tokio::test]
async fn agent_tools_drive_a_project_end_to_end() {
    let (c, _) = connect(FakeOpts { roots: vec!["/home/u".into()], ..FakeOpts::default() }).await;
    let tools = tools_for(c);
    let run = |name: &'static str, args: Value| {
        let t = tool(&tools, name).clone();
        async move { t.execute(ToolCtx::default(), args).await.unwrap() }
    };

    let out = run("concat_status", json!({})).await;
    assert!(!out.is_error && out.content.contains("0.2.8-fake"), "{}", out.content);

    let out = run("concat_project", json!({"action": "create", "location": "/home/u", "name": "t1"})).await;
    assert!(!out.is_error, "{}", out.content);
    let path = "/home/u/t1";

    let out = run("concat_media", json!({"action": "import", "path": path, "file": "/home/u/src.mp4"})).await;
    let media: Value = serde_json::from_str(&out.content).unwrap();
    let mid = media["media_id"].as_str().unwrap().to_owned();

    let read: Value = serde_json::from_str(&run("concat_project", json!({"action": "read", "path": path})).await.content).unwrap();
    let track = read["timeline"]["tracks"][0]["id"].as_str().unwrap().to_owned();
    let out = run("concat_cut", json!({"action": "add_clip", "path": path, "media_id": mid, "track_id": track, "start": 0})).await;
    let clip = serde_json::from_str::<Value>(&out.content).unwrap()["clip_id"].as_str().unwrap().to_owned();

    let out = run("concat_cut", json!({"action": "cut_range", "path": path, "from": 10, "to": 20})).await;
    assert!(!out.is_error, "{}", out.content);

    let out = run("concat_text", json!({"action": "captions_from_words", "path": path, "offset_y": 0.3,
        "words": [{"text": "hello", "start": 0.0, "end": 0.4}, {"text": "there", "start": 0.5, "end": 0.9}]})).await;
    assert!(!out.is_error && out.content.contains("\"added\": 1"), "{}", out.content);

    let out = run("concat_keyframe", json!({"action": "reframe", "path": path, "clip_id": clip,
        "points": [{"at": 0.0, "center_x": 0.3, "center_y": 0.5}, {"at": 1.0, "center_x": 0.7, "center_y": 0.5, "zoom": 1.1}]})).await;
    assert!(!out.is_error && out.content.contains("offsetX"), "{}", out.content);

    // Roots error surfaces as a tool error the model can act on, not a crash.
    let out = run("concat_project", json!({"action": "create", "location": "/srv", "name": "bad"})).await;
    assert!(out.is_error && out.content.contains("CONCAT_MCP_ROOTS"), "{}", out.content);

    // Bad args are an InvalidArgs ToolError, unknown action too.
    let e = tool(&tools, "concat_cut").execute(ToolCtx::default(), json!({"action": "split", "path": path})).await.unwrap_err();
    assert!(matches!(e, hk_tools::ToolError::InvalidArgs { .. }));

    // dry_run works with the plan JSON an agent would write, and needs no editor.
    let out = run("concat_apply_plan", json!({"dry_run": true, "plan": {
        "name": "p", "source": "/home/u/src.mp4", "kept_ranges": [{"start": 1.0, "end": 5.0}]}})).await;
    assert!(!out.is_error && out.content.contains("\"place_range\"") && out.content.contains("4.0"), "{}", out.content);
    let out = run("concat_apply_plan", json!({"dry_run": true, "plan": {"name": "p", "source": "/s.mp4", "kept_ranges": []}})).await;
    assert!(out.is_error && out.content.contains("kept_ranges"), "{}", out.content);
}

#[tokio::test]
async fn session_without_binary_reports_install_hint() {
    // PATH-less env and bogus configured path: error text is what the model sees.
    let session = ConcatSession::new(ConcatConfig { path: Some("/definitely/not/here/concat".into()), ..ConcatConfig::default() });
    let tools = concat_tools(session);
    let out = tool(&tools, "concat_status").execute(ToolCtx::default(), json!({})).await.unwrap();
    assert!(out.is_error && out.content.contains("[concat] path"), "{}", out.content);
}

#[cfg(target_os = "android")]
#[tokio::test]
async fn android_is_unsupported() {
    assert!(matches!(ProcessBackend::spawn(&ConcatConfig::default()).await, Err(ConcatError::Unsupported(_))));
}
