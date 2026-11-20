@echo off
rem Release build of the terminal. The C runtime is linked in by .cargo\config.toml, so the exe
rem runs on a machine without the Visual C++ redistributable.
setlocal
cd /d "%~dp0"
rem set RUSTFLAGS=-C target-feature=+crt-static
cargo build --release --bin TradeVision
if errorlevel 1 goto :failed
echo.
echo Built %CD%\target\release\TradeVision.exe
pause
exit /b 0

:failed
echo Build failed.
pause
exit /b 1
