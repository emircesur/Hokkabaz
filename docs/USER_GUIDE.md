# Hokkabaz user guide

Hokkabaz is an AI agent workbench with n8n-compatible automation flows and a video studio. It runs on your device
(Windows, Linux, macOS, Android) and can connect to headless instances you host yourself.

## Install

Download from the GitHub Releases page: Windows (`-setup.exe` or `.zip`), Linux (`.deb`, `.AppImage`, `.tar.gz`; needs
`libmpv` for video preview and `ffmpeg` for the Studio), macOS (`.dmg`, unsigned: right-click > Open), Android (`.apk`,
arm64 for phones). Settings > *Check for updates* tells you when a newer release exists; nothing installs automatically.

## First run

1. **Models** tab: enable a provider and paste its API key (DeepSeek is on by default). Keys are stored encrypted on the
   device and never shown again. No key? Download an on-device model (see below).
2. **Chat**: new chat, type a task. The agent can read and edit files in the workspace folder, run commands and search the
   web. Risky tool calls (shell, writes, web, MCP, plugins) ask first: approve once, always for the session, or deny. Plan
   mode lets the agent look but not change anything.
3. Android: allow notifications so approval requests reach you while the app is in the background.

## Flows

The **Flows** tab is an n8n-compatible canvas: add nodes, connect them, run a test, inspect each node's input/output
(Runs tab keeps history). Import any n8n workflow JSON (clipboard or file); unsupported nodes stay in the flow and
can run on your own n8n (Plugins > n8n). Credentials live in Plugins > Credentials; nodes pick them in the node panel.
Webhook and schedule triggers fire on a **headless instance** (Instances tab); on a phone, flows run when you start them.

## Studio

Import a video; the Studio transcribes it, finds highlight moments with a virality score, cuts silences and filler words
(click a word to delete it), reframes to 9:16 following the speaker, adds captions, B-roll and templates, and drafts the
social post. The agent can drive the same project live. Details: `STUDIO.md`.

## Extending

* **Plugins** (WASM, sandboxed, capability list shown before install), **MCP servers** (trust prompt), **integrations**,
  **credentials**: `PLUGINS.md`.
* **On-device models**: Models tab > *On-device models*. Download, pick an offline fallback: `LOCAL_MODELS.md`.
* **n8n link**: import/push workflows, delegate unsupported nodes: `N8N_SYNC.md`.

## Instances

*This device* is always there. Add a headless instance (URL + token, or scan its pairing QR) to run chats and flows on
your server; the app switches all tabs to it. Hosting guide: `DEPLOY.md`.

## Command line

`hk` (chat, TUI, sessions, `hk plugin ...`) and `hokkabazd` ship with the source build: `cargo build --release -p hk-cli -p hk-daemon`.

## Privacy

Hokkabaz sends prompts only to the providers you configure (or nowhere, with on-device models). No telemetry. See
`SECURITY.md` for what is protected and what is not.

## Troubleshooting

* *Studio says ffmpeg is missing*: install ffmpeg and restart (Linux: `apt install ffmpeg libmpv2`).
* *A provider shows an error badge*: open its card in Models; the message says whether the key, URL or network is wrong.
* *Python in the Code node is off*: it runs an unsandboxed `python3`; set `HOKKABAZ_ALLOW_PYTHON=1` where the flow runs.
* *Download of a local model stops*: run it again; it resumes where it left off.
