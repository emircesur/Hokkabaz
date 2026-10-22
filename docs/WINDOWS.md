# Hokkabaz for Windows x64

Extract the entire ZIP into a writable folder, then open `hokkabaz.exe`. Windows
10/11 x64 is required. Keep `data`, the DLLs, `concat` and `tools` alongside it.
The portable package includes Concat 0.2.8 and FFmpeg; no separate editor or
FFmpeg installation is necessary. Builds are currently unsigned.

Open **Studio**, import a video, then select **Shorts** or **Full editor**.

- **Shorts** previews the edited timeline live: word/silence cuts, animated
  captions, crop, B-roll and ducked music. Changes reload the preview without
  waiting for an export. Playback time is edited time; transcript seeking maps
  source timestamps into that timeline.
- **Full editor** places the real Concat UI inside Hokkabaz. All the upstream
  editor controls, multitrack timelines, effects, keyframes, transitions, audio,
  speech/vision panels and live GPU preview remain available. Download model
  weights through Concat when a feature needs them.
- Save native editor changes with **Ctrl+S**. Switching to Shorts or another
  Hokkabaz screen keeps the editor alive, including unsaved changes. Reopening
  the same project restores that surface. **Close project** uses Concat's close
  flow. Close it before switching projects.
- **Apply latest Shorts edits** creates a new Concat project from the current
  short-form edit. Existing manual editor projects remain on disk. Manual
  Concat changes are not converted back into transcript operations. Export
  manual timeline changes using Concat's own Export panel.

Choose models/API keys in Models and Studio Connections. Analysis, cloud speech,
stock and publishing require your own configured services/keys. Local speech
and vision models are not bundled. Remote instances handle short-form work;
embedded native editing requires the local instance and local media.

## Rebuild

On Windows with Visual Studio's Desktop development with C++ workload, Rust
stable and Flutter 3.47.7:

```powershell
git submodule update --init --recursive
cd apps/flutter
flutter pub get --enforce-lockfile
flutter build windows --release
cd ../..
./scripts/package-windows.ps1
```

The Windows Actions workflow builds the same portable ZIP, verifies packaged
executables and checks that Hokkabaz creates a window. An Actions artifact is
uploaded. Builds on `hk/embedded-concat-studio` also deliver checksum-verified
ZIP chunks on a separate `builds/windows-x64-<source SHA>` Git branch for cloud
sessions with Git-only access. Those branches contain build output, not source.
