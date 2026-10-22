# Hokkabaz Studio — agentic video screen

Separate Flutter screen (`/studio`) + Rust crates for long-video → short-clip workflows (OpusClip-class), with a text-based editor and an agent that drives the whole pipeline. Bring-your-own AI: any hk-llm provider (own API keys) or local models (Ollama, whisper.cpp, on-device).

## Feature map

| Feature | Approach |
|---|---|
| Highlight detection (funny / insightful / hooky segments) | transcript → chunked LLM pass (JSON schema: start, end, title, hook, reason) + audio-energy / laughter / scene-cut signals as hints; dedupe/merge overlaps; min/max duration constraints |
| Auto captions | `Transcriber` trait: OpenAI-compatible `/audio/transcriptions` (word timestamps; works with OpenAI, Groq, local whisper servers) and local `whisper-rs` (feature `whisper`); word-level model; ASS generator with presets (karaoke highlight, per-word color, outline, position, animation) |
| Filler / silence removal | transcript word flags (um, uh, like, "yani", "şey"…, per-language list) + ffmpeg `silencedetect`; produces cut list |
| Click-a-word-to-delete | transcript editor: deleting words edits an EDL (cut ranges, crossfade-safe boundaries); render uses kept ranges; zero-duration word fixups |
| Smart reframing 16:9 → 9:16 | per-clip crop path: face/speaker detection (feature `vision`, ONNX face model via `ort`/`tract`) → smoothed crop keypoints; fallback center / `cropdetect`; render with `crop`+`scale` expressions |
| Virality score | LLM rubric (hook strength, emotion, novelty, clarity, pacing, shareability, platform fit) → 0–100 + per-criterion reasons; calibrated prompt, cached by clip hash |
| B-roll + templates | keyword extraction per sentence (LLM) → stock providers (Pexels, Pixabay: user key; local folder; generated images optional) → overlay track in EDL; JSON templates (caption style + layout + intro/outro + music ducking) |
| Social posting | LLM titles/descriptions/hashtags per platform; publish connectors as hk-integrations nodes (YouTube Data API first; others behind user-supplied tokens) so n8n flows can automate; manual "copy / export package" always available |
| Fine editing | optional external editor: **Concat** (https://github.com/jub0t/concat) via `concat --mcp` / JSON-RPC; "Open in Concat" + agent tool bridge |

## Concat license decision

Concat is **AGPL-3.0-or-later**. Hokkabaz license is deferred (DESIGN D10). We therefore never vendor or link Concat: it is an optional, user-installed external program driven over MCP/JSON-RPC (process boundary). Core pipeline (`hk-media`, `hk-stt`, `hk-studio`) is our own code on ffmpeg (process) + permissive crates.

## Architecture

```
Flutter /studio  ──FFI──►  hk-ffi api/studio.rs
  media bin | player + transcript editor | timeline | clips (scores) | agent chat | export queue
                              │
                  hk-studio (project model = EDL, highlight/score/reframe/caption/broll/social pipeline, studio agent tools)
                    ├─ hk-stt     (Transcriber trait: openai-compat HTTP, whisper-rs; transcript+word model)
                    ├─ hk-media   (ffmpeg/ffprobe process wrapper: probe, audio extract, cut/concat, render filtergraph, thumbs, silencedetect, progress)
                    ├─ hk-llm     (own APIs / local)
                    └─ hk-integrations (stock B-roll, publishers) + hk-flow nodes (`hokkabaz.studio*`)
```

Project files: `<data>/studio/<project-id>/{project.json, transcript.json, proxies/, renders/}`. Everything model-visible is logged like chat sessions.

## Platform notes

- Desktop: ffmpeg/ffprobe found on PATH or configured; first-run check with install hint. Hardware encode flags auto-probed.
- Android: ffmpeg process not available → render via remote daemon (`hokkabazd` with ffmpeg) or bundled ffmpeg-kit later; phone UI works as remote controller by default.
- Player: Flutter `media_kit` (libmpv) if available in pub cache, else `video_player`; proxies (low-res) for scrubbing.
- Docker image for hokkabazd gains ffmpeg (optional build arg).

## Risks

| Risk | Mitigation |
|---|---|
| Face tracking quality / model licensing | Feature-gated; use permissively-licensed ONNX face model (check license before bundling), fallback to center crop |
| whisper.cpp C++ build weight | `whisper` feature off by default; HTTP transcriber is the default path |
| Long videos (2h+) cost | chunked transcription, cached results, proxy files, cheap-model first pass then rerank |
| Platform publishing APIs need app review | Export package + manual copy always works; connectors are optional |
| Virality score is a heuristic | Label as estimate; show reasons; allow custom rubric prompt |
