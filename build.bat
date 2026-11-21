@echo off
setlocal
cd /d "%~dp0"

rem Builds both unigst.exe (CLI only) and unigstgui.exe (GUI only).
cargo build --release
if errorlevel 1 exit /b %errorlevel%

copy /y "target\release\unigst.exe" "unigst.exe" >nul
if errorlevel 1 exit /b %errorlevel%
copy /y "target\release\unigstgui.exe" "unigstgui.exe" >nul
if errorlevel 1 exit /b %errorlevel%

echo Copied unigst.exe and unigstgui.exe to %~dp0
