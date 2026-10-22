# Concat integration (`hk-concat`)

Concat (https://github.com/jub0t/concat, Rust, AGPL-3.0-or-later, beta) is the included video editor behind Hokkabaz Studio's fine editing and rich render (DESIGN D16, `docs/STUDIO.md`). Source: git submodule `third_party/concat` (pinned SHA in `docs/UPSTREAM.md`). Crate `crates/hk-concat` bridges it two ways behind one trait.

```
Studio agent / hk-ffi / CLI
        |  concat_* tools  (hk-concat::agent_tools, re-exported by hk-server feature `concat`)
        v
   ConcatClient  (typed: projects, media, cuts, titles/captions, keyframes, effects, frames, export)
        v
   trait ConcatBackend  --  call_tool(name, json args) -> json   (Concat's own MCP tool names)
      |                         |
 ProcessBackend            EngineBackend   (cargo feature `engine`, off by default, UNVERIFIED)
 `concat --mcp` child      Concat crates in-process (same Toolbox + headless Api as `concat --mcp`)
```

## Install Concat (process bridge)

The bridge needs the `concat` executable; it never bundles it.

| System | Install | Binary the bridge finds |
|---|---|---|
| Linux | `.deb` / `.rpm` / Arch package from https://github.com/jub0t/Concat/releases | `/usr/bin/concat` |
| Linux (AppImage) | download, `chmod +x` (needs FUSE) | `~/Applications/Concat*.AppImage` (or set the path) |
| macOS | `.dmg` from releases | `/Applications/Concat.app/Contents/MacOS/concat` |
| Windows | setup or `.msi` | `%LOCALAPPDATA%\Programs\Concat\concat.exe`, `C:\Program Files\Concat\concat.exe` |
| From source | `git submodule update --init`, `cargo build --release -p concat` in `third_party/concat/src` (Rust 1.93, FFmpeg 7 dev libs, cmake, C++) | `third_party/concat/src/target/release/concat` (set the path) |

Android has no Concat binary: the bridge compiles there but every spawn returns `ConcatError::Unsupported`. Use a remote `hokkabazd` on desktop instead.

### Lookup order

1. `[concat] path` in config
2. env `HK_CONCAT`
3. `PATH` (`concat`)
4. the table above (incl. `~/.local/bin`, `~/bin`, AppImages in `~/Applications`, `~/Downloads`)

A configured path (1 or 2) that is not an executable file is an error, never a silent fall-through. If nothing is found the error lists everything searched.

## Config (`[concat]`)

```toml
[concat]
path = "/usr/bin/concat"     # optional
backend = "process"          # process | engine
mode = "auto"                # auto | headless | window   (concat --mcp <mode>)
roots = ["/data/videos"]     # extra write roots -> CONCAT_MCP_ROOTS (REPLACES the $HOME default)
api_address = "127.0.0.1:7420"   # reach a running Concat server instead
api_token = "..."                # resolved secret (hk-store), never logged
timeout_secs = 300
```

`hk_concat::ConcatConfig` (serde) is the type; wiring it into the `hk-core` config file is the config owner's job.

## How the process bridge works

- Spawns `concat --mcp <mode>` with a cleared environment (allowlist: PATH, HOME, XDG_*, APPDATA/LOCALAPPDATA, ... plus `CONCAT_MCP_ROOTS`, `CONCAT_API_ADDRESS`, `CONCAT_API_TOKEN` from config). Host secrets are not inherited. stderr is kept as a bounded tail for error messages.
- Speaks MCP over stdio (newline JSON-RPC 2.0, `initialize` handshake of the 2025 era; `concat-mcp/src/protocol.rs`). A minimal client lives in `rpc.rs` instead of `hk-mcp`/rmcp because `hk-mcp::call_tool` flattens replies to text (drops images and `structuredContent`) and because a generic stream client is unit-testable over `tokio::io::duplex` with no binary. A `concat` entry in the hk-mcp server config still works if you only want Concat's raw tools in the chat (`mcp.concat.*`); the typed tools below are the Studio path.
- **Modes** (Concat docs/MCP.md): `window` goes through the open Concat window (Settings > Automation > Scripts and apps; reads Concat's `automation.json`); the agent cannot open the project on screen. `headless` runs Concat's engine inside the `--mcp` process; a project open in the window is refused. `auto` (default) = window if reachable, else headless.
- Edits live in memory until `project_save`. One export at a time. Concat serves one request at a time, so `export_wait` polls `export_status` (progress for the UI, and `export_cancel` stays possible).

### Roots restriction

Concat writes projects, exports and preview frames **only under the user's home folder**, or `CONCAT_MCP_ROOTS` (headless). Anything else is refused with `"<path> is outside where this API writes (<roots>)"`. The bridge:

- parses that into `ConcatError::OutsideRoots { path, roots }` whose text names the fix (use a path under the roots or add the folder to `[concat] roots`),
- pre-checks `project_create` location, `export_start` output and `get_frame` output locally when the roots are knowable (headless/auto without a remote server), so the model gets the error without a round trip. `..` is rejected like Concat does.
- Reading input media is not restricted by Concat; only writes are.

Studio must therefore keep project folders / renders under the home folder (or configure roots); `<data>/studio/...` outside home needs `[concat] roots`.

## Typed client and agent tools

`ConcatClient` methods map 1:1 to Concat tool names (verified against `concat-mcp/src/tools.rs`): `concat_version`, `project_list|create|open|save|close|set_video`, `timeline_read`, `project_get`, `media_probe|import`, `clip_add|trim|update|transform|speed|frame`, `clips_split|move|remove`, `track_add`, `title_add`, `captions_add`, `keyframe_set|clear`, `effects_list`, `effect_layer_add`, `edit_apply|undo|redo`, `preview_frame`, `export_start|status|cancel`. Composites: `cut_range` (delete a timeline span on all lanes with ripple), `export_wait` (poll + progress callback), `set_effects`/`set_transition` (via `clip_update`).

Agent tools (`hk_concat::concat_tools(session)`, 11 action-style tools; `hk-server --features concat` re-exports them for `AgentRuntime::add_tools`, same pattern as `flow_tools`):

| Tool | Actions | Risk |
|---|---|---|
| `concat_status` | version/mode/backend, install hint on failure | safe |
| `concat_project` | list, create, open, save, close, read, as_plan | create/save/close: ask |
| `concat_media` | probe, import | safe |
| `concat_cut` | add_clip, split, trim, delete, move, cut_range, speed, add_track, undo, redo | safe (in-memory, undoable) |
| `concat_text` | title, captions, captions_from_words | safe |
| `concat_keyframe` | set, clear, transform, frame, reframe | safe |
| `concat_effect` | list, set_effects, transition, layer | safe |
| `concat_preview` | PNG of the composited frame at a time | ask (writes a file) |
| `concat_export` | start, status, wait, cancel | start: ask |
| `concat_apply_plan` | whole `StudioClipPlan` -> project (`dry_run` lists the operations) | ask unless dry_run |
| `concat_raw` | any Concat tool by name | ask |

Chosen over a pass-through of Concat's ~45 tools: far fewer schema tokens, per-action risk for the approval gate, and composites Concat lacks. The editor process starts on first use (`ConcatSession`) and is respawned only if it died (not on timeouts/refusals: a respawn would drop unsaved in-memory edits).

## Studio EDL <-> Concat project (`hk_concat::plan`)

`StudioClipPlan { name, source, kept_ranges, captions{words, style}, crop_keyframes, overlays, video, framing }` is lowered by `to_concat_commands(plan)` to a flat `Vec<Command>` (`ImportMedia`, `PlaceRange`, `Frame`, `CropKeys`, `AddCaptions`, `AddTitle`, `AddMediaOverlay`); `apply_commands` / `apply_plan` run them against a client; `plan_from_view(project_get_json, source)` reads a project back.

- **Cut list**: each kept source range is placed as `clip_add` + one atomic `edit_apply` batch (head `clip_trim`, tail `clip_trim`, `clips_move`) at its cumulative output time. Touching ranges are merged. Round-trip is exact.
- **Captions**: words (source time) -> dropped if their midpoint is in a cut region -> output time -> grouped into lines (max words/chars, max gap, never across a cut) -> one `captions_add` batch. Concat captions are single-style titles: no per-word karaoke highlight (use `max_words_per_line = 1` for word-pop). Reading back spreads each line's words evenly (timings are approximate) and classifies text clips at `offsetY >= 0.15` as captions, others as titles.
- **Reframe**: `CropKeyframe { time, center_x, center_y, zoom }` (subject position as a fraction of the source frame) -> Concat `scale/offsetX/offsetY` keyframes via `crop_to_transform` (scale 1 = fit; fill scale and offset clamping follow Concat's `framing_scale`). Edge keys are synthesised per kept range so motion is continuous across cuts. Without keyframes the clips get `clip_frame fill`.
- **Overlays**: titles via `title_add`; b-roll on its own new top lane, trimmed to length, with opacity/transform.
- Not carried by the plan: effects, transitions, speed ramps, audio edits (edit these in Concat; `as_plan` ignores them).

## In-process engine (cargo feature `engine`, off)

`hk-concat/engine` adds path deps on `concat-api`, `concat-mcp`, `concat-project`, `concat-export`, `concat-effects`, `concat-text`, `concat-speech`, `concat-vision` (all `third_party/concat/src/crates/*`) and `EngineBackend`, which runs Concat's own `Toolbox` over a headless `Api` on a worker thread, so tool names/semantics equal the process bridge's. Requirements (from Concat's README): Rust >= 1.93, FFmpeg 7 dev libs (`libavcodec libavformat libavutil libswscale libswresample libavfilter libavdevice`), cmake, a C++ toolchain, ONNX Runtime download at build time. **Status: written against Concat's source, not compiled in the dev container** (no FFmpeg 7). The CI job `studio.yml` is the first place that builds it. Day-to-day work does not need it. Concat's workspace patches (`[patch.crates-io]` for slint/whisper-rs-sys) do not apply when its crates are path deps of our workspace; if the engine build needs them, add the same `[patch]` to our root `Cargo.toml`.

## License and trademark

- Hokkabaz is AGPL-3.0-or-later (D10) because Concat is linked (engine) and bundled (process). Source offer for network use applies to the daemon (AGPL section 13).
- Concat's name and logo are trademarked (`third_party/concat/TRADEMARK.md`). Say "powered by / compatible with Concat" (nominative use). **Never name a modified Concat build "Concat"** (nor use its logo as our icon): if we patch the submodule or ship a modified in-process engine as a product, call the editor something else. Unmodified official builds may keep the name.
- Upstream contributions need Concat's CLA (`third_party/concat/CLA.md`).
- Keep `THIRD_PARTY_NOTICES.md` and the pinned SHA in `docs/UPSTREAM.md` current when bumping the submodule.

## Tests

`cargo test -p hk-concat` uses an in-memory fake `concat --mcp` over `tokio::io::duplex` (`src/fake.rs`) that models Concat's documented edit semantics; no binary needed. It validates the bridge's sequencing and the plan round trip against our reading of Concat's contract, **not** against the real editor. Smoke test against a real install: `HK_CONCAT=/path/to/concat` then run the `concat_status` tool, or `concat --mcp headless` with any MCP client.
