@echo off
setlocal
cd /d "%~dp0"

cargo build --release
if errorlevel 1 exit /b %errorlevel%

copy /y "target\release\unigst.exe" "unigst.exe" >nul
echo Copied unigst.exe to %~dp0
