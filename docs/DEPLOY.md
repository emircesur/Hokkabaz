# Running a headless instance (`hokkabazd`)

A headless instance runs flows (webhooks, schedules, durable waits), serves the agent API and pairs with the
desktop and phone apps (Instances tab). Pick one:

| Where | How |
|---|---|
| Docker / Compose | `Dockerfile`, `docker-compose.example.yml` |
| Linux server | `deploy/hokkabazd.service` (systemd, hardened) |
| Railway / PaaS | the same Dockerfile, volume on `/data` |
| Raspberry Pi | native aarch64 build, or the Docker image on a 64-bit OS |

First start creates the API tokens. `hokkabazd token` prints one; `hokkabazd pair` prints a QR for the mobile app
(needs `HOKKABAZ_PUBLIC_URL` to be reachable from the phone). Never expose port 7878 without TLS in front.

## Settings

| Variable | Meaning |
|---|---|
| `HOKKABAZ_HOME` | data dir (db, sessions, secrets). Back this up. |
| `HOKKABAZ_LISTEN` | bind address, default `127.0.0.1:7878`. If unset and `$PORT` is set, binds `0.0.0.0:$PORT`. |
| `HOKKABAZ_PUBLIC_URL` | URL clients reach it at (webhook URLs, pairing QR) |
| `HOKKABAZ_ADMIN_TOKEN_FILE` | pin the admin token (min 24 chars) instead of generating one |
| `HOKKABAZ_ENV_ALLOWLIST` | host env vars workflows may read via `$env` (default none) |
| `HOKKABAZ_ALLOW_PYTHON=1` | allow the Code node's Python mode (unsandboxed `python3`; needs Python in the image) |
| `DEEPSEEK_API_KEY` | default model provider key |

## Docker

```sh
cp docker-compose.example.yml docker-compose.yml     # set HOKKABAZ_PUBLIC_URL, secrets
docker compose up -d
docker compose exec hokkabazd hokkabazd token
```

The image is distroless, non-root, read-only root fs, `/data` is the only writable path.

## systemd

See the header of `deploy/hokkabazd.service` (user, binary, env file). `ReadWritePaths` is the only place the agent can write;
widen it deliberately for workspaces it should edit.

## Railway

1. New project > Deploy from GitHub repo > pick the fork (Railway uses the root `Dockerfile`).
2. Add a **volume** mounted at `/data`.
3. Variables: `HOKKABAZ_PUBLIC_URL=https://<your-service>.up.railway.app`, `DEEPSEEK_API_KEY`, and
   `HOKKABAZ_ADMIN_TOKEN_FILE` is not needed: read the generated token with `railway run hokkabazd token`
   or set a long `HOKKABAZ_ADMIN_TOKEN` yourself. The service binds `$PORT` automatically.
4. Railway terminates TLS, so the public URL is already `https`.
5. In the app: Instances > Add instance > the URL and token (or `hokkabazd pair` from `railway shell`).

## Raspberry Pi

* 64-bit Raspberry Pi OS: install the Docker image (`docker build -t hokkabazd .` takes a while on a Pi 4; build on a PC with
  `docker buildx build --platform linux/arm64 ...` and push to a registry).
* Native: `sudo apt install build-essential pkg-config`, install Rust, then
  `cargo build --release --locked -p hk-daemon` and follow the systemd steps. Expect 20-40 minutes on a Pi 4.
* A Pi 4/5 with 4 GB+ also runs small on-device models, but the headless daemon does not load them: use a provider
  (or Ollama on the LAN) for the daemon and the desktop/phone app for on-device inference.

## Model providers on the instance

`hokkabazd` serves every provider the app does. Set a key in the environment and the provider is enabled on first start:
`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, `DEEPSEEK_API_KEY`, `OPENROUTER_API_KEY`, `GROQ_API_KEY`,
`MISTRAL_API_KEY`, `XAI_API_KEY`, `TOGETHER_API_KEY`, `FIREWORKS_API_KEY`, `CEREBRAS_API_KEY`; `OLLAMA_HOST` points at Ollama.
Anything else (LM Studio, vLLM, llama.cpp server, private gateways) is added from the app: Models > Add endpoint, on a
connected instance too (admin token). `HOKKABAZ_MODEL=provider/model` sets the default model. Build with
`--features local-llm` to run GGUF models inside the daemon (`hokkabazd models pull <id>`).
Local transcription needs the `hk-whisper` helper next to `hokkabazd` (`Dockerfile.studio` includes it).

GPU: build with `--features local-llm-vulkan` (or `local-llm-cuda`) and `hk-whisper` with `--features vulkan` (or `cuda`).
Local models offload every layer by default; `HOKKABAZ_GPU=0` turns it off, `HOKKABAZ_GPU_DEVICE=N` picks the card,
`HOKKABAZ_GPU_LAYERS=N` caps the layers (models that do not fit load on the CPU). Whisper's GPU switch is in the Studio
settings. In the app the same switches are under Models > On-device models.
