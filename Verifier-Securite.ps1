# Lecture seule du code ; les rapports sont écrits dans target/security.
$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath $PSScriptRoot
if (-not (Get-Command cargo-audit -ErrorAction SilentlyContinue)) {
    throw 'Outil facultatif manquant : cargo install cargo-audit --locked --version 0.22.2'
}
$directory = Join-Path $PSScriptRoot 'target/security'
New-Item -ItemType Directory -Force $directory | Out-Null
$metadataText = & cargo metadata --locked --format-version 1 --filter-platform x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { throw 'Inventaire Windows impossible' }
$metadata = $metadataText | ConvertFrom-Json
$ids = @{}; foreach ($node in $metadata.resolve.nodes) { $ids[$node.id] = $true }
$active = @{}; foreach ($package in $metadata.packages) {
    if ($ids.ContainsKey($package.id)) { $active["$($package.name)@$($package.version)"] = $true }
}
$auditText = & cargo-audit audit --file Cargo.lock --json
# A nonzero audit result may concern a dependency used only on another OS.
if (-not $auditText) { throw 'Audit RustSec sans résultat : vérifie la connexion réseau' }
$report = $auditText | ConvertFrom-Json
$auditText | Set-Content -Encoding utf8 (Join-Path $directory 'rustsec-complet.json')
$failures = @($report.vulnerabilities.list | Where-Object { $active.ContainsKey("$($_.package.name)@$($_.package.version)") })
$other = @($report.vulnerabilities.list | Where-Object { -not $active.ContainsKey("$($_.package.name)@$($_.package.version)") })
Write-Host "RustSec : $($failures.Count) alerte(s) sur les dépendances Windows actives ; $($other.Count) sur les autres cibles du verrou."
foreach ($entry in $report.warnings.unmaintained) {
    if ($active.ContainsKey("$($entry.package.name)@$($entry.package.version)")) { Write-Warning "Composant non maintenu : $($entry.package.name) $($entry.package.version)" }
}
if ($failures.Count) {
    $failures | ForEach-Object { Write-Host "$($_.advisory.id) : $($_.package.name) $($_.package.version) - $($_.advisory.title)" }
    throw 'Des avis RustSec concernent la cible Windows : analyse et correction nécessaires.'
}
Write-Host 'Ce contrôle ne couvre pas les bibliothèques C/C++ du moteur OCR.'
