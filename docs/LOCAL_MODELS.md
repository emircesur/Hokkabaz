# On-device models

Models run on this device through llama.cpp (`hk-local`, crate feature `llama`). Nothing leaves the device.

## Use

Models tab > *On-device models*:

* **Download**: curated Q4_K_M GGUF models from HuggingFace (Qwen2.5, Llama 3.2, Gemma 2). Downloads resume after a
  dropped connection and are verified against the SHA-256 HF publishes; a file that fails the check is deleted.
* **Fit**: each model shows whether it runs well, is tight on RAM, or is too large for this device
  (weights + ~25% for cache and runtime must stay under ~55% / 80% of total RAM). The recommended model is the largest
  that fits comfortably.
* **Add GGUF file**: use any GGUF already on disk (used in place, not copied).
* **Offline fallback**: pick an installed model; when your default model's provider is unreachable, rate limited or
  otherwise failing before the first token, the router continues on the on-device model (chain
  `<default model> -> local/<fallback>`). It does not replace a model you pick explicitly.

Select a model by hand as `local/<id>` (e.g. `local/qwen2.5-1.5b`) like any provider model.

## How it behaves

* One model stays loaded (the last used); a request builds its own context (2048 tokens on <=4 GB devices, 4096 up to
  8 GB, 8192 above; never more than the model was trained for). Prompts that do not fit fail with a clear context error
  instead of truncating silently.
* Tools: llama.cpp's template engine cannot render tool schemas, so tools are described in the system prompt and the
  model answers with `<tool_call>{"name":..., "arguments":{...}}</tool_call>` (Hermes/Qwen convention). The stream parser
  turns those into normal tool calls. Qwen2.5 1.5B+ follows this reliably; 0.5B works for simple single calls.
* Cancelling a turn stops generation between tokens.

## Build

`cargo build -p hk-local --features llama` needs cmake, a C++ toolchain and libclang (bindgen). `hk-ffi` enables it for
desktop targets. Verify with a real model:

```sh
HK_LOCAL_GGUF=/path/qwen2.5-0.5b-instruct-q4_k_m.gguf \
  cargo test -p hk-local --features llama --test engine_live -- --nocapture
```

Updating the catalog: take the file size and SHA-256 from the HF headers (`x-linked-size`, `x-linked-etag`) and keep the
list sorted smallest first (`catalog::tests` checks it).
