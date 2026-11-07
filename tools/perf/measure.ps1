<#
.SYNOPSIS
  Samples one desktop client's resource use (all of its processes) for a fixed time.

.DESCRIPTION
  Client-agnostic: the same script measures the Electron client and the Qt client, so every
  phase of the Qt work reports numbers against the same baseline. Everything comes from
  Windows performance counters in one Get-Counter stream, so rate counters are computed by
  PDH over the sample interval (no polling of our own inside the measured app).

  Per sample, summed over every process whose instance name matches -ProcessName:
    cpu_core_pct      % of one logical core (100 = one core busy); cpu_machine_pct = / cores
    ws_mb             working set
    private_ws_mb     private working set (Task Manager's "Memory" column)
    private_bytes_mb  commit charge (private bytes)
    ctx_switches_s    context switches / s over all threads: the wakeup proxy on Windows
    threads, handles
    gpu_<engtype>_pct GPU engine utilization by engine type (3D, VideoDecode, VideoEncode, Copy, ...)
    gpu_dedicated_mb  dedicated GPU memory, gpu_shared_mb shared GPU memory
  -IncludeDwm adds dwm.exe's GPU 3D utilization (composition cost both clients pay).

  Writes <Out>.csv (every sample) and <Out>.json (mean / p50 / p95 / max per metric + metadata),
  and prints the summary.

.EXAMPLE
  .\measure.ps1 -ProcessName Decibell -Seconds 300 -Label "electron idle, busy channel" -Out results\electron-idle
#>
param(
  [Parameter(Mandatory)] [string] $ProcessName,
  [int] $Seconds = 120,
  [int] $Interval = 1,
  [int] $WarmupSeconds = 0,
  [string] $Label = '',
  [Parameter(Mandatory)] [string] $Out,
  [switch] $IncludeDwm
)
$ErrorActionPreference = 'Stop'

$cores = [Environment]::ProcessorCount
$pn = $ProcessName
$counters = @(
  "\Process($pn*)\ID Process",
  "\Process($pn*)\% Processor Time",
  "\Process($pn*)\Working Set",
  "\Process($pn*)\Working Set - Private",
  "\Process($pn*)\Private Bytes",
  "\Process($pn*)\Thread Count",
  "\Process($pn*)\Handle Count",
  "\Thread($pn*)\Context Switches/sec",
  "\GPU Engine(*)\Utilization Percentage",
  "\GPU Process Memory(*)\Dedicated Usage",
  "\GPU Process Memory(*)\Shared Usage"
)
if ($IncludeDwm) { $counters += "\Process(dwm)\ID Process" }

if ($WarmupSeconds -gt 0) { Write-Host "warm-up $WarmupSeconds s"; Start-Sleep -Seconds $WarmupSeconds }

$n = [math]::Max(1, [math]::Floor($Seconds / $Interval))
Write-Host "sampling $pn every $Interval s, $n samples"
$rows = New-Object System.Collections.Generic.List[object]

Get-Counter -Counter $counters -SampleInterval $Interval -MaxSamples $n -ErrorAction SilentlyContinue | ForEach-Object {
  $s = $_
  # Instance name -> pid for our processes (and dwm when asked).
  $pidByInst = @{}
  $dwmPid = $null
  foreach ($c in $s.CounterSamples) {
    if ($c.Path -match '\\process\((.+)\)\\id process$') {
      if ($Matches[1] -eq 'dwm') { $dwmPid = [int]$c.CookedValue } else { $pidByInst[$Matches[1]] = [int]$c.CookedValue }
    }
  }
  $pids = @{}; foreach ($v in $pidByInst.Values) { if ($v -gt 0) { $pids[$v] = $true } }

  $r = [ordered]@{
    t = $s.Timestamp.ToString('o'); procs = $pids.Count
    cpu_core_pct = 0.0; ws_mb = 0.0; private_ws_mb = 0.0; private_bytes_mb = 0.0
    threads = 0; handles = 0; ctx_switches_s = 0.0
    gpu_dedicated_mb = 0.0; gpu_shared_mb = 0.0
  }
  $gpu = @{}
  $dwm3d = 0.0
  foreach ($c in $s.CounterSamples) {
    $p = $c.Path; $v = $c.CookedValue
    if ($p -match '\\process\((.+)\)\\(.+)$') {
      if (-not $pidByInst.ContainsKey($Matches[1])) { continue }
      switch ($Matches[2]) {
        '% processor time'      { $r.cpu_core_pct += $v }
        'working set'           { $r.ws_mb += $v / 1MB }
        'working set - private' { $r.private_ws_mb += $v / 1MB }
        'private bytes'         { $r.private_bytes_mb += $v / 1MB }
        'thread count'          { $r.threads += [int]$v }
        'handle count'          { $r.handles += [int]$v }
      }
    } elseif ($p -match '\\thread\((.+)/\d+\)\\context switches/sec$') {
      if ($pidByInst.ContainsKey($Matches[1])) { $r.ctx_switches_s += $v }
    } elseif ($p -match '\\gpu engine\(pid_(\d+)_.*engtype_(.+)\)\\utilization percentage$') {
      $gp = [int]$Matches[1]; $et = $Matches[2]
      if ($pids.ContainsKey($gp)) { $gpu[$et] = [double]($gpu[$et]) + $v }
      elseif ($dwmPid -and $gp -eq $dwmPid -and $et -eq '3D') { $dwm3d += $v }
    } elseif ($p -match '\\gpu process memory\(pid_(\d+)_.*\)\\(dedicated|shared) usage$') {
      if ($pids.ContainsKey([int]$Matches[1])) {
        if ($Matches[2] -eq 'dedicated') { $r.gpu_dedicated_mb += $v / 1MB } else { $r.gpu_shared_mb += $v / 1MB }
      }
    }
  }
  $r.cpu_machine_pct = $r.cpu_core_pct / $cores
  foreach ($et in @('3D', 'VideoDecode', 'VideoEncode', 'Copy', 'Compute')) { $r["gpu_$($et)_pct"] = [double]($gpu[$et]) }
  if ($IncludeDwm) { $r.dwm_gpu_3D_pct = $dwm3d }
  $rows.Add([pscustomobject]$r)
}

if ($rows.Count -eq 0) { throw "no samples: is a process named '$pn' running?" }

$dir = Split-Path -Parent $Out
if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force $dir | Out-Null }
$rows | Export-Csv -NoTypeInformation -Path "$Out.csv"

function Stat($vals) {
  $a = @($vals | Sort-Object)
  if ($a.Count -eq 0) { return $null }
  $mean = ($a | Measure-Object -Average).Average
  [ordered]@{
    mean = [math]::Round($mean, 2)
    p50 = [math]::Round($a[[math]::Floor(($a.Count - 1) * 0.5)], 2)
    p95 = [math]::Round($a[[math]::Floor(($a.Count - 1) * 0.95)], 2)
    max = [math]::Round($a[-1], 2)
  }
}
$metrics = $rows[0].PSObject.Properties.Name | Where-Object { $_ -notin @('t') }
$summary = [ordered]@{}
foreach ($m in $metrics) { $summary[$m] = Stat ($rows | ForEach-Object { $_.$m }) }

$gpuName = (Get-CimInstance Win32_VideoController | Select-Object -First 1).Name
$meta = [ordered]@{
  label = $Label; process = $pn; samples = $rows.Count; interval_s = $Interval
  started = $rows[0].t; machine = $env:COMPUTERNAME; logical_cores = $cores
  cpu = (Get-CimInstance Win32_Processor | Select-Object -First 1).Name; gpu = $gpuName
  os = (Get-CimInstance Win32_OperatingSystem).Caption + ' ' + [Environment]::OSVersion.Version
}
[ordered]@{ meta = $meta; summary = $summary } | ConvertTo-Json -Depth 5 | Set-Content -Encoding utf8 "$Out.json"

"{0}  ({1} samples x {2}s)" -f $Label, $rows.Count, $Interval
"{0,-18} {1,10} {2,10} {3,10} {4,10}" -f 'metric', 'mean', 'p50', 'p95', 'max'
foreach ($m in $metrics) { $x = $summary[$m]; "{0,-18} {1,10} {2,10} {3,10} {4,10}" -f $m, $x.mean, $x.p50, $x.p95, $x.max }
