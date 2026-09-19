<#
.SYNOPSIS
  Register (or remove) the weekday Task Scheduler job that runs run-scheduled.ps1.

.DESCRIPTION
  The task runs Monday-Friday at -Time (local time; this machine is on US Eastern) under
  your account, only while you are logged on. That is deliberate: IQFeed is a desktop app
  that must be running and logged in, and no password is stored anywhere.
  If the PC was off at the scheduled time the task runs as soon as it can.

  Default 17:15 ET: the CME Globex day ends at 17:00 ET, so the last hourly bar and the
  derived daily bar are complete.

.EXAMPLE
  .\register-task.ps1                     # register at 17:15
  .\register-task.ps1 -Time 17:30         # different time
  .\register-task.ps1 -IQFeedAutostart    # also start the IQFeed launcher at Windows logon
  .\register-task.ps1 -RunNow             # register, then start it once immediately
  .\register-task.ps1 -Unregister         # remove the task
#>
param(
    [string]$Time = '17:15',
    [string]$TaskName = 'IQFeed Continuous Futures Download',
    [switch]$IQFeedAutostart,
    [switch]$RunNow,
    [switch]$Unregister
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path

if ($Unregister) {
    Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
    Write-Host "Removed scheduled task '$TaskName'."
    return
}

$exe = Join-Path $root 'target\release\iqfeed-dl.exe'
if (-not (Test-Path $exe)) {
    throw "$exe not found. Run 'cargo build --release' first."
}
[void][datetime]::ParseExact($Time, 'HH:mm', $null)   # fail early on a bad time

$script = Join-Path $root 'run-scheduled.ps1'
$action = New-ScheduledTaskAction -Execute 'powershell.exe' -WorkingDirectory $root `
    -Argument "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$script`""
$trigger = New-ScheduledTaskTrigger -Weekly -At $Time `
    -DaysOfWeek Monday, Tuesday, Wednesday, Thursday, Friday
$principal = New-ScheduledTaskPrincipal -UserId "$env:USERDOMAIN\$env:USERNAME" `
    -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -StartWhenAvailable -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries -MultipleInstances IgnoreNew `
    -ExecutionTimeLimit (New-TimeSpan -Hours 1)

Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger `
    -Principal $principal -Settings $settings -Force `
    -Description 'Downloads IQFeed continuous futures (hourly + derived daily) to CSV.' | Out-Null
Write-Host "Registered '$TaskName': Mon-Fri at $Time (local time), only while $env:USERNAME is logged on."

if ($IQFeedAutostart) {
    $launcher = 'C:\Program Files\DTN\IQFeed\iqlink.exe'
    if (-not (Test-Path $launcher)) { throw "IQFeed launcher not found at $launcher" }
    $startup = [Environment]::GetFolderPath('Startup')
    $lnk = Join-Path $startup 'IQFeed Launcher.lnk'
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($lnk)
    $shortcut.TargetPath = $launcher
    $shortcut.WorkingDirectory = Split-Path $launcher
    $shortcut.Save()
    Write-Host "Added IQFeed launcher to Startup: $lnk"
    Write-Host 'Open the launcher once and tick "save username/password" / auto-connect so it logs in unattended.'
}

if ($RunNow) {
    Start-ScheduledTask -TaskName $TaskName
    Write-Host 'Started the task; watch logs\scheduled.log.'
}
