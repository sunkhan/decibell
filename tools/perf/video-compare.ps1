# Watched-stream comparison, Qt client vs Electron (qt-client spike (a)): plays the same .dbv test streams in both,
# alternating, and measures each run three ways: the app's own report (decode / draw counts, latency), framecount
# (which video frames reached the screen: the frame number barcode read through Desktop Duplication) and, with -Resources, measure.ps1 (CPU,
# wakeups, memory, GPU engines). Results append to -Out as JSON lines.
#
#   .\tools\perf\video-compare.ps1 -Streams C:\b\streams\1440p120-h264.dbv [-Runs 2] [-Seconds 10] [-Resources]
param(
    [Parameter(Mandatory)] [string[]] $Streams,
    [int] $Runs = 2,
    [int] $Seconds = 10,
    [switch] $Resources,
    [switch] $Maximized,
    [string] $QtExe = 'C:\b\qt\decibell-qt.exe',
    [string] $QtBin = 'C:\Qt\6.12.0\msvc2022_64\bin',
    [string] $FrameCount = 'C:\b\framecount\framecount.exe',
    [string] $Out = "$PSScriptRoot\results\video-compare.jsonl"
)
$ErrorActionPreference = 'Stop'
$repo = Resolve-Path "$PSScriptRoot\..\.."
$electron = "$repo\electron-client\node_modules\electron\dist\electron.exe"
$harness = "$PSScriptRoot\electron-video"
New-Item -ItemType Directory -Force (Split-Path $Out) | Out-Null
$env:Path = "$QtBin;" + $env:Path
$fps = 120
if ((Split-Path $Streams[0] -Leaf) -match 'p(\d+)-') { $fps = [int]$Matches[1] }
$appSeconds = $Seconds + 4 # 2 s settle inside the app + margin for framecount
# measure.ps1 sums every process by name: a run left over from before would be counted in.
$stale = @(Get-Process decibell-qt, electron -ErrorAction SilentlyContinue)
if ($stale.Count) { throw "close these first (measure.ps1 would count them in): $(($stale | ForEach-Object { "$($_.Name) $($_.Id)" }) -join ', ')" }

function Measure-Run($client) {
    $tmp = [System.IO.Path]::GetTempFileName()
    if ($client -eq 'qt') {
        $env:DECIBELL_BENCH_OUT = $tmp
        $qargs = @('--video', ($Streams -join ','), '--bench-video', "$appSeconds")
        if ($Maximized) { $qargs += '--maximized' }
        $proc = Start-Process $QtExe -ArgumentList $qargs -PassThru
        $title = 'Decibell (Qt spike)'; $procName = 'decibell-qt'
    } else {
        $eargs = @($harness, "--files=$($Streams -join ',')", "--seconds=$appSeconds", "--out=$tmp")
        if ($Maximized) { $eargs += '--maximized' }
        $proc = Start-Process $electron -ArgumentList $eargs -PassThru
        $title = 'electron-video'; $procName = 'electron'
    }
    Start-Sleep -Seconds 3
    $res = $null
    if ($Resources) {
        $resJob = Start-Job -ScriptBlock { param($s, $n, $sec) & "$s\measure.ps1" -ProcessName $n -WarmupSeconds 0 -Seconds $sec -Out "$env:TEMP\vc-res" | Out-Null; Get-Content "$env:TEMP\vc-res.json" -Raw } -ArgumentList $PSScriptRoot, $procName, ($Seconds - 1)
    }
    $fc = & $FrameCount $title $Seconds "--fps=$fps" "--tiles=$($Streams.Count)" | ConvertFrom-Json
    if ($Resources) { $res = (Receive-Job $resJob -Wait | ConvertFrom-Json).summary; Remove-Job $resJob }
    $proc.WaitForExit(60000) | Out-Null
    # Electron's child processes (GPU, network, renderer) end a moment after the main one.
    for ($i = 0; $i -lt 30 -and @(Get-Process $procName -ErrorAction SilentlyContinue).Count; $i++) { Start-Sleep -Milliseconds 500 }
    $app = Get-Content $tmp -Raw | ConvertFrom-Json
    Remove-Item $tmp
    [pscustomobject]@{ client = $client; streams = $Streams; maximized = [bool]$Maximized; framecount = $fc; app = $app; resources = $res }
}

for ($r = 1; $r -le $Runs; $r++) {
    foreach ($client in 'qt', 'electron') {
        $m = Measure-Run $client
        $m | ConvertTo-Json -Depth 8 -Compress | Add-Content $Out
        $tiles = ($m.framecount.tiles | ForEach-Object { "{0}/s skipped {1} (p50 {2} p95 {3} p99 {4} ms, late {5})" -f $_.shown_per_s, $_.skipped, $_.interval_ms.p50, $_.interval_ms.p95, $_.interval_ms.p99, $_.late }) -join ' | '
        $res = if ($m.resources) { " | cpu {0}% ctx/s {1} privWS {2} MB" -f $m.resources.cpu_core_pct.mean, $m.resources.ctx_switches_s.mean, $m.resources.private_ws_mb.mean } else { '' }
        "{0,-9} run {1}: {2}{3}" -f $client, $r, $tiles, $res
    }
}
