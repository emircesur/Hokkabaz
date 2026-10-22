# Android build

- Target ABIs: arm64-v8a (aarch64-linux-android), x86_64, armeabi-v7a (cargokit only).
- `hk-ffi` is the only Rust crate on the Android path. Its tree must not contain landlock,
  hk-acp, hk-sandbox, wasmtime, portable-pty, axum. (`hk-mcp`/`rmcp` are in, HTTP transport only; `hk-plugin` is in
  without its `wasm-plugins` feature, so plugins can be listed but not run; `hk-local` carries llama.cpp, linked with a static libc++.)
  `hk-server` is pulled with `default-features = false, features = ["runtime"]`; `portable-pty`
  in `hk-tools` is `cfg(not(target_os = "android"))`. CI job `audit` in
  `.github/workflows/android.yml` enforces this via `cargo tree --target <triple>`.
- Local check: `ANDROID_NDK_HOME=<ndk> cargo ndk -t arm64-v8a check -p hk-ffi`.
- Size-tuned build: `cargo ndk -t arm64-v8a -o out/jniLibs build -p hk-ffi --profile mobile`
  (`[profile.mobile]` in root `Cargo.toml`: opt-level s, thin LTO, 1 CGU, stripped, unwind kept
  so frb can catch panics). Output lands in `target/<triple>/mobile/`.
- Flutter/cargokit hardcodes `cargo build --release` (vendored, not patched), so `--profile mobile`
  is not selectable there. CI mirrors the profile via `CARGO_PROFILE_RELEASE_*` env vars
  (see `MOBILE_PROFILE_ENV` in `android.yml`); export the same locally before
  `flutter build apk --release` to get the small .so.
