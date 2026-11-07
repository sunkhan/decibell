# Chat scrolling, Qt spike vs the installed Electron client, paired runs on the same machine state: the Qt client's
# `--bench-scroll` (the 10k-message fixture) and electron-scroll.mjs on the real client's open channel (launched with
# a debugging port, the channel opened through it). Both scroll into history at the same constant speed, driven per
# frame, and report frame intervals against the panel's refresh. Results append to -Out as JSON lines.
#
#   .\tools\perf\scroll-compare.ps1 [-Runs 3] [-Seconds 6] [-Speed 1000] [-Server DailyGaming] [-Channel general]
# Launching the client signs it in (saved sign-in) and opens the channel: run it with the owner's go-ahead.
param(
    [int] $Runs = 3,
    [int] $Seconds = 6,
    [int] $Speed = 1000,
    [string] $Server = 'DailyGaming',
    [string] $Channel = 'general',
    [int] $Port = 9333,
    [string] $ElectronExe = "$env:LOCALAPPDATA\Programs\decibell-electron-client\Decibell.exe",
    [string] $QtExe = 'C:\b\qt\decibell-qt.exe',
    [string] $QtBin = 'C:\Qt\6.12.0\msvc2022_64\bin',
    [string] $Node = 'C:\dev\node20\node.exe',
    [string] $Out = "$PSScriptRoot\results\scroll-compare.jsonl"
)
$ErrorActionPreference = 'Stop'
$env:Path = "$QtBin;" + $env:Path
New-Item -ItemType Directory -Force (Split-Path $Out) | Out-Null
if (@(Get-Process Decibell, decibell-qt -ErrorAction SilentlyContinue).Count) { throw 'close Decibell / decibell-qt first' }

Add-Type -Namespace ScrollCompare -Name Win -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
'@
# An occluded Chromium window runs no requestAnimationFrame: keep the client on top while it is measured.
function Set-Topmost($proc, [bool]$on) {
    $proc.Refresh()
    [void][ScrollCompare.Win]::SetWindowPos($proc.MainWindowHandle, [IntPtr]($(if ($on) { -1 } else { -2 })), 0, 0, 0, 0, 0x0013)
}

$client = Start-Process $ElectronExe -ArgumentList "--remote-debugging-port=$Port" -PassThru
& $Node --experimental-websocket "$PSScriptRoot\open-channel.mjs" $Port $Server $Channel | Out-Null
Start-Sleep -Seconds 8 # history loaded, pictures decoded, settled
$main = Get-Process Decibell | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1

for ($r = 1; $r -le $Runs; $r++) {
    $tmp = [System.IO.Path]::GetTempFileName()
    $env:DECIBELL_BENCH_OUT = $tmp
    Start-Process $QtExe -ArgumentList '--bench-scroll', "$Seconds", '--bench-speed', "$Speed" -Wait
    $qt = Get-Content $tmp -Raw | ConvertFrom-Json
    Remove-Item $tmp
    Set-Topmost $main $true
    Start-Sleep -Seconds 1
    $el = & $Node --experimental-websocket "$PSScriptRoot\electron-scroll.mjs" $Port $Seconds $Speed | ConvertFrom-Json
    Set-Topmost $main $false
    [pscustomobject]@{ run = $r; speed = $Speed; qt = $qt; electron = $el } | ConvertTo-Json -Depth 6 -Compress | Add-Content $Out
    "run {0}: qt p50 {1} p99 {2} missed {3} | electron p50 {4} p99 {5} missed {6}" -f $r, $qt.interval_ms.p50, $qt.interval_ms.p99, $qt.missed_vsyncs, $el.interval_ms.p50, $el.interval_ms.p99, $el.missed_vsyncs
}
"done (the client is left open, port $Port)"
