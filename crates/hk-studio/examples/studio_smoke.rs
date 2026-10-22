//! One end-to-end check, without live model or publishing APIs.
//! cargo run -p hk-studio --example studio_smoke -- <4-second-video> <scratch-dir>
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use hk_concat::{ConcatClient, ConcatConfig, ConcatMode, StudioClipPlan};
use hk_llm::{
    CancellationToken, ChatRequest, EventStream, FinishReason, ModelInfo, Provider, StreamEvent,
};
use hk_store::secrets::MemorySecretStore;
use hk_studio::{Studio, StudioError};
use serde_json::{Value, json};

#[derive(Debug)]
struct FixtureModel;

#[async_trait::async_trait]
impl Provider for FixtureModel {
    fn id(&self) -> &str {
        "fixture"
    }
    async fn models(&self) -> hk_llm::Result<Vec<ModelInfo>> {
        Ok(Vec::new())
    }
    async fn stream(
        &self,
        _req: ChatRequest,
        _cancel: CancellationToken,
    ) -> hk_llm::Result<EventStream> {
        let response = json!({"candidates":[{"id":"","span":{"start":0.8,"end":4.0},
            "title":"A valuable moment","hook":"Why this matters","reason":"A complete thought", "score":76,
            "criteria":[{"name":"hook","score":76,"reason":"Clear opening"}]}]});
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(StreamEvent::TextDelta {
                text: response.to_string(),
            }),
            Ok(StreamEvent::Finish(FinishReason::Stop)),
        ])))
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let video = PathBuf::from(args.get(1).ok_or("video argument required")?);
    let scratch = PathBuf::from(args.get(2).ok_or("scratch directory argument required")?);
    let studio = Studio::new(
        &scratch,
        Arc::new(FixtureModel),
        Arc::new(MemorySecretStore::default()),
    )?;
    let imported = studio.request("import", json!({"path":video})).await?;
    let id = imported["project"]["id"]
        .as_str()
        .ok_or("project id missing")?;
    let transcript = json!({"language":"en","duration":4.0,"words":[
        {"text":"Um","start":0.0,"end":0.3},{"text":"valuable","start":0.5,"end":1.0},
        {"text":"moment","start":1.1,"end":1.5},{"text":"really","start":2.6,"end":2.9},
        {"text":"matters.","start":3.0,"end":3.5}]});
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let server = std::thread::spawn(move || -> std::io::Result<()> {
        let (mut stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(15)))?;
        let mut request = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            let n = stream.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..n]);
            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("content-length:")
                            .and_then(|s| s.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        let body = transcript.to_string();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )?;
        Ok(())
    });
    let mut config = studio.request("connections", json!({})).await?["config"].clone();
    config["stt"]["base_url"] = json!(format!("http://{address}/v1"));
    config["model"] = json!("fixture/demo");
    studio.request("save_connections", config).await?;
    let mut current = studio
        .request("transcribe", json!({"project_id":id}))
        .await?;
    server
        .join()
        .map_err(|_| "speech fixture server failed")??;
    let mut edit = current["project"]["edit"].clone();
    edit["deleted_words"] = json!([3]);
    edit["remove_fillers"] = json!(true);
    edit["remove_silences"] = json!(true);
    current = studio
        .request(
            "edit",
            json!({"project_id":id,"revision":current["project"]["revision"],"edit":edit}),
        )
        .await?;
    let expected = current["output_duration"]
        .as_f64()
        .ok_or("output duration missing")?;
    assert!(expected < 3.0 && expected > 2.0);
    assert!(matches!(
        studio
            .request("edit", json!({"project_id":id,"revision":0,"edit":edit}))
            .await,
        Err(StudioError::Conflict)
    ));
    studio.request("undo", json!({"project_id":id})).await?;
    let redone = studio.request("redo", json!({"project_id":id})).await?;
    assert_eq!(redone["project"]["edit"], edit);
    let highlights=studio.request("analyze",json!({"project_id":id,"model":"fixture/demo","count":1,"min_seconds":3.0,"max_seconds":6.0})).await?;
    assert_eq!(
        highlights["project"]["candidates"]
            .as_array()
            .ok_or("candidates missing")?
            .len(),
        1
    );
    let clip_id = highlights["project"]["candidates"][0]["id"]
        .as_str()
        .ok_or("clip id missing")?;
    let with_broll = studio
        .request(
            "add_broll",
            json!({"project_id":id,"path":video,"span":{"start":0.4,"end":1.4}}),
        )
        .await?;
    let edited: hk_studio::Project = serde_json::from_value(with_broll["project"].clone())?;
    let clip = edited.candidates[0].span;
    let mapped = edited.mapped_broll(Some(clip))?;
    assert_eq!(mapped.len(), 1);
    assert!((mapped[0].1 - 0.1).abs() < 0.001);
    let expected = edited
        .kept_ranges(Some(clip))?
        .iter()
        .map(|r| r.duration())
        .sum::<f64>();
    let playback = studio
        .request("playback", json!({"project_id":id,"clip_id":clip_id}))
        .await?;
    assert!((playback["duration"].as_f64().ok_or("preview duration")? - expected).abs() < 0.001);
    std::fs::write(
        scratch.join("playback.json"),
        serde_json::to_vec_pretty(&playback)?,
    )?;
    let editor = studio
        .request("prepare_editor", json!({"project_id":id,"clip_id":clip_id}))
        .await?;
    let reopened = studio
        .request("prepare_editor", json!({"project_id":id,"clip_id":clip_id}))
        .await?;
    assert_eq!(
        editor["path"], reopened["path"],
        "Reopening must preserve manual edits"
    );
    let full_editor = studio
        .request("prepare_editor", json!({"project_id":id}))
        .await?;
    assert_ne!(editor["path"], full_editor["path"]);
    let clip_again = studio
        .request("prepare_editor", json!({"project_id":id,"clip_id":clip_id}))
        .await?;
    assert_eq!(
        editor["path"], clip_again["path"],
        "Switching clips must preserve editor projects"
    );
    let job = studio
        .request("render", json!({"project_id":id,"clip_id":clip_id}))
        .await?;
    let finished = tokio::time::timeout(std::time::Duration::from_secs(90), async {
        loop {
            let jobs = studio.request("jobs", json!({})).await?;
            if let Some(job) = jobs.as_array().and_then(|jobs| {
                jobs.iter()
                    .find(|j| j["id"] == job["id"] && j["state"] != "running")
            }) {
                break Ok::<Value, StudioError>(job.clone());
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    })
    .await??;
    assert_eq!(finished["state"], "completed", "{finished}");
    let output = PathBuf::from(finished["output"].as_str().ok_or("output missing")?);
    let rendered = hk_media::probe(&output).await?;
    assert_eq!(rendered.display_size(), Some((1080, 1920)));
    assert!((rendered.duration.ok_or("duration missing")? - expected).abs() < 0.15);
    assert!(rendered.has_audio());
    let package = studio
        .request(
            "export_package",
            json!({"project_id":id,"job_id":job["id"],"clip_id":clip_id}),
        )
        .await?;
    let directory = PathBuf::from(
        package["directory"]
            .as_str()
            .ok_or("package directory missing")?,
    );
    let subtitles = std::fs::read_to_string(directory.join("captions.srt"))?;
    assert!(!subtitles.contains("Um") && !subtitles.contains("really"));
    assert!(subtitles.contains("valuable") && directory.join("video.mp4").exists());
    let plan: StudioClipPlan = serde_json::from_value(
        studio
            .request("concat_plan", json!({"project_id":id,"clip_id":clip_id}))
            .await?,
    )?;
    let client = ConcatClient::connect(&ConcatConfig {
        mode: ConcatMode::Headless,
        roots: vec![
            scratch.clone(),
            video.parent().ok_or("source parent missing")?.to_path_buf(),
        ],
        ..ConcatConfig::default()
    })
    .await?;
    let applied = hk_concat::apply_plan(&client, scratch.to_string_lossy().as_ref(), &plan).await?;
    client.project_save(&applied.project, None).await?;
    let view = client.timeline_read(&applied.project).await?;
    assert!(!view.timeline.clips.is_empty());
    let concat_output = scratch.join("concat-render.mp4");
    let export = client
        .export_start(
            &applied.project,
            &hk_concat::ExportSpec {
                output: concat_output.to_string_lossy().into_owned(),
                width: Some(180),
                height: Some(320),
                codec: Some("h264".into()),
                preset: Some("ultrafast".into()),
                ..hk_concat::ExportSpec::default()
            },
        )
        .await?;
    let status = client
        .export_wait(
            &export.job,
            std::time::Duration::from_millis(200),
            std::time::Duration::from_secs(60),
            |_| {},
        )
        .await?;
    assert!(concat_output.exists(), "Concat export failed: {status:?}");
    let concat_info = hk_media::probe(&concat_output).await?;
    assert_eq!(concat_info.display_size(), Some((180, 320)));
    client.shutdown().await;
    studio.shutdown();
    let restored = Studio::new(
        &scratch,
        Arc::new(FixtureModel),
        Arc::new(MemorySecretStore::default()),
    )?;
    let jobs = restored.request("jobs", json!({})).await?;
    assert!(
        jobs.as_array()
            .ok_or("jobs missing")?
            .iter()
            .any(|j| j["id"] == job["id"] && j["state"] == "completed")
    );
    let projects = restored.request("projects", json!({})).await?;
    assert_eq!(projects.as_array().ok_or("projects missing")?.len(), 1);
    println!(
        "Studio smoke passed: speech API fixture, edit/undo, conflict guard, fixture highlights, captioned vertical render, export package, real Concat MCP edit/export, restart persistence"
    );
    Ok(())
}
