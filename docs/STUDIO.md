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

## Development and connections

The `/studio` screen is implemented in `apps/flutter/lib/screens/studio`. Its Rust backend is
`hk-studio`, shared through FFI and authenticated `POST /api/v1/studio/{operation}` requests.
The existing Instances screen selects the local or remote backend. File paths on a remote
instance refer to that instance's filesystem; remote scrubbing uses authenticated frame requests.

Implemented: persistent projects and render history, optimistic edit revisions, undo/redo,
word deletion/restoration, filler and silence cuts, word-level ASS karaoke, SRT/VTT export,
12 caption presets, smoothed crop paths, B-roll overlays, voice-ducked music, render progress
and cancellation, Concat project handoff, and guarded Studio agent tools. `hokkabaz.studio*`
flow nodes expose the same pipeline, including manual opt-in YouTube uploads.

In **Studio → Connections**:

- Set an analysis `provider/model` already configured in Models (including Ollama and custom
  OpenAI-compatible/local network providers). Direct UI analysis also uses the local default
  model when no Studio override is set. Analysis and scoring use cached, logged transcript
  requests; reach scores are estimates with reasons.
- Speech defaults to a keyless local OpenAI-compatible Whisper server at
  `http://127.0.0.1:8080/v1`. Change its URL/model for your own speech API. API keys go into
  the shared secret backend, never project JSON. Audio is uploaded only when Transcribe runs.
- In-process whisper.cpp is available with `hk-studio/whisper` (and the forwarding `whisper`
  features on `hk-ffi`/`hk-daemon`). Model weights are user-supplied; none are downloaded by setup.
- Pexels and Pixabay use their own keys. Local B-roll needs no stock account. Keep attribution
  returned with an asset; the export package includes it.
- A subject detector is optional. Studio sends a sampled frame as multipart field `image` to
  the configured URL. Return `{"subjects":[{"x":0.5,"y":0.4,"confidence":0.9}],"scene_cut":false}`
  with normalized coordinates. The highest-confidence subject drives a smoothed crop path.
  Without a detector, Studio uses center crop. A bundled ONNX detector remains future work.
- YouTube uploads need an OAuth access token with `youtube.upload` scope. Metadata is editable
  per clip. The UI uploads privately only after confirmation; expired tokens must be replaced.
  TikTok, Instagram and LinkedIn currently support metadata and export packages, not direct posting.

Linux prerequisites: Rust stable, Flutter/Dart matching `pubspec.yaml`, FFmpeg/ffprobe 7+ on PATH,
GTK 3 and libmpv development packages, Clang, CMake and Ninja. Set `HK_FFMPEG`, `HK_FFPROBE`
or `HK_CONCAT` to use explicit binaries. Concat 0.2.8's process bridge works independently of
the optional in-process engine; a source build of the full Concat engine is not required.

```sh
git submodule update --init --depth 1 third_party/concat
cargo check -p hk-studio -p hk-ffi -p hk-daemon
cd apps/flutter
flutter pub get --enforce-lockfile
flutter analyze --no-pub
flutter run -d linux
```

Regenerate FFI from the repository root with the pinned tool:
`RUST_LOG=warn flutter_rust_bridge_codegen generate` (version 2.11.1; requires cargo-expand).

Validation stays narrow: affected crates' check/Clippy, Flutter analysis/build, and one pipeline
smoke example. `cargo run -p hk-studio --example studio_smoke -- <4-second-video> <empty-scratch-dir>`
uses fixture speech/LLM replies, real FFmpeg, and the installed Concat MCP process. It checks
editing, undo, stale revisions, styled vertical output, export files and restart persistence.
Live model quality, third-party stock accounts, social uploads and Android packaging require
their own configured services/devices and were not exercised by that smoke check.


## Embedded full editor and live preview

The desktop Studio now has **Shorts** and **Full editor** modes. The full,
unmodified Concat 0.2.8 Slint interface is hosted as a native child surface on
Windows and Linux X11. This preserves the native multitrack editor, effects,
keyframes, transitions, audio tools, speech/vision panels and live compositor.
It runs in its own process with its own event loop; its window is inside the
Hokkabaz workspace, rather than a separate editor window.

Shorts playback uses an mpv EDL for frame-accurate cuts and the same media filter
graph used for export, at preview resolution. libass captions, crop tracking,
B-roll and music ducking are applied during playback. Edits coalesce, replace
the preview and preserve the playhead/play state. Transcript seeking maps source
time into edited time. Rendering remains a separate high-quality operation.

Native editor projects persist per imported project and selected clip. Reopening
preserves manual edits. Explicitly applying newer Shorts edits creates a new
native project and retains previous projects. Native timeline edits do not
round-trip into transcript operations; export them through Concat's Export panel.
Concat's upstream API still does not edit the visible native project live.
The Shorts agent changes the Shorts project and its live preview.

Linux embedding requires X11 (or an XWayland session launched with
`GDK_BACKEND=x11`). Windows packages include the complete upstream editor and
checksum-verified FFmpeg tools. See [Windows builds](WINDOWS.md).
