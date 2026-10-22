# Hokkabaz

Native, open-source AI agent harness + automation workbench. Rust core, Flutter UI (desktop + Android), `hk` CLI/TUI.

- Any model: DeepSeek (default), OpenAI-compatible, Anthropic, Gemini, Ollama, llama.cpp, LM Studio, …
- Agentic chat with tools, subagents, approvals, MCP
- n8n-style automation flows with webhooks, cron, agent nodes
- Headless instances (`hokkabazd`) controlled from desktop or phone
- WASM plugins + built-in open-source integrations

Built on ideas from [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) and [OpenCode](https://github.com/anomalyco/opencode).

Status: working builds for Windows, Linux, macOS and Android. Docs: [user guide](docs/USER_GUIDE.md), [deploying](docs/DEPLOY.md), [plugins](docs/PLUGINS.md), [studio](docs/STUDIO.md), [local models](docs/LOCAL_MODELS.md), [n8n link](docs/N8N_SYNC.md), [security](docs/SECURITY.md). Design: [DESIGN.md](DESIGN.md), plan: [PLAN.md](PLAN.md), agent rules: [CLAUDE.md](CLAUDE.md).


## Cloning

Concat (the included video editor engine) is a git submodule. Clone with `git clone --recurse-submodules`, or run `git submodule update --init` afterwards; Cargo needs `third_party/concat` on disk to resolve the workspace.
