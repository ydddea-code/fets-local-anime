$ErrorActionPreference = 'Stop'
$binaryDir = Join-Path $PSScriptRoot '..\src-tauri\binaries'
$licenseDir = Join-Path $PSScriptRoot '..\licenses\third-party'
$scratch = Join-Path $env:TEMP ('fets-sidecars-' + [guid]::NewGuid())
New-Item -ItemType Directory -Force $binaryDir, $licenseDir, $scratch | Out-Null

function Get-LatestAsset($repository, $pattern) {
    $release = Invoke-RestMethod -Headers @{ 'User-Agent' = 'fets-anime-build' } "https://api.github.com/repos/$repository/releases/latest"
    $asset = $release.assets | Where-Object { $_.name -match $pattern } | Select-Object -First 1
    if (-not $asset) { throw "No asset matching $pattern in $repository latest release" }
    $archive = Join-Path $scratch $asset.name
    Invoke-WebRequest -Headers @{ 'User-Agent' = 'fets-anime-build' } -Uri $asset.browser_download_url -OutFile $archive
    return $archive
}

try {
    $ffmpegArchive = Get-LatestAsset 'BtbN/FFmpeg-Builds' '^ffmpeg-master-latest-win64-gpl-shared\.zip$'
    $ffmpegRoot = Join-Path $scratch 'ffmpeg'
    Expand-Archive -LiteralPath $ffmpegArchive -DestinationPath $ffmpegRoot
    $ffmpegBin = Get-ChildItem $ffmpegRoot -Directory -Recurse | Where-Object { (Test-Path (Join-Path $_.FullName 'ffmpeg.exe')) -and (Test-Path (Join-Path $_.FullName 'ffprobe.exe')) } | Select-Object -First 1
    if (-not $ffmpegBin) { throw 'ffmpeg.exe and ffprobe.exe were not found in the FFmpeg archive' }
    Copy-Item (Join-Path $ffmpegBin.FullName '*') $binaryDir -Force

    if (-not (Get-Command 7z.exe -ErrorAction SilentlyContinue)) { throw 'Install 7-Zip and ensure 7z.exe is on PATH to fetch the mpv archive.' }
    $mpvArchive = Get-LatestAsset 'shinchiro/mpv-winbuild-cmake' '^mpv-x86_64-[0-9]{8}-git-[a-f0-9]+\.7z$'
    $mpvRoot = Join-Path $scratch 'mpv'
    New-Item -ItemType Directory $mpvRoot | Out-Null
    & 7z.exe x $mpvArchive "-o$mpvRoot" -y | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Could not extract mpv archive' }
    $mpv = Get-ChildItem $mpvRoot -Filter mpv.exe -File -Recurse | Select-Object -First 1
    if (-not $mpv) { throw 'mpv.exe was not found in the mpv archive' }
    Copy-Item (Join-Path $mpv.DirectoryName '*') $binaryDir -Force
    Get-ChildItem $mpvRoot -File -Recurse | Where-Object { $_.Name -match '^(LICENSE|COPYING|README|.*license.*|.*copyright.*)' } | Copy-Item -Destination $licenseDir -Force
    Get-ChildItem $ffmpegRoot -File -Recurse | Where-Object { $_.Name -match '^(LICENSE|COPYING|.*license.*|.*copyright.*)' } | Copy-Item -Destination $licenseDir -Force
    Set-Content (Join-Path $licenseDir 'VERSIONS.txt') "FFmpeg archive: $($ffmpegArchive | Split-Path -Leaf)`r`nmpv archive: $($mpvArchive | Split-Path -Leaf)`r`nmpv source/build: https://github.com/shinchiro/mpv-winbuild-cmake`r`nFFmpeg source/build: https://github.com/BtbN/FFmpeg-Builds"
}
finally {
    Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
}
