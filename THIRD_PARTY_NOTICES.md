# Third-party notices

Hokkabaz ports concepts (not vendored source) from the projects below. Both are MIT-licensed.

| Project | Source | License | Use |
|---|---|---|---|
| deepseek-harness (`dsh`) | https://github.com/deepseek-ai/deepseek-harness | MIT | Primary upstream: agent loop, session log, tool pipeline semantics. Pinned SHA in `docs/UPSTREAM.md`. |
| opencode | https://github.com/anomalyco/opencode | MIT | Secondary upstream: provider catalog, modes, TUI concepts. Pinned SHA in `docs/UPSTREAM.md`. |

Original copyright notices and MIT license texts for each project must be reproduced here before first release.
Rust dependency licenses: generate with `cargo about` / `cargo deny` in CI (P0.2).
