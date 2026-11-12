# Installs Deckly from its GitHub releases on Windows (PowerShell 5.1 or 7):
#
#   irm https://raw.githubusercontent.com/deckly-app/Deckly/main/install.ps1 | iex
#
# Uninstall:
#   & ([scriptblock]::Create((irm https://raw.githubusercontent.com/deckly-app/Deckly/main/install.ps1))) -Uninstall
#
# Settings (environment variables):
#   DECKLY_VERSION   a release tag such as v0.0.1 (default: the latest release)
#   DECKLY_SETUP     run a local setup program instead of downloading
#
# Runs the release's setup program silently: Deckly goes to
# %LOCALAPPDATA%\Programs\Deckly with a Start menu entry and an entry in
# Installed apps, without administrator rights, then starts. Your profiles
# and settings are never touched.

param([switch]$Uninstall)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$Repo = 'deckly-app/Deckly'
$Name = 'deckly-setup-windows-x64.exe'
$InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\Deckly'

function Say($Text) { Write-Host "deckly: $Text" }

if ($Uninstall) {
    $Uninstaller = Join-Path $InstallDir 'unins000.exe'
    if (-not (Test-Path $Uninstaller)) {
        throw "Deckly is not installed in $InstallDir; use Settings > Apps > Installed apps"
    }
    Start-Process -Wait -FilePath $Uninstaller -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART'
    Say 'Deckly removed. Your profiles and settings are still in place.'
    return
}

if (-not [Environment]::Is64BitOperatingSystem) {
    throw 'Deckly needs 64-bit Windows.'
}

[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
$Temp = Join-Path ([IO.Path]::GetTempPath()) ("deckly-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $Temp | Out-Null
try {
    if ($env:DECKLY_SETUP) {
        $Setup = $env:DECKLY_SETUP
        Say "installing from $Setup"
    } else {
        if ($env:DECKLY_VERSION) {
            $Base = "https://github.com/$Repo/releases/download/$($env:DECKLY_VERSION)"
        } else {
            $Base = "https://github.com/$Repo/releases/latest/download"
        }
        $Setup = Join-Path $Temp $Name
        Say "downloading $Base/$Name"
        Invoke-WebRequest -UseBasicParsing -Uri "$Base/$Name" -OutFile $Setup
        $Sums = (Invoke-WebRequest -UseBasicParsing -Uri "$Base/SHA256SUMS").Content
        if ($Sums -is [byte[]]) { $Sums = [Text.Encoding]::UTF8.GetString($Sums) }
        $Line = $Sums -split "`n" | Where-Object { $_ -match "^([0-9a-fA-F]{64}) [ *]?$([regex]::Escape($Name))\s*$" } | Select-Object -First 1
        if (-not $Line) { throw "the checksums have no line for $Name" }
        $Expected = ($Line -split '\s+')[0].ToLowerInvariant()
        $Actual = (Get-FileHash -Algorithm SHA256 -Path $Setup).Hash.ToLowerInvariant()
        if ($Expected -ne $Actual) { throw "checksum mismatch for $Name; not installing" }
        Unblock-File -Path $Setup
    }

    Say 'installing (a running Deckly is closed first)'
    $Process = Start-Process -Wait -PassThru -FilePath $Setup -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/CURRENTUSER', '/LAUNCH=1'
    if ($Process.ExitCode -ne 0) { throw "the setup failed with exit code $($Process.ExitCode)" }
    Say "installed $InstallDir"
    Say 'done. Deckly is starting; later, open it from the Start menu.'
} finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $Temp
}
