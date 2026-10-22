param([switch]$DependenciesOnly, [string]$Release = "apps/flutter/build/windows/x64/runner/Release", [string]$Output = "hokkabaz-windows-x64.zip")
$ErrorActionPreference = "Stop"
$stage = if ($DependenciesOnly) { Join-Path ([System.IO.Path]::GetTempPath()) "hokkabaz-dependency-check" } else { (Resolve-Path $Release).Path }
New-Item -ItemType Directory -Force $stage | Out-Null
$cache = Join-Path ([System.IO.Path]::GetTempPath()) "hokkabaz-windows-deps"
New-Item -ItemType Directory -Force $cache | Out-Null
function Fetch-Verified([string]$Url, [string]$File, [string]$Sha256) {
  if (!(Test-Path $File) -or (Get-FileHash $File -Algorithm SHA256).Hash.ToLowerInvariant() -ne $Sha256) {
    Invoke-WebRequest -Uri $Url -OutFile $File
  }
  if ((Get-FileHash $File -Algorithm SHA256).Hash.ToLowerInvariant() -ne $Sha256) { throw "Checksum mismatch: $File" }
}

# Complete upstream release: Slint editor, GPU compositor, speech/vision runtimes,
# effect catalogue and their DLLs. No user-installed Concat is required.
$msi = Join-Path $cache "Concat-0.2.8-windows-x86_64.msi"
Fetch-Verified "https://github.com/jub0t/Concat/releases/download/v0.2.8/Concat-0.2.8-windows-x86_64.msi" $msi "dc24b7588ec7fe430223ccb06cb7fa54027382e4b8867557bc5515710fa3af98"
$expanded = Join-Path $cache "concat-expanded"
if (!(Get-ChildItem $expanded -Filter concat.exe -Recurse -ErrorAction SilentlyContinue)) {
  $installer = Start-Process msiexec.exe -ArgumentList @('/a', "`"$msi`"", '/qn', "TARGETDIR=`"$expanded`"") -Wait -PassThru
  if ($installer.ExitCode -ne 0) { throw "Concat MSI extraction failed: $($installer.ExitCode)" }
}
$editor = Get-ChildItem $expanded -Filter concat.exe -Recurse | Select-Object -First 1
if (!$editor) { throw "Upstream installer contains no concat.exe" }
New-Item -ItemType Directory -Force "$stage/concat" | Out-Null
Copy-Item "$($editor.DirectoryName)/*" "$stage/concat" -Recurse -Force

# Reuse the submodule's pinned, checksum-verified FFmpeg build. Shared DLLs stay
# beside these executables, isolated from Concat's and media_kit's versions.
$dependency = Get-Content "third_party/concat/scripts/ci/deps.lock" | Where-Object { $_ -match '^ffmpeg-win64\s+' } | Select-Object -First 1
if (!$dependency) { throw "Pinned ffmpeg-win64 dependency is missing" }
$fields = $dependency -split '\s+'
$ffzip = Join-Path $cache "ffmpeg-win64.zip"
Fetch-Verified $fields[1] $ffzip $fields[2]
$ffdir = Join-Path $cache "ffmpeg-expanded"
if (!(Test-Path $ffdir)) { Expand-Archive $ffzip $ffdir }
$ffmpeg = Get-ChildItem $ffdir -Filter ffmpeg.exe -Recurse | Select-Object -First 1
if (!$ffmpeg) { throw "FFmpeg archive contains no ffmpeg.exe" }
New-Item -ItemType Directory -Force "$stage/tools" | Out-Null
Copy-Item "$($ffmpeg.DirectoryName)/*" "$stage/tools" -Recurse -Force
Get-ChildItem $ffdir -Filter '*LICENSE*' -Recurse | Copy-Item -Destination "$stage/tools" -Force
Copy-Item LICENSE, THIRD_PARTY_NOTICES.md $stage
Copy-Item docs/WINDOWS.md "$stage/README.md"
$sha = git rev-parse HEAD
@"
Hokkabaz source: https://github.com/emircesur/Hokkabaz/tree/$sha
Concat source (complete upstream UI): https://github.com/jub0t/Concat/tree/2756209a92766a4e4726999a51648349a272c159
FFmpeg source and build recipe: $($fields[1])
All Hokkabaz modifications and build instructions are in the Hokkabaz source above.
"@ | Set-Content "$stage/SOURCE.txt"

if ($DependenciesOnly) { Write-Host 'Pinned Concat and FFmpeg dependencies verified and staged'; return }

# Check the actual packaged executables and required native bridge before zip.
if (!(Get-ChildItem $stage -Filter '*hk_ffi*.dll' -Recurse)) { throw "Rust FFI DLL is missing" }
& "$stage/tools/ffmpeg.exe" -version | Select-Object -First 1
if ($LASTEXITCODE -ne 0) { throw "Packaged FFmpeg failed to load" }
& "$stage/tools/ffprobe.exe" -version | Select-Object -First 1
if ($LASTEXITCODE -ne 0) { throw "Packaged ffprobe failed to load" }
$probe = Start-Process "$stage/concat/concat.exe" -ArgumentList '--version' -Wait -PassThru
if ($probe.ExitCode -ne 0) { throw "Packaged Concat failed to load: $($probe.ExitCode)" }
$app = Start-Process "$stage/hokkabaz.exe" -PassThru
try {
  Start-Sleep -Seconds 8
  $app.Refresh()
  if ($app.HasExited) { throw "Hokkabaz exited on startup: $($app.ExitCode)" }
  if ($app.MainWindowHandle -eq 0) { throw "Hokkabaz did not create a desktop window" }
} finally {
  if (!$app.HasExited) { $app.CloseMainWindow() | Out-Null; $app.WaitForExit(5000) | Out-Null }
  if (!$app.HasExited) { $app.Kill() }
}
Compress-Archive -Path "$stage/*" -DestinationPath $Output -Force
$hash = (Get-FileHash $Output -Algorithm SHA256).Hash.ToLowerInvariant()
"$hash  $([System.IO.Path]::GetFileName($Output))" | Set-Content "$Output.sha256"
Write-Host "Windows package created: $Output (SHA256 $hash)"
