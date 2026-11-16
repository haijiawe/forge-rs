#!/usr/bin/env pwsh
# Install a built `forge.exe` into a directory on your PATH.
#
# This copies a binary you already have; it does not download anything. Run it
# after `cargo build --release` (or point -Source at a release artifact).
#
#   pwsh scripts/install.ps1                      # copy target/release/forge.exe
#   pwsh scripts/install.ps1 -Source bin\forge.exe
#   pwsh scripts/install.ps1 -AddToPath           # also edit the user PATH
#
# Exits 0 when the installed copy answers `--version`, 1 otherwise. Every step
# is idempotent: running it twice replaces the binary and touches PATH once.

[CmdletBinding()]
param(
    [string]$Source = "",
    [string]$Prefix = "",
    [switch]$AddToPath,
    [switch]$Force
)

$ErrorActionPreference = "Stop"

function Resolve-ForgeSource {
    param([string]$explicit)
    $candidates = @()
    if ($explicit) {
        $candidates += $explicit
    } else {
        $candidates += (Join-Path $PSScriptRoot "..\target\release\forge.exe")
        $candidates += (Join-Path $PSScriptRoot "..\bin\forge.exe")
    }
    foreach ($candidate in $candidates) {
        $full = [System.IO.Path]::GetFullPath($candidate)
        if (Test-Path -LiteralPath $full -PathType Leaf) {
            return $full
        }
    }
    throw "no forge.exe found; build one with 'cargo build --release' or pass -Source"
}

function Get-ForgeVersion {
    param([string]$binary)
    $output = & $binary --version 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "$binary --version exited $LASTEXITCODE"
    }
    $line = ($output | Select-Object -First 1).ToString()
    if (-not $line.StartsWith("forge ")) {
        throw "$binary does not look like forge: '$line'"
    }
    return $line
}

$sourceFull = Resolve-ForgeSource -explicit $Source
$sourceVersion = Get-ForgeVersion -binary $sourceFull
Write-Host "source:  $sourceFull"
Write-Host "version: $sourceVersion"

if (-not $Prefix) {
    $Prefix = Join-Path $env:LOCALAPPDATA "forge\bin"
}
$prefixFull = [System.IO.Path]::GetFullPath($Prefix)
if (-not (Test-Path -LiteralPath $prefixFull -PathType Container)) {
    New-Item -ItemType Directory -Force -Path $prefixFull | Out-Null
}
$target = Join-Path $prefixFull "forge.exe"

# Replacing a running binary fails on Windows; say so instead of throwing an
# opaque access-denied from Copy-Item.
if ((Test-Path -LiteralPath $target) -and -not $Force) {
    try {
        $stream = [System.IO.File]::Open($target, "Open", "ReadWrite", "None")
        $stream.Dispose()
    } catch {
        throw "$target is in use (a running forge?). Close it or pass -Force after stopping it."
    }
}

Copy-Item -LiteralPath $sourceFull -Destination $target -Force
$installedVersion = Get-ForgeVersion -binary $target
if ($installedVersion -ne $sourceVersion) {
    throw "installed copy reports '$installedVersion', expected '$sourceVersion'"
}
Write-Host "installed: $target"

$userPath = [System.Environment]::GetEnvironmentVariable("Path", "User")
$userEntries = @()
if ($userPath) { $userEntries = $userPath -split ";" | Where-Object { $_ } }
$inUserPath = $userEntries -contains $prefixFull
$inProcessPath = ($env:Path -split ";") -contains $prefixFull

if ($AddToPath -and -not $inUserPath) {
    $newPath = (@($userEntries) + $prefixFull) -join ";"
    [System.Environment]::SetEnvironmentVariable("Path", $newPath, "User")
    Write-Host "path:      added $prefixFull to the user PATH (new shells only)"
} elseif ($inUserPath) {
    Write-Host "path:      $prefixFull is already on the user PATH"
} elseif (-not $inProcessPath) {
    Write-Host "path:      not on PATH; run again with -AddToPath, or add:"
    Write-Host "           $prefixFull"
}

Write-Host ""
Write-Host "next: forge doctor"