$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath $PSScriptRoot
$text = & cargo metadata --locked --format-version 1 --filter-platform x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { throw 'Inventaire Windows impossible' }
$metadata = $text | ConvertFrom-Json
$ids = @{}; foreach ($node in $metadata.resolve.nodes) { $ids[$node.id] = $true }
$notices = Get-Content -LiteralPath LICENCES-TIERS.txt -Raw
$missing = @()
foreach ($package in $metadata.packages) {
    if ($package.name -eq 'hdv-captures' -or -not $ids.ContainsKey($package.id)) { continue }
    $pattern = '(?m)^' + [regex]::Escape("$($package.name) $($package.version) |")
    if ($notices -notmatch $pattern) { $missing += "$($package.name) $($package.version)" }
}
if ($missing.Count) { throw "Notices à actualiser après changement de dépendances : $($missing -join ', ')" }
Write-Host 'Chaque dépendance Windows active dispose de sa notice de version. Les textes doivent également être relus lors de chaque mise à jour.'
