# Installs Deckly from its GitHub releases on Windows (PowerShell 5.1 or 7):
#
#   irm https://raw.githubusercontent.com/auuruum/Deckly/main/install.ps1 | iex
#
# Uninstall:
#   & ([scriptblock]::Create((irm https://raw.githubusercontent.com/auuruum/Deckly/main/install.ps1))) -Uninstall
#
# Settings (environment variables):
#   DECKLY_VERSION   a release tag such as v0.1.0 (default: the latest release)
#   DECKLY_ARCHIVE   install from a local zip instead of downloading
#
# Deckly goes to %LOCALAPPDATA%\Programs\Deckly with a Start menu shortcut.
# No administrator rights are needed. Your profiles and settings are never
# touched.

param([switch]$Uninstall)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$Repo = 'auuruum/Deckly'
$Target = 'x86_64-pc-windows-msvc'
$Name = "deckly-$Target.zip"
$InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\Deckly'
$Shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'Deckly.lnk'

function Say($Text) { Write-Host "deckly: $Text" }

function Stop-Deckly {
    $running = Get-Process -Name 'deckly', 'deckly-web' -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith($InstallDir, [StringComparison]::OrdinalIgnoreCase) }
    if ($running) {
        Say 'closing the running Deckly'
        $running | Stop-Process -Force
        Start-Sleep -Milliseconds 500
    }
}

if ($Uninstall) {
    Stop-Deckly
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $InstallDir
    Remove-Item -Force -ErrorAction SilentlyContinue $Shortcut
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
    if ($env:DECKLY_ARCHIVE) {
        $Archive = $env:DECKLY_ARCHIVE
        Say "installing from $Archive"
    } else {
        if ($env:DECKLY_VERSION) {
            $Base = "https://github.com/$Repo/releases/download/$($env:DECKLY_VERSION)"
        } else {
            $Base = "https://github.com/$Repo/releases/latest/download"
        }
        $Archive = Join-Path $Temp $Name
        Say "downloading $Base/$Name"
        Invoke-WebRequest -UseBasicParsing -Uri "$Base/$Name" -OutFile $Archive
        $Sums = (Invoke-WebRequest -UseBasicParsing -Uri "$Base/SHA256SUMS").Content
        if ($Sums -is [byte[]]) { $Sums = [Text.Encoding]::UTF8.GetString($Sums) }
        $Line = $Sums -split "`n" | Where-Object { $_ -match "^([0-9a-fA-F]{64}) [ *]?$([regex]::Escape($Name))\s*$" } | Select-Object -First 1
        if (-not $Line) { throw "the checksums have no line for $Name" }
        $Expected = ($Line -split '\s+')[0].ToLowerInvariant()
        $Actual = (Get-FileHash -Algorithm SHA256 -Path $Archive).Hash.ToLowerInvariant()
        if ($Expected -ne $Actual) { throw "checksum mismatch for $Name; not installing" }
    }

    Expand-Archive -Path $Archive -DestinationPath $Temp -Force
    $Source = Join-Path $Temp "deckly-$Target"
    if (-not (Test-Path (Join-Path $Source 'deckly.exe'))) {
        throw "the archive does not hold deckly-$Target\deckly.exe"
    }

    Stop-Deckly
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    Copy-Item -Path (Join-Path $Source '*') -Destination $InstallDir -Recurse -Force
    # Downloaded files carry the internet zone mark; the zip was checked.
    Get-ChildItem -Path $InstallDir -Recurse -File | Unblock-File

    $Shell = New-Object -ComObject WScript.Shell
    $Link = $Shell.CreateShortcut($Shortcut)
    $Link.TargetPath = Join-Path $InstallDir 'deckly.exe'
    $Link.WorkingDirectory = $InstallDir
    $Link.Description = 'Deckly'
    $Link.Save()

    Say "installed $InstallDir"
    Say 'done. Start Deckly from the Start menu.'
} finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $Temp
}
