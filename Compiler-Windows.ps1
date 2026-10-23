# Vérifications et compilation ; aucune installation ni modification de Windows.
$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath $PSScriptRoot
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
    if (Test-Path (Join-Path $cargoBin 'cargo.exe')) { $env:PATH = "$cargoBin;$env:PATH" }
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw 'Rust/Cargo manquant : suis le mini-tutoriel de compilation dans README.md.'
}
$info = & rustc -vV | Out-String
if ($LASTEXITCODE -ne 0 -or $info -notmatch 'host: x86_64-pc-windows-msvc') {
    throw 'Installe la chaine Rust Windows x64 MSVC : voir README.md.'
}
Write-Host 'Verification du formatage, tests, Clippy et compilation Windows...' -ForegroundColor Cyan
& cargo fmt --check
if ($LASTEXITCODE -ne 0) { throw 'Formatage invalide ou rustfmt absent : rustup component add rustfmt' }
& cargo test --locked
if ($LASTEXITCODE -ne 0) { throw 'Les tests ont echoue. Lis les erreurs ci-dessus.' }
& cargo clippy --locked --all-targets -- -D warnings
if ($LASTEXITCODE -ne 0) { throw 'Clippy a echoue ou manque : rustup component add clippy' }
& cargo build --locked --release
if ($LASTEXITCODE -ne 0) { throw 'Compilation echouee : verifie MSVC et le SDK Windows (README.md).' }
$targetRoot = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $PSScriptRoot 'target' }
$exePath = Join-Path $targetRoot 'release\hdv-captures.exe'
if (-not (Test-Path -LiteralPath $exePath)) { throw 'Executable absent apres compilation.' }
Write-Host "EXE : $exePath" -ForegroundColor Green
Get-FileHash -LiteralPath $exePath -Algorithm SHA256 | Format-List
