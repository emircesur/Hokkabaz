# Third-party notices

Hokkabaz ports concepts (not vendored source) from the projects below. Both are MIT-licensed.

| Project | Source | License | Use |
|---|---|---|---|
| deepseek-harness (`dsh`) | https://github.com/deepseek-ai/deepseek-harness | MIT | Primary upstream: agent loop, session log, tool pipeline semantics. Pinned SHA in `docs/UPSTREAM.md`. |
| opencode | https://github.com/anomalyco/opencode | MIT | Secondary upstream: provider catalog, modes, TUI concepts. Pinned SHA in `docs/UPSTREAM.md`. |

Original copyright notices and MIT license texts for each project must be reproduced here before first release.
Rust dependency licenses: generate with `cargo about` / `cargo deny` in CI (P0.2).

## Included (not just concepts)

| Project | Source | License | Use |
|---|---|---|---|
| Concat | https://github.com/jub0t/concat (submodule `third_party/concat`, SHA in `docs/UPSTREAM.md`) | AGPL-3.0-or-later | Video editing/render engine and MCP/JSON-RPC server used by Studio. Name and logo are trademarks of the Concat author (TRADEMARK.md); Hokkabaz is AGPL-3.0-or-later because of this inclusion. |

## Linked libraries and downloadable assets

| Project | Source | License | Use |
|---|---|---|---|
| llama.cpp (via `llama-cpp-2`) | https://github.com/ggml-org/llama.cpp | MIT | On-device inference (`hk-local`). |
| wasmtime | https://github.com/bytecodealliance/wasmtime | Apache-2.0 WITH LLVM-exception | WASM plugin runtime (`hk-plugin`, desktop). |
| whisper.cpp (via `whisper-rs`, optional feature) | https://github.com/ggml-org/whisper.cpp | MIT | Local speech-to-text (`hk-stt`, off by default). |
| rustface + SeetaFace model | https://github.com/atomashpolskiy/rustface | BSD-2-Clause | Face tracking for 9:16 reframing (`hk-studio`). |
| QuickJS (via `rquickjs`) | https://bellard.org/quickjs/ | MIT | Expression and Code-node engine (`hk-flow`). |
| scraper, pulldown-cmark, quick-xml | crates.io | MIT / Apache-2.0 | HTML, Markdown and XML nodes (`hk-flow`). |
| media_kit | https://github.com/media-kit/media-kit | MIT | Video preview on Linux/Windows (links libmpv, LGPL/GPL, loaded dynamically). |
| FFmpeg | https://ffmpeg.org | LGPL-2.1+ / GPL-2.0+ | Not shipped: Hokkabaz runs the `ffmpeg` found on the system. |

Models offered in the on-device catalog are downloaded by the user from HuggingFace and keep their own licenses, shown next
to each model in the app: Qwen2.5 (Apache-2.0; the 3B model is under the Qwen Research License), Llama 3.2 (Llama 3.2
Community License), Gemma 2 (Gemma Terms of Use). Hokkabaz does not redistribute them.
