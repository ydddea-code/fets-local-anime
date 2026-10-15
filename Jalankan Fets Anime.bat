@echo off
setlocal
for %%P in (
  "%LOCALAPPDATA%\Fets Anime\fets-anime-desktop.exe"
  "%LOCALAPPDATA%\Fets Anime\Fets Anime.exe"
  "%LOCALAPPDATA%\Programs\Fets Anime\fets-anime-desktop.exe"
  "%LOCALAPPDATA%\Programs\Fets Anime\Fets Anime.exe"
  "%~dp0src-tauri\target\release\fets-anime-desktop.exe"
  "%~dp0src-tauri\target\debug\fets-anime-desktop.exe"
) do (
  if exist "%%~P" (
    start "" "%%~P"
    exit /b 0
  )
)
echo Fets Anime belum terpasang di komputer ini.
echo Buka GitHub Releases untuk mengunduh installer Windows.
start "" "https://github.com/ydddea-code/fets-local-anime/releases/latest"
