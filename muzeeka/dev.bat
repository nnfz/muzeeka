@echo off
setlocal
cd /d "%~dp0"

where npm >nul 2>&1
if errorlevel 1 (
  echo [error] npm not found. Install Node.js and reopen the terminal.
  pause
  exit /b 1
)

where cargo >nul 2>&1
if errorlevel 1 (
  echo [error] cargo not found. Install Rust and reopen the terminal.
  pause
  exit /b 1
)

echo [muzeeka] npm install...
call npm install
if errorlevel 1 (
  echo [error] npm install failed.
  pause
  exit /b 1
)

echo [muzeeka] starting tauri dev...
call npx tauri dev
set "EXIT_CODE=%ERRORLEVEL%"

if not "%EXIT_CODE%"=="0" (
  echo.
  echo [error] tauri dev exited with code %EXIT_CODE%
  pause
)
exit /b %EXIT_CODE%
