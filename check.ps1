# check.ps1 — the same gate as `make check`, for hosts without GNU make.
#
# Why this file exists: this box has MSVC's nmake.exe but no GNU make, so
# `make check` cannot run here — and a gate that only exists in CI is a gate
# nobody runs while working. The order is deliberately identical to the
# Makefile's, so a green run here means the same thing a green CI run means.
#
# Usage:  pwsh -File check.ps1
#         pwsh -File check.ps1 -SkipLint      (during a rename refactor)

param(
    [switch]$SkipLint
)

$ErrorActionPreference = 'Stop'
$failed = @()

function Step {
    param([string]$Name, [scriptblock]$Body)
    Write-Host ""
    Write-Host "== $Name " -NoNewline
    Write-Host ("=" * [Math]::Max(0, 60 - $Name.Length)) -ForegroundColor DarkGray
    $sw = [Diagnostics.Stopwatch]::StartNew()
    & $Body
    $code = $LASTEXITCODE
    $sw.Stop()
    if ($code -ne 0) {
        Write-Host "   FAILED ($($code)) after $([Math]::Round($sw.Elapsed.TotalSeconds,1))s" -ForegroundColor Red
        $script:failed += $Name
    } else {
        Write-Host "   ok ($([Math]::Round($sw.Elapsed.TotalSeconds,1))s)" -ForegroundColor Green
    }
}

Step 'fmt'  { cargo fmt --all -- --check }
if (-not $SkipLint) {
    Step 'lint' { cargo clippy --workspace --all-targets --offline -- -D warnings }
}
Step 'arch' { cargo run -q -p archcheck --offline -- }
Step 'test' { cargo test --workspace --offline }

Write-Host ""
if ($failed.Count -gt 0) {
    Write-Host "check: FAILED in $($failed -join ', ')" -ForegroundColor Red
    exit 1
}
Write-Host "check: ok" -ForegroundColor Green
exit 0
