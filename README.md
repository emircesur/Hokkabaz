# Hokkabaz

Native, open-source AI agent harness + automation workbench. Rust core, Flutter UI (desktop + Android), `hk` CLI/TUI.

- Any model: DeepSeek (default), OpenAI-compatible, Anthropic, Gemini, Ollama, llama.cpp, LM Studio, …
- Agentic chat with tools, subagents, approvals, MCP
- n8n-style automation flows with webhooks, cron, agent nodes
- Headless instances (`hokkabazd`) controlled from desktop or phone
- WASM plugins + built-in open-source integrations
- Studio: transcript editing, AI-ranked shorts, animated captions, reframing, B-roll and Concat editing

Built on ideas from [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) and [OpenCode](https://github.com/anomalyco/opencode).

Status: active development. See [DESIGN.md](DESIGN.md), [PLAN.md](PLAN.md), [CLAUDE.md](CLAUDE.md), and [Studio setup](docs/STUDIO.md#development-and-connections).
