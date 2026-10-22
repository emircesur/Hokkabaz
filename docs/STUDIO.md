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
| Fine editing / rich render | **Concat** included (see below): engine + MCP bridge, "Open in Concat", agent tool bridge |

## Concat (included)

Concat (https://github.com/jub0t/concat, Rust, AGPL-3.0-or-later, beta) is part of Hokkabaz: git submodule `third_party/concat` pinned to a SHA (see `docs/UPSTREAM.md`), crate `hk-concat` bridges it.

- **Process bridge (always available):** `concat --mcp` (headless or via window automation) and its JSON-RPC API, driven through hk-mcp; used by the Studio agent as an editing backend (projects, cuts, titles, captions, keyframes, effects, frame preview, export) and for "Open in Concat" fine editing.
- **In-process engine (cargo feature `engine`, off by default):** path deps on `concat-api`, `concat-project`, `concat-export`, `concat-effects`, `concat-text`, `concat-speech`, `concat-vision`. Needs FFmpeg 7 dev libs, cmake, C++, Rust ≥ 1.93 (see third_party/concat README). Built in desktop release CI, not required for day-to-day work.
- **License consequence:** Hokkabaz is AGPL-3.0-or-later (DESIGN D10). Concat's name/logo are trademarked (`third_party/concat/TRADEMARK.md`): we describe it nominatively ("powered by / compatible with Concat"); modified Concat builds must not be named Concat. Upstream contributions need its CLA.
- **Division of labour:** own pipeline (`hk-media` ffmpeg process, `hk-stt`, `hk-studio` EDL/highlights/virality/social) stays headless and Android-remote-friendly; Concat is the rich editor/render engine on desktop. `hk-studio` EDL maps to a Concat project (export/import adapter) so one project can use either backend.

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


## Status and how to use it

Implemented (see PLAN.md Phase 8 for per-task notes):

| Area | State |
|---|---|
| Import, transcript (word-level), filler / pause detection | working; STT = OpenAI-compatible HTTP (OpenAI, Groq, local server) or local whisper.cpp (feature `whisper`, untested without a model) |
| Highlights + virality score | working with any `hk-llm` provider; scored by a fixed 7-criterion rubric, an estimate not a prediction |
| Text-based editing | click / select / delete words; fillers and pauses switches; undo / redo; preview plays the edit instantly (no render) |
| Captions | karaoke / word-by-word / line presets, colours, size, position; burned in with ASS |
| Smart reframing | 9:16 or 1:1 crop path following a face (SeetaFace, model downloaded once) or moving subject; camera dead zone + smoothing |
| B-roll | LLM picks moments; Pexels, Pixabay or your own folder; still images preview live, video B-roll is composited at render |
| Social copy | per-platform titles / descriptions / tags within limits |
| Publish | YouTube Shorts upload (your OAuth refresh token, private by default); other platforms: export package |
| Agent | `studio_*` tools (projects, transcribe, transcript, highlights, edit, reframe, b-roll, social, render, export, publish); edits appear live in the editor, tagged "Agent edited" |
| Concat | `hk-concat` bridge + `concat_*` tools (docs/CONCAT.md) |

Setup (Studio settings in the app):

1. Install **ffmpeg** (6+) and make sure `ffmpeg` / `ffprobe` are on `PATH`, or set `HK_FFMPEG` / `HK_FFPROBE`.
2. Models tab: add a provider, then pick the Studio model (`provider/model`) in Studio settings.
3. Speech to text: base URL + model (+ key) of any OpenAI-compatible transcription endpoint.
4. Optional: Pexels / Pixabay keys or a folder of your own footage; YouTube OAuth client + refresh token; "Download face model".
5. Linux desktop playback uses libmpv (`libmpv-dev` / `mpv-libs`); Windows bundles it; Android uses the system player.

Known gaps: Studio runs only where ffmpeg runs (desktop, or a daemon) - on Android the screen works but import / render fail until a remote Studio host is connected (not built yet); no speaker diarization (largest / nearest face is followed); TikTok / Instagram direct publishing; n8n nodes for Studio.

## From flows

Flows can drive the Studio with the **Video Studio** node (`hokkabaz.studio`, this device only): pick a step
(transcribe, highlights, edit, reframe, b-roll, social, render, export, publish) and give its arguments as JSON
(expressions work per item). It runs the same `studio_*` tool the agent uses, so flow runs and the live editor stay consistent.
