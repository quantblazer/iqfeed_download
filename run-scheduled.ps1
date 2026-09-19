<#
.SYNOPSIS
  Scheduled entry point for the IQFeed downloader.

.DESCRIPTION
  Runs `iqfeed-dl download`, waiting for IQFeed to come up first. If the run fails
  (exit code 1 or 2) it waits a few minutes and tries once more. Each attempt is
  logged to logs\scheduled.log; the downloader's own detail goes to logs\export_YYYY-MM-DD.log
  and reports\download_YYYY-MM-DD.csv.

  Exit code: 0 all good, 1 some symbols failed, 2 fatal (IQFeed never became ready, bad config...).
#>
param(
    [int]$WaitMinutes = 15,
    [int]$RetryDelayMinutes = 5
)

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $root

$exe = Join-Path $root 'target\release\iqfeed-dl.exe'
$logDir = Join-Path $root 'logs'
New-Item -ItemType Directory -Force -Path $logDir | Out-Null
$log = Join-Path $logDir 'scheduled.log'

function Write-Log([string]$message) {
    Add-Content -Path $log -Value ("{0} {1}" -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), $message)
}

if (-not (Test-Path $exe)) {
    Write-Log "FATAL: $exe not found. Run 'cargo build --release' first."
    exit 2
}

$code = 0
for ($attempt = 1; $attempt -le 2; $attempt++) {
    Write-Log "attempt ${attempt}: download --wait-for-iqfeed $WaitMinutes"
    & $exe download --wait-for-iqfeed $WaitMinutes
    $code = $LASTEXITCODE
    Write-Log "attempt $attempt finished with exit code $code"
    if ($code -eq 0) { break }
    if ($attempt -lt 2) {
        Write-Log "retrying in $RetryDelayMinutes minute(s)"
        Start-Sleep -Seconds ($RetryDelayMinutes * 60)
    }
}

if ($code -eq 0) {
    Write-Log 'OK'
} else {
    Write-Log "FAILED (exit code $code); see logs\export_*.log and reports\"
}
exit $code
