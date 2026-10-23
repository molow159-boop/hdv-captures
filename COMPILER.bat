@echo off
chcp 65001 >nul
cd /d "%~dp0"
where cargo >nul 2>nul
if errorlevel 1 set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
echo Verification du formatage, tests, Clippy et compilation Windows...
cargo fmt --check
if errorlevel 1 goto echec
cargo test --locked
if errorlevel 1 goto echec
cargo clippy --locked --all-targets -- -D warnings
if errorlevel 1 goto echec
cargo build --locked --release
if errorlevel 1 goto echec
echo.
set "HDV_TARGET=%~dp0target"
if defined CARGO_TARGET_DIR set "HDV_TARGET=%CARGO_TARGET_DIR%"
echo EXE : %HDV_TARGET%\release\hdv-captures.exe
certutil -hashfile "%HDV_TARGET%\release\hdv-captures.exe" SHA256
pause
exit /b 0
:echec
echo.
echo Compilation echouee. Consulte README.md et les messages ci-dessus.
pause
exit /b 1
