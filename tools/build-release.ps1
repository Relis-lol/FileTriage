<#
.SYNOPSIS
    Baut FileTriage so, wie es ausgeliefert wird.

.DESCRIPTION
    Der Unterschied zu `cargo build --release` ist klein, aber wichtig: Dieses
    Skript setzt --remap-path-prefix. Ohne das stehen der Pfad des
    Benutzerprofils und der der Crate-Registry im fertigen Programm - das war
    kein theoretisches Risiko, sondern der erste Fund von
    tools/audit_release.py.

    Das Cargo-Profilfeld `trim-paths` waere der saubere Weg, ist in Cargo 1.97
    aber noch nicht stabil. Die Pfade hier zur Bauzeit zu ermitteln ist
    ausserdem besser, als sie in eine eingecheckte Datei zu schreiben: Auf
    einem anderen Rechner heissen sie anders.

.PARAMETER Channel
    'community' - Oeffentliche Source-Available-Ausgabe (Voreinstellung).
    'store'  - Der Microsoft Store ist die Lizenzautoritaet.
    'direct' - Privater direkter Bau ausserhalb des Stores.

.PARAMETER Bundle
    Zusaetzlich den NSIS-Installer erzeugen.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\build-release.ps1
    powershell -ExecutionPolicy Bypass -File tools\build-release.ps1 -Channel store
#>
param(
  [ValidateSet('community', 'store', 'direct')][string]$Channel = 'community',
  [switch]$Bundle
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$src = Join-Path $repo 'src-tauri'

Write-Host 'FileTriage Release-Bau'
Write-Host "  Kanal   : $Channel"
Write-Host "  Projekt : $repo"

# --- Pfade, die nicht ins Programm gehoeren -------------------------------
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$rustupHome = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $env:USERPROFILE '.rustup' }

# Reihenfolge zaehlt, und zwar andersherum als man denkt: Passen mehrere
# Regeln, nimmt rustc die LETZTE. Also steht das breite Auffangnetz vorn und
# die genauen Regeln hinten. Andersherum gewinnt das Netz, und im Programm
# landen Pfade, die noch die Cargo-Registry nennen.
$remap = @(
  "--remap-path-prefix=$env:USERPROFILE=~"
  "--remap-path-prefix=$rustupHome=rust"
  "--remap-path-prefix=$cargoHome=cargo"
  "--remap-path-prefix=$repo=filetriage"
)

$env:RUSTFLAGS = $remap -join ' '
$env:FILETRIAGE_CHANNEL = $Channel

Write-Host "  RUSTFLAGS : $($env:RUSTFLAGS)"
Write-Host ''

Push-Location $src
try {
  # Der Kanal steckt in build.rs; eine Aenderung erzwingt eine Neuuebersetzung.
  if ($Bundle) {
    cargo tauri build
  } else {
    cargo build --release
  }
  if ($LASTEXITCODE -ne 0) { throw "Bau fehlgeschlagen (Exit $LASTEXITCODE)" }
} finally {
  Pop-Location
  Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue
  Remove-Item Env:FILETRIAGE_CHANNEL -ErrorAction SilentlyContinue
}

$exe = Join-Path $src 'target\release\filetriage.exe'
if (-not (Test-Path $exe)) { throw "Kein Programm unter $exe" }

$mb = (Get-Item $exe).Length / 1MB
$hash = (Get-FileHash $exe -Algorithm SHA256).Hash
Write-Host ''
Write-Host ('Programm : {0}' -f $exe)
Write-Host ('Groesse  : {0:N2} MB' -f $mb)
Write-Host ('SHA256   : {0}' -f $hash)

if ($Bundle) {
  $nsisDir = Join-Path $src 'target\release\bundle\nsis'
  $installer = Get-ChildItem -LiteralPath $nsisDir -Filter '*.exe' -File |
    Sort-Object LastWriteTimeUtc -Descending |
    Select-Object -First 1
  if (-not $installer) { throw "Kein NSIS-Installer unter $nsisDir" }
  $installerHash = (Get-FileHash -LiteralPath $installer.FullName -Algorithm SHA256).Hash
  Write-Host ('Installer: {0}' -f $installer.FullName)
  Write-Host ('SHA256   : {0}' -f $installerHash)
}

# --- Sofort nachsehen, ob etwas durchgerutscht ist ------------------------
Write-Host ''
Write-Host 'Leck-Pruefung:'
python (Join-Path $PSScriptRoot 'audit_release.py')
if ($LASTEXITCODE -ne 0) {
  throw 'Die Leck-Pruefung hat etwas gefunden. Das Ergebnis darf nicht ausgeliefert werden.'
}
