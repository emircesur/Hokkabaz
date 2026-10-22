use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hk_concat::{ConcatConfig, ConcatSession};
use hk_llm::{KeyRef, Provider};
use hk_media::{AudioSpec, RenderSpec, Segment, Tools};
use hk_store::{Secret, SecretStore};
use hk_stt::{SttConfig, TranscribeOpts, Transcript};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::store::{ProjectStore, atomic_json};
use crate::{
    Broll, Candidate, Edit, Project, Result, Source, Span, StudioError, ai, ass, invalid, srt,
    stock, templates, vtt,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Connections {
    /// Existing Hokkabaz provider/model; keys remain in the shared secret store.
    pub model: String,
    pub stt: SttConfig,
    pub concat_binary: Option<PathBuf>,
    /// Optional image detector: POST multipart image -> {subjects:[{x,y,confidence}],scene_cut:bool}.
    pub vision_url: Option<String>,
}

impl Default for Connections {
    fn default() -> Self {
        Self {
            model: String::new(),
            stt: SttConfig {
                base_url: "http://127.0.0.1:8080/v1".into(),
                key_ref: Some(KeyRef::new("studio/stt")),
                ..SttConfig::default()
            },
            concat_binary: None,
            vision_url: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub project_id: String,
    pub clip_id: Option<String>,
    pub render_key: String,
    #[serde(default)]
    pub created_ms: u64,
    pub state: String,
    pub progress: f64,
    pub output: PathBuf,
    pub error: Option<String>,
}

struct Running {
    job: Job,
    cancel: hk_llm::CancellationToken,
}

pub struct Studio {
    store: ProjectStore,
    root: PathBuf,
    provider: Arc<dyn Provider>,
    secrets: Arc<dyn SecretStore>,
    tools: Tools,
    http: reqwest::Client,
    jobs: Mutex<BTreeMap<String, Running>>,
    config_lock: Mutex<()>,
}

impl std::fmt::Debug for Studio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Studio")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

fn string<'a>(args: &'a Value, name: &str) -> Result<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid(format!("{name} is required")))
}

impl Studio {
    pub fn new(
        data: &Path,
        provider: Arc<dyn Provider>,
        secrets: Arc<dyn SecretStore>,
    ) -> Result<Arc<Self>> {
        let root = data.join("studio");
        std::fs::create_dir_all(&root)?;
        std::fs::create_dir_all(root.join("jobs"))?;
        let mut jobs = BTreeMap::new();
        for entry in std::fs::read_dir(root.join("jobs"))? {
            let path = entry?.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let mut job: Job = serde_json::from_slice(&std::fs::read(&path)?)?;
            if job.state == "running" {
                job.state = "interrupted".into();
                job.error = Some("Render interrupted by instance restart; start it again".into());
                atomic_json(&path, &job)?;
            }
            jobs.insert(
                job.id.clone(),
                Running {
                    job,
                    cancel: hk_llm::CancellationToken::new(),
                },
            );
        }
        Ok(Arc::new(Self {
            store: ProjectStore::new(root.join("projects"))?,
            root,
            provider,
            secrets,
            tools: Tools::from_env(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(600))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            jobs: Mutex::new(jobs),
            config_lock: Mutex::new(()),
        }))
    }

    pub(crate) fn connections(&self) -> Result<Connections> {
        let path = self.root.join("connections.json");
        if path.exists() {
            Ok(serde_json::from_slice(&std::fs::read(path)?)?)
        } else {
            Ok(Connections::default())
        }
    }

    pub(crate) fn clip<'a>(
        &self,
        project: &'a Project,
        args: &Value,
    ) -> Result<Option<&'a Candidate>> {
        match args
            .get("clip_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            None => Ok(None),
            Some(id) => project
                .candidates
                .iter()
                .find(|c| c.id == id)
                .map(Some)
                .ok_or_else(|| invalid("clip no longer exists")),
        }
    }

    fn model(&self, args: &Value) -> Result<String> {
        Ok(args
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or(self.connections()?.model))
    }

    /// A single JSON operation shared by FFI, authenticated REST, flows and agent tools.
    /// Only the UI/REST admin boundary may call `save_connections` or `set_secret`.
    pub async fn request(self: &Arc<Self>, operation: &str, args: Value) -> Result<Value> {
        match operation {
            "editor_environment" => {
                let binary = hk_concat::locate_binary(self.connections()?.concat_binary.as_deref())?;
                return Ok(json!({"binary":binary,"path":""}));
            }
            "connections" => {
                let config=self.connections()?;
                let mut keys=BTreeMap::new();
                for name in ["stt","pexels","pixabay","youtube","vision"] {
                    keys.insert(name,self.secrets.get(&format!("studio/{name}"))?.is_some());
                }
                return Ok(json!({"config":config,"keys":keys,"local_whisper":cfg!(feature="whisper")}));
            }
            "save_connections" => {
                let mut config: Connections=serde_json::from_value(args)?;
                validate_endpoint(&config.stt.base_url)?;
                if let Some(url)=&config.vision_url { validate_endpoint(url)?; }
                // Arbitrary headers could smuggle secrets into the plain JSON settings file.
                config.stt.headers.clear();
                config.stt.key_ref=Some(KeyRef::new("studio/stt"));
                if config.stt.model.trim().is_empty() { return Err(invalid("STT model is required")); }
                let _guard=self.config_lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                atomic_json(&self.root.join("connections.json"),&config)?;
                return Ok(json!({"saved":true}));
            }
            "set_secret" => {
                let name=string(&args,"name")?;
                if !["stt","pexels","pixabay","youtube","vision"].contains(&name) { return Err(invalid("unknown Studio connection")); }
                let key=args["value"].as_str().unwrap_or("").trim();
                let target=format!("studio/{name}");
                if key.is_empty() { self.secrets.delete(&target)?; } else { self.secrets.set(&target,&Secret::new(key))?; }
                return Ok(json!({"saved":true}));
            }
            "templates" => return Ok(json!(templates().iter().map(|(id,style)|json!({"id":id,"captions":style})).collect::<Vec<_>>())),
            "projects" => return Ok(json!(self.store.list()?.iter().map(|p|json!({"id":p.id,"name":p.name,"duration":p.duration(),"revision":p.revision})).collect::<Vec<_>>())),
            "import" => return self.import(&args).await,
            "jobs" => return Ok(json!(self.jobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner).values().map(|r|r.job.clone()).collect::<Vec<_>>())),
            "cancel_render" => {
                let id=string(&args,"job_id")?;
                if let Some(r)=self.jobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(id) { r.cancel.cancel(); }
                return Ok(json!({"cancelled":true}));
            }
            "search_broll" => return stock::search(&self.http,self.secrets.as_ref(),string(&args,"provider")?,string(&args,"query")?).await,
            _ => {}
        }
        let id = string(&args, "project_id")?;
        let mut project = self.store.load(id)?;
        let revision = project.revision;
        let dir = self.store.dir(id)?;
        match operation {
            "get" => {}
            "edit" => {
                if args["revision"].as_u64() != Some(revision) {
                    return Err(StudioError::Conflict);
                }
                project.set_edit(serde_json::from_value(args["edit"].clone())?)?;
            }
            "undo" => {
                if let Some(previous) = project.undo.pop() {
                    project
                        .redo
                        .push(std::mem::replace(&mut project.edit, previous));
                }
            }
            "redo" => {
                if let Some(next) = project.redo.pop() {
                    project
                        .undo
                        .push(std::mem::replace(&mut project.edit, next));
                }
            }
            "template" => {
                let name = string(&args, "template")?;
                let style = templates()
                    .into_iter()
                    .find(|(id, _)| *id == name)
                    .ok_or_else(|| invalid("unknown template"))?
                    .1;
                project.set_edit(Edit {
                    captions: style,
                    template: name.into(),
                    ..project.edit.clone()
                })?;
            }
            "import_transcript" => {
                let mut transcript: Transcript =
                    serde_json::from_value(args["transcript"].clone())?;
                prepare_transcript(&mut transcript, project.duration())?;
                project.transcript = Some(transcript);
                project.edit.deleted_words.clear();
                project.edit.restored_words.clear();
                project.undo.clear();
                project.redo.clear();
            }
            "transcribe" => {
                let config = self.connections()?;
                let mut stt = config.stt;
                if self.secrets.get("studio/stt")?.is_none() {
                    stt.key_ref = None;
                }
                let keys = |key: &KeyRef| {
                    self.secrets
                        .get(&key.0)
                        .ok()
                        .flatten()
                        .map(|s| s.expose().to_owned())
                };
                let transcriber = hk_stt::build_chunked_transcriber(
                    &stt,
                    &keys,
                    Arc::new(MediaSplitter(self.tools.clone())),
                )?;
                if !project.source.info.has_audio() {
                    return Err(invalid("source has no audio track"));
                }
                let audio = dir.join("speech.wav");
                self.tools
                    .extract_audio(&project.source.path, &audio, AudioSpec::default())
                    .await?;
                let result = transcriber
                    .transcribe(
                        &audio,
                        &TranscribeOpts {
                            language: stt.language.clone(),
                            ..TranscribeOpts::default()
                        },
                    )
                    .await;
                let _ = tokio::fs::remove_file(audio).await;
                let mut transcript = result?;
                prepare_transcript(&mut transcript, project.duration())?;
                project.transcript = Some(transcript);
                project.edit.deleted_words.clear();
                project.edit.restored_words.clear();
                project.undo.clear();
                project.redo.clear();
            }
            "analyze" => {
                project.candidates = ai::highlights(
                    self.provider.as_ref(),
                    &self.model(&args)?,
                    &dir,
                    &project,
                    ai::HighlightOptions {
                        count: args["count"].as_u64().unwrap_or(5) as usize,
                        min: args["min_seconds"].as_f64().unwrap_or(15.0),
                        max: args["max_seconds"].as_f64().unwrap_or(60.0),
                        rubric: args["rubric"].as_str().unwrap_or(""),
                    },
                )
                .await?;
            }
            "score_clip" => {
                let clip = self
                    .clip(&project, &args)?
                    .ok_or_else(|| invalid("clip_id is required"))?;
                let scored = ai::score(
                    self.provider.as_ref(),
                    &self.model(&args)?,
                    &dir,
                    &project,
                    clip,
                    args["rubric"].as_str().unwrap_or(""),
                )
                .await?;
                if let Some(slot) = project.candidates.iter_mut().find(|c| c.id == scored.id) {
                    *slot = scored;
                }
            }
            "social" => {
                let mut metadata = ai::social(
                    self.provider.as_ref(),
                    &self.model(&args)?,
                    &dir,
                    &project,
                    self.clip(&project, &args)?,
                    args["platform"].as_str().unwrap_or("youtube"),
                )
                .await?;
                metadata.clip_id = self.clip(&project, &args)?.map(|c| c.id.clone());
                project
                    .social
                    .retain(|s| s.platform != metadata.platform || s.clip_id != metadata.clip_id);
                project.social.push(metadata);
            }
            "save_social" => {
                let mut metadata: crate::Social = serde_json::from_value(args["metadata"].clone())?;
                metadata.clip_id = self.clip(&project, &args)?.map(|c| c.id.clone());
                if metadata.title.trim().is_empty()
                    || metadata.title.chars().count() > 100
                    || !["youtube", "tiktok", "instagram", "linkedin"]
                        .contains(&metadata.platform.as_str())
                {
                    return Err(invalid(
                        "Invalid social metadata: choose a platform and title of at most 100 characters",
                    ));
                }
                project
                    .social
                    .retain(|s| s.platform != metadata.platform || s.clip_id != metadata.clip_id);
                project.social.push(metadata);
            }
            "reframe" => {
                self.track_subject(&mut project, &dir).await?;
            }
            "add_broll" => {
                let overlay = self.broll(&args, &dir).await?;
                let mut edit = project.edit.clone();
                edit.broll.push(overlay);
                project.set_edit(edit)?;
            }
            "auto_broll" => {
                self.auto_broll(&mut project, &dir, &args).await?;
            }
            "preview" => {
                let at = args["time"].as_f64().unwrap_or(0.0);
                if !at.is_finite() || at < 0.0 || at > project.duration() {
                    return Err(invalid("invalid preview time"));
                }
                let path = dir.join(format!("preview-{}.jpg", uuid::Uuid::new_v4()));
                self.tools
                    .thumbnail(&project.source.path, at, &path, 640)
                    .await?;
                let bytes = tokio::fs::read(&path).await?;
                tokio::fs::remove_file(path).await?;
                return Ok(json!({"bytes":bytes}));
            }
            "playback" => {
                return self.playback(&project, self.clip(&project, &args)?.map(|c| c.span), &dir);
            }
            "prepare_editor" => return self.prepare_editor(&project, &args, &dir).await,
            "render" => return self.start_render(&project, &args, &dir).await,
            "export_package" => return self.export_package(&project, &args, &dir),
            "concat_plan" | "open_concat" => {
                let plan =
                    self.concat_plan(&project, self.clip(&project, &args)?.map(|c| c.span))?;
                if operation == "concat_plan" {
                    return Ok(json!(plan));
                }
                let config = self.connections()?;
                let session = ConcatSession::new(ConcatConfig {
                    path: config.concat_binary.clone(),
                    mode: hk_concat::ConcatMode::Headless,
                    roots: vec![
                        dir.clone(),
                        project.source.path.parent().unwrap_or(&dir).to_path_buf(),
                    ],
                    ..ConcatConfig::default()
                });
                let result = async {
                    let client = session.client().await?;
                    let applied =
                        hk_concat::apply_plan(&client, dir.to_string_lossy().as_ref(), &plan)
                            .await?;
                    client.project_save(&applied.project, None).await?;
                    Ok::<_, StudioError>(json!({"project":applied.project}))
                }
                .await;
                // Save before stopping the MCP child; launch the saved project in the editor.
                session.shutdown().await;
                let result = result?;
                self.launch_concat(
                    result["project"].as_str().unwrap_or(""),
                    config.concat_binary.as_deref(),
                )
                .await?;
                return Ok(result);
            }
            "publish" => return self.publish(&project, &args).await,
            _ => return Err(invalid(format!("unknown Studio operation: {operation}"))),
        }
        if operation != "get" {
            self.store.save(&mut project, Some(revision))?;
        }
        let ranges = project.kept_ranges(None)?;
        Ok(
            json!({"project":project,"kept_ranges":ranges,"output_duration":ranges.iter().map(|s|s.duration()).sum::<f64>()}),
        )
    }

    async fn import(&self, args: &Value) -> Result<Value> {
        let path = std::fs::canonicalize(string(args, "path")?)?;
        let info = self.tools.probe(&path).await?;
        if info.video().is_none() || !info.duration.is_some_and(|d| d.is_finite() && d > 0.0) {
            return Err(invalid("Import a video with a valid duration"));
        }
        let name = path
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("Video")
            .to_owned();
        let mut project = Project {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            revision: 0,
            source: Source { path, info },
            transcript: None,
            edit: Edit::default(),
            candidates: Vec::new(),
            social: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
        };
        self.store.save(&mut project, None)?;
        Ok(json!({"project":project}))
    }

    async fn broll(&self, args: &Value, dir: &Path) -> Result<Broll> {
        let path = if let Some(url) = args["url"].as_str() {
            let path = dir.join(format!("stock-{}.mp4", uuid::Uuid::new_v4()));
            stock::download(&self.http, url, &path).await?;
            path
        } else {
            std::fs::canonicalize(string(args, "path")?)?
        };
        let info = self.tools.probe(&path).await?;
        if info.video().is_none() {
            return Err(invalid("B-roll must contain video or an image"));
        }
        let span: Span = serde_json::from_value(args["span"].clone())?;
        if let Some(duration) = info.duration {
            if span.duration() > duration + 0.01 {
                return Err(invalid("B-roll is shorter than its overlay range"));
            }
        }
        Ok(Broll {
            path,
            span,
            attribution: args["attribution"].as_str().map(str::to_owned),
        })
    }

    async fn auto_broll(&self, project: &mut Project, dir: &Path, args: &Value) -> Result<()> {
        let ranges = project.kept_ranges(None)?;
        let words = project.output_words(&ranges);
        if words.is_empty() {
            return Err(invalid("Transcribe before planning B-roll"));
        }
        let prompt = format!(
            "Suggest at most 5 relevant stock video inserts. Each 2–5 seconds on this EDITED timeline. \
            Return {{\"inserts\":[{{\"query\":\"short visual search\",\"span\":{{\"start\":0,\"end\":3}}}}]}}. \
            No overlapping inserts, duration <= {}. Transcript: {}",
            ranges.iter().map(|r| r.duration()).sum::<f64>(),
            serde_json::to_string(&words)?
        );
        let result =
            ai::json_completion(self.provider.as_ref(), &self.model(args)?, dir, &prompt).await?;
        let inserts = result["inserts"]
            .as_array()
            .ok_or_else(|| invalid("model omitted B-roll inserts"))?;
        if inserts.len() > 5 {
            return Err(invalid("model returned too many B-roll inserts"));
        }
        let provider = args["provider"].as_str().unwrap_or("pexels");
        let mut edit = project.edit.clone();
        for insert in inserts {
            let found = stock::search(
                &self.http,
                self.secrets.as_ref(),
                provider,
                string(insert, "query")?,
            )
            .await?;
            let asset = found["assets"]
                .as_array()
                .and_then(|a| a.first())
                .ok_or_else(|| invalid("No matching stock video"))?;
            let value = json!({"url":asset["url"],"attribution":asset["attribution"],"span":insert["span"]});
            edit.broll.push(self.broll(&value, dir).await?);
        }
        project.set_edit(edit)
    }

    async fn track_subject(&self, project: &mut Project, dir: &Path) -> Result<()> {
        let config = self.connections()?;
        let Some(url) = config.vision_url else {
            let mut edit = project.edit.clone();
            edit.subject.clear();
            return project.set_edit(edit);
        };
        let mut points = Vec::new();
        let token = self.secrets.get("studio/vision")?;
        let mut time = 0.0;
        while time < project.duration() {
            let image = dir.join("tracking-frame.jpg");
            self.tools
                .thumbnail(&project.source.path, time, &image, 640)
                .await?;
            let bytes = tokio::fs::read(&image).await?;
            let _ = tokio::fs::remove_file(&image).await;
            let form = reqwest::multipart::Form::new().part(
                "image",
                reqwest::multipart::Part::bytes(bytes)
                    .file_name("frame.jpg")
                    .mime_str("image/jpeg")?,
            );
            let mut request = self.http.post(&url).multipart(form);
            if let Some(token) = &token {
                request = request.bearer_auth(token.expose());
            }
            let result: Value = request.send().await?.error_for_status()?.json().await?;
            if let Some(subject) = result["subjects"].as_array().and_then(|v| {
                v.iter().max_by(|a, b| {
                    a["confidence"]
                        .as_f64()
                        .unwrap_or(0.0)
                        .total_cmp(&b["confidence"].as_f64().unwrap_or(0.0))
                })
            }) {
                points.push(crate::SubjectPoint {
                    time,
                    x: subject["x"]
                        .as_f64()
                        .ok_or_else(|| invalid("detector omitted x"))?,
                    y: subject["y"]
                        .as_f64()
                        .ok_or_else(|| invalid("detector omitted y"))?,
                    scene_cut: result["scene_cut"].as_bool().unwrap_or(false),
                });
            }
            time += 2.0;
        }
        let mut edit = project.edit.clone();
        edit.subject = points;
        project.set_edit(edit)
    }

    pub(crate) fn media_spec(
        &self,
        project: &Project,
        clip: Option<Span>,
        dir: &Path,
        key: &str,
        output: &Path,
    ) -> Result<RenderSpec> {
        let ranges = project.kept_ranges(clip)?;
        if ranges.is_empty() {
            return Err(invalid("Nothing remains after the selected cuts"));
        }
        let mut spec = RenderSpec::new(
            vec![hk_media::Source {
                path: project.source.path.clone(),
                has_audio: project.source.info.has_audio(),
            }],
            ranges
                .iter()
                .map(|r| Segment {
                    source: 0,
                    start: r.start,
                    end: r.end,
                })
                .collect(),
            output,
        );
        spec.size = Some(project.edit.aspect.size());
        spec.fps = Some(30.0);
        spec.crop = project.crop_path(&ranges);
        spec.preset = Some("veryfast".into());
        if project.edit.captions.enabled {
            let path = dir.join(format!("captions-{key}.ass"));
            std::fs::write(
                &path,
                ass(
                    &project.output_words(&ranges),
                    &project.edit.captions,
                    project.edit.aspect.size(),
                ),
            )?;
            spec.subtitles = Some(path);
        }
        spec.overlays = project
            .mapped_broll(clip)?
            .iter()
            .map(|(b, source_start)| hk_media::Overlay {
                path: b.path.clone(),
                source_start: *source_start,
                kind: if b
                    .path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| ["jpg", "jpeg", "png", "webp"].contains(&e))
                {
                    hk_media::OverlayKind::Image
                } else {
                    hk_media::OverlayKind::Video
                },
                start: b.span.start,
                end: b.span.end,
                x: None,
                y: None,
                width: Some(project.edit.aspect.size().0),
                opacity: None,
            })
            .collect();
        spec.music = project.edit.music.clone().map(|path| hk_media::MusicMix {
            path,
            gain: 0.15,
            ducking: Some(hk_media::Ducking::default()),
            loop_track: true,
        });
        Ok(spec)
    }

    async fn start_render(
        self: &Arc<Self>,
        project: &Project,
        args: &Value,
        dir: &Path,
    ) -> Result<Value> {
        project.edit.validate(
            project.duration(),
            project.transcript.as_ref().map_or(0, |t| t.words.len()),
        )?;
        let clip = self.clip(project, args)?;
        let job_id = uuid::Uuid::new_v4().to_string();
        let output = dir.join(format!("render-{job_id}.mp4"));
        let spec = self.media_spec(project, clip.map(|c| c.span), dir, &job_id, &output)?;
        // Validate/spawn before advertising a queued job.
        let mut handle = self.tools.render(spec).await?;
        let mut progress = handle.progress();
        let cancel = hk_llm::CancellationToken::new();
        let job = Job {
            id: job_id.clone(),
            project_id: project.id.clone(),
            clip_id: clip.map(|c| c.id.clone()),
            render_key: self.render_key(project, clip.map(|c| c.span))?,
            created_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64),
            state: "running".into(),
            progress: 0.0,
            output,
            error: None,
        };
        {
            let mut jobs = self
                .jobs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if jobs.values().filter(|j| j.job.state == "running").count() >= 2 {
                handle.cancel();
                return Err(invalid(
                    "Two renders are already running; wait or cancel one",
                ));
            }
            atomic_json(&self.root.join("jobs").join(format!("{job_id}.json")), &job)?;
            jobs.insert(
                job_id.clone(),
                Running {
                    job: job.clone(),
                    cancel: cancel.clone(),
                },
            );
        }
        let studio = self.clone();
        tokio::spawn(async move {
            let wait = handle.wait();
            tokio::pin!(wait);
            let mut progress_open = true;
            let outcome = loop {
                tokio::select! {
                    result=&mut wait => break result,
                    ()=cancel.cancelled() => break Err(hk_media::MediaError::Cancelled),
                    result=progress.changed(), if progress_open => {
                        if result.is_ok() {
                            if let Some(r)=studio.jobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_mut(&job_id) {
                                r.job.progress=progress.borrow().fraction;
                            }
                        } else { progress_open=false; }
                    }
                }
            };
            // Dropping wait on cancel drops RenderHandle's cancellation sender.
            let mut jobs = studio
                .jobs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(r) = jobs.get_mut(&job_id) {
                match outcome {
                    Ok(_) => {
                        r.job.state = "completed".into();
                        r.job.progress = 1.0;
                    }
                    Err(hk_media::MediaError::Cancelled) => {
                        r.job.state = "cancelled".into();
                    }
                    Err(error) => {
                        r.job.state = "failed".into();
                        r.job.error = Some(error.to_string());
                    }
                }
                if let Err(error) = atomic_json(
                    &studio.root.join("jobs").join(format!("{job_id}.json")),
                    &r.job,
                ) {
                    r.job.error = Some(format!("Could not save render status: {error}"));
                }
            }
        });
        Ok(json!(job))
    }

    fn export_package(&self, project: &Project, args: &Value, dir: &Path) -> Result<Value> {
        let ranges = project.kept_ranges(self.clip(project, args)?.map(|c| c.span))?;
        let words = project.output_words(&ranges);
        let package = dir.join(format!("package-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&package)?;
        std::fs::write(
            package.join("captions.srt"),
            srt(&words, &project.edit.captions),
        )?;
        std::fs::write(
            package.join("captions.vtt"),
            vtt(&words, &project.edit.captions),
        )?;
        std::fs::write(
            package.join("captions.ass"),
            ass(&words, &project.edit.captions, project.edit.aspect.size()),
        )?;
        atomic_json(&package.join("social.json"), &project.social)?;
        atomic_json(
            &package.join("concat-plan.json"),
            &self.concat_plan(project, self.clip(project, args)?.map(|c| c.span))?,
        )?;
        atomic_json(&package.join("project.json"), project)?;
        atomic_json(
            &package.join("attributions.json"),
            &project
                .edit
                .broll
                .iter()
                .filter_map(|b| b.attribution.as_ref())
                .collect::<Vec<_>>(),
        )?;
        let clip = self.clip(project, args)?;
        let render_key = self.render_key(project, clip.map(|c| c.span))?;
        let completed = self
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|r| {
                r.job.project_id == project.id
                    && r.job.state == "completed"
                    && r.job.clip_id == clip.map(|c| c.id.clone())
                    && r.job.render_key == render_key
                    && args["job_id"].as_str().is_none_or(|id| id == r.job.id)
            })
            .max_by_key(|r| r.job.created_ms)
            .map(|r| r.job.clone());
        if let Some(job) = completed {
            std::fs::copy(job.output, package.join("video.mp4"))?;
        }
        Ok(json!({"directory":package}))
    }

    fn completed_job(&self, project_id: &str, id: Option<&str>) -> Option<Job> {
        self.jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|r| {
                r.job.project_id == project_id
                    && r.job.state == "completed"
                    && id.is_none_or(|i| i == r.job.id)
            })
            .max_by_key(|r| r.job.created_ms)
            .map(|r| r.job.clone())
    }

    async fn publish(&self, project: &Project, args: &Value) -> Result<Value> {
        if args["confirmed"].as_bool() != Some(true) {
            return Err(invalid("Review metadata and confirm the YouTube upload"));
        }
        let job = self
            .completed_job(&project.id, Some(string(args, "job_id")?))
            .ok_or_else(|| invalid("Choose a completed render from this project"))?;
        let clip = job
            .clip_id
            .as_ref()
            .map(|id| {
                project
                    .candidates
                    .iter()
                    .find(|c| c.id == *id)
                    .ok_or_else(|| invalid("This render's clip no longer exists"))
            })
            .transpose()?;
        if self.render_key(project, clip.map(|c| c.span))? != job.render_key {
            return Err(invalid(
                "This render predates your current edits; render again before uploading",
            ));
        }
        let metadata = project
            .social
            .iter()
            .find(|s| s.platform == "youtube" && s.clip_id == job.clip_id)
            .ok_or_else(|| {
                invalid("Generate and review YouTube metadata for this rendered clip first")
            })?;
        let token = self
            .secrets
            .get("studio/youtube")?
            .ok_or_else(|| invalid("Add a YouTube OAuth access token with youtube.upload scope"))?;
        stock::youtube(
            &self.http,
            &token,
            &job.output,
            metadata,
            args["privacy"].as_str().unwrap_or("private"),
        )
        .await
    }

    pub(crate) fn concat_plan(
        &self,
        project: &Project,
        clip: Option<Span>,
    ) -> Result<hk_concat::StudioClipPlan> {
        let ranges = project.kept_ranges(clip)?;
        let mut plan = hk_concat::StudioClipPlan::new(
            format!("studio-{}-{}", project.id, uuid::Uuid::new_v4()),
            project.source.path.to_string_lossy(),
            ranges
                .iter()
                .map(|r| hk_concat::TimeRange {
                    start: r.start,
                    end: r.end,
                })
                .collect(),
        );
        let (width, height) = project.edit.aspect.size();
        plan.video = hk_concat::VideoSettings {
            width,
            height,
            rate_num: 30,
            rate_den: 1,
        };
        let style = &project.edit.captions;
        if style.enabled {
            let transcript = project.transcript.as_ref();
            plan.captions = Some(hk_concat::CaptionTrack {
                words: transcript
                    .into_iter()
                    .flat_map(|t| t.words.iter().enumerate())
                    .filter(|(i, w)| {
                        !project.edit.deleted_words.contains(i)
                            && !(project.edit.remove_fillers
                                && w.flags.filler
                                && !project.edit.restored_words.contains(i))
                    })
                    .map(|(_, w)| hk_concat::Word {
                        text: w.text.clone(),
                        start: w.start,
                        end: w.end,
                    })
                    .collect(),
                style: hk_concat::CaptionStyle {
                    font_family: Some(style.font.clone()),
                    font_size: Some(f64::from(style.font_size) / f64::from(height)),
                    color: Some(style.color.clone()),
                    stroke_width: Some(f64::from(style.outline)),
                    uppercase: style.uppercase,
                    max_words_per_line: style.words_per_line,
                    ..hk_concat::CaptionStyle::default()
                },
            });
        }
        if let Some(path) = project.crop_path(&ranges) {
            let (sw, sh) = project
                .source
                .info
                .display_size()
                .unwrap_or((width, height));
            plan.crop_keyframes = path
                .keys
                .iter()
                .map(|key| hk_concat::CropKeyframe {
                    time: key.t,
                    center_x: (key.x + key.w / 2.0) / f64::from(sw),
                    center_y: (key.y + key.h / 2.0) / f64::from(sh),
                    zoom: 1.0,
                })
                .collect();
        }
        plan.overlays = project
            .mapped_broll(clip)?
            .iter()
            .map(|(b, source_start)| hk_concat::Overlay::Media {
                file: b.path.to_string_lossy().into_owned(),
                source_start: *source_start,
                start: b.span.start,
                duration: b.span.duration(),
                scale: 1.0,
                offset_x: 0.0,
                offset_y: 0.0,
                opacity: 1.0,
            })
            .collect();
        Ok(plan)
    }

    pub(crate) fn render_key(&self, project: &Project, clip: Option<Span>) -> Result<String> {
        use sha2::{Digest, Sha256};
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(
                &project.source,
                &project.edit,
                &project.transcript,
                clip
            ))?)
        ))
    }

    #[cfg(not(target_os = "android"))]
    async fn launch_concat(&self, project: &str, binary: Option<&Path>) -> Result<()> {
        let binary = hk_concat::locate_binary(binary)?;
        tokio::process::Command::new(binary)
            .arg("--project")
            .arg(project)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        Ok(())
    }

    #[cfg(target_os = "android")]
    async fn launch_concat(&self, _project: &str, _binary: Option<&Path>) -> Result<()> {
        Err(invalid("Open Concat on a desktop instance"))
    }

    pub fn shutdown(&self) {
        for r in self
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
        {
            r.cancel.cancel();
        }
    }
}

fn validate_endpoint(raw: &str) -> Result<()> {
    let url =
        reqwest::Url::parse(raw).map_err(|_| invalid("endpoint must be a valid HTTP(S) URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid(
            "Use an HTTP(S) endpoint without embedded credentials, query or fragment",
        ));
    }
    Ok(())
}

fn prepare_transcript(transcript: &mut Transcript, duration: f64) -> Result<()> {
    transcript.fix_timings();
    if transcript
        .words
        .iter()
        .any(|w| w.end > duration + 0.5 || w.text.len() > 4096)
    {
        return Err(invalid(
            "Transcript words extend past the video or contain oversized text",
        ));
    }
    transcript.duration = duration;
    hk_stt::FillerSet::for_language(transcript.language.as_deref().unwrap_or("en"), false)
        .mark(transcript);
    transcript.flag_gaps(0.6);
    transcript.regroup_segments(&hk_stt::SegmentOpts::default());
    Ok(())
}

#[derive(Debug)]
struct MediaSplitter(Tools);

#[async_trait::async_trait]
impl hk_stt::AudioSplitter for MediaSplitter {
    async fn duration(&self, audio: &Path) -> hk_stt::Result<f64> {
        self.0
            .probe(audio)
            .await
            .map_err(|e| hk_stt::SttError::Config(e.to_string()))?
            .duration
            .ok_or_else(|| hk_stt::SttError::Config("audio duration unavailable".into()))
    }

    async fn extract(&self, audio: &Path, start: f64, end: f64, out: &Path) -> hk_stt::Result<()> {
        #[cfg(not(target_os = "android"))]
        {
            let status = tokio::process::Command::new(&self.0.ffmpeg)
                .args([
                    "-hide_banner",
                    "-nostdin",
                    "-y",
                    "-loglevel",
                    "error",
                    "-ss",
                    &start.to_string(),
                    "-t",
                    &(end - start).to_string(),
                    "-i",
                ])
                .arg(audio)
                .args(["-vn", "-ac", "1", "-ar", "16000"])
                .arg(out)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .status()
                .await
                .map_err(|e| hk_stt::SttError::Splitter(e.to_string()))?;
            if !status.success() {
                return Err(hk_stt::SttError::Config("FFmpeg audio split failed".into()));
            }
            Ok(())
        }
        #[cfg(target_os = "android")]
        {
            let _ = (audio, start, end, out);
            Err(hk_stt::SttError::Unsupported(
                "Use a desktop render instance".into(),
            ))
        }
    }
}
