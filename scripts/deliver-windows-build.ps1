param([string]$Status, [string]$SourceSha, [string]$RunUrl)
$ErrorActionPreference = "Stop"
# Git-only artifact transport for cloud sessions whose Git proxy works while
# GitHub's Actions API is unavailable. Each fresh branch is isolated from code.
$temp = Join-Path $env:RUNNER_TEMP "hokkabaz-delivery"
New-Item -ItemType Directory -Force $temp | Out-Null
$parts = @()
if (Test-Path 'apps/flutter/windows-release.log') { Copy-Item 'apps/flutter/windows-release.log' "$temp/windows-release.log" }
if (Test-Path 'windows-package.log') { Copy-Item 'windows-package.log' "$temp/windows-package.log" }
if ((Test-Path 'hokkabaz-windows-x64.zip') -and (Test-Path 'hokkabaz-windows-x64.zip.sha256')) { $Status = 'success' }
if ($Status -eq 'success') {
  $file = [System.IO.File]::OpenRead((Resolve-Path 'hokkabaz-windows-x64.zip'))
  try {
    $buffer = New-Object byte[] (48MB)
    $index = 0
    while (($count = $file.Read($buffer, 0, $buffer.Length)) -gt 0) {
      $name = 'hokkabaz-windows-x64.zip.part{0:D3}' -f $index
      $dest = Join-Path $temp $name
      $stream = [System.IO.File]::Create($dest)
      try { $stream.Write($buffer, 0, $count) } finally { $stream.Dispose() }
      $parts += @{ name = $name; sha256 = (Get-FileHash $dest -Algorithm SHA256).Hash.ToLowerInvariant(); bytes = $count }
      ++$index
    }
  } finally { $file.Dispose() }
  Copy-Item 'hokkabaz-windows-x64.zip.sha256' $temp
}
@{status=$Status; source_sha=$SourceSha; run_url=$RunUrl; parts=$parts} | ConvertTo-Json -Depth 5 | Set-Content "$temp/status.json"
git switch --orphan "windows-build-$env:GITHUB_RUN_ID-$env:GITHUB_RUN_ATTEMPT"
if ($LASTEXITCODE -ne 0) { throw "Cannot create artifact branch" }
New-Item -ItemType Directory -Force artifacts | Out-Null
Copy-Item "$temp/*" artifacts
git add -f artifacts
git -c user.name='github-actions[bot]' -c user.email='41898282+github-actions[bot]@users.noreply.github.com' commit -m "Windows build $SourceSha ($Status)"
if ($LASTEXITCODE -ne 0) { throw "Cannot commit build delivery" }
git push origin "HEAD:refs/heads/builds/windows-x64-$SourceSha"
if ($LASTEXITCODE -ne 0) { throw "Cannot deliver Windows build via Git" }
