param(
    [string]$Python = 'python',
    [string]$OutDir = 'dist',
    [string]$BuildDir = 'build'
)

$ErrorActionPreference = 'Stop'
$ProjectDir = Split-Path -Parent $PSScriptRoot
Push-Location -LiteralPath $ProjectDir
try {
    New-Item -ItemType Directory -Path $BuildDir -Force | Out-Null
    $BuildDir = (Resolve-Path -LiteralPath $BuildDir).Path
    $Icon = Join-Path $BuildDir 'clash-monitor.ico'
    & $Python -B -c 'import sys; from clash_monitor.tray import _tray_image; _tray_image().save(sys.argv[1])' $Icon
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $env:PYINSTALLER_CONFIG_DIR = Join-Path $BuildDir 'cache'
    & $Python -B -m PyInstaller --noconfirm --clean --noupx --windowed --onefile --hidden-import pystray._win32 --icon $Icon --name ClashTrafficMonitor --distpath $OutDir --workpath $BuildDir --specpath $BuildDir clash_monitor/__main__.py
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} finally {
    Pop-Location
}
