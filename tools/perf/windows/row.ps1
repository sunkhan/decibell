<#
.SYNOPSIS
  One measured row of the client-against-Electron table on Windows: samples a running client and appends
  the row to a results file.

.DESCRIPTION
  The Windows twin of what `measure.sh` + `scenarios.sh` produce on Linux, built on the two scripts beside
  this folder that were already used for the Electron baseline: `..\measure.ps1` (one Get-Counter stream:
  CPU, memory, context switches, GPU engines, summed over the process tree) and `..\threads.ps1` (context
  switches per thread, with thread names). Windows PowerShell 5.1; nothing to install.

  The client is NOT started by this script: start it yourself in the state to measure and pass its process
  id (or its name, when only one client with that name runs: the Electron client is Decibell.exe and the
  native one decibell.exe, the same name to Windows, so with both running a name is refused). In particular
  the installed Electron client is never started by a script, which would use the owner's profile unasked.

  What a row holds, and why these counters:
    private_ws_mb    Process \ Working Set - Private, summed over the tree, mean. THE memory figure: the
                     pages only this process can use that are in RAM. The fair twin of Linux's
                     Private_Clean + Private_Dirty, with one difference to keep in mind: Windows counts a
                     process's own executable pages as shareable, never private, where Linux counts the
                     pages of a binary only one process maps as Private_Clean. So this reads a little
                     lower than the Linux figure for the same single-process program; both clients are
                     measured the same way here, which is what the ratio needs.
    commit_mb        Process \ Private Bytes: committed private memory whether or not it is in RAM. The
                     upper bound, and what a low-memory machine pays in page file.
    cpu_core_pct     Process \ % Processor Time: percent of one logical core, mean over the window.
    wakeups_s        Thread \ Context Switches/sec over every thread of the tree, median. A context
                     switch is a thread being scheduled in: the nearest Windows has to Linux's voluntary
                     context switches (it also counts preemptions, which at idle are nil).
    frames           Frames the native client drew / presented during the window, from its own counters
                     (see -PerfLog). DWM offers no per-process presented-frame count a script can read
                     without tracing, so Electron's cell stays empty: its idle cost shows in CPU, wakeups
                     and GPU 3D instead.
    gpu_3d_pct, gpu_decode_pct   GPU Engine \ Utilization Percentage of the tree's 3D and VideoDecode engines.
    gpu_mb           GPU Process Memory \ Dedicated Usage.
  The per-thread table (who wakes) is written beside the samples as <run>-threads.txt.

.PARAMETER PerfLog
  Native client only: the file its stderr goes to. Start the client with --perf and stderr redirected:
      $p = Start-Process .\target\release\decibell.exe -ArgumentList '--perf' -RedirectStandardError perf.log -PassThru
  With --perf the client prints "decibell: frames drawn=N presented=M" whenever the named event
  Local\decibell-perf-<pid> is set; this script sets it at both ends of the window and subtracts.

.EXAMPLE
  .\row.ps1 -ProcessId $p.Id -Client gpui -Label "idle, #general, unfocused" -PerfLog perf.log
.EXAMPLE
  .\row.ps1 -ProcessId 12345 -Client electron -Label "idle, #general, unfocused"
#>
param(
  [int] $ProcessId = 0,
  [string] $ProcessName,
  [Parameter(Mandatory)] [string] $Label,
  [ValidateSet('gpui', 'electron')] [string] $Client = 'gpui',
  [int] $Settle = 20,
  [int] $Seconds = 60,
  [string] $PerfLog,
  [string] $Results,
  [int] $Top = 10
)
$ErrorActionPreference = 'Stop'
$perf = Split-Path -Parent $PSScriptRoot
if (-not $Results) { $Results = Join-Path $perf 'results\windows-table.md' }

if ($ProcessId -le 0) {
  if (-not $ProcessName) { throw "give -ProcessId (or -ProcessName when only one client of that name runs)" }
  # Roots: processes of that name whose parent is not one of them (Electron's helpers are children).
  $named = @(Get-CimInstance Win32_Process | Where-Object { $_.Name -ieq "$ProcessName.exe" })
  $ids = @($named | ForEach-Object { [int]$_.ProcessId })
  $roots = @($named | Where-Object { $ids -notcontains [int]$_.ParentProcessId })
  if ($roots.Count -ne 1) { throw "$($roots.Count) clients named $ProcessName are running: pass -ProcessId" }
  $ProcessId = [int]$roots[0].ProcessId
}
$root = Get-Process -Id $ProcessId

# The native client's frame counters, through its perf event. $null when it cannot be read.
function Frames {
  if ($Client -ne 'gpui' -or -not $PerfLog) { return $null }
  try {
    $event = [System.Threading.EventWaitHandle]::OpenExisting("Local\decibell-perf-$ProcessId")
    [void]$event.Set(); $event.Dispose()
  } catch { Write-Warning "no perf event (was the client started with --perf?): $_"; return $null }
  Start-Sleep -Milliseconds 500
  $line = @(Get-Content -Path $PerfLog -ErrorAction SilentlyContinue | Where-Object { $_ -match 'frames drawn=(\d+) presented=(\d+)' }) | Select-Object -Last 1
  if ($line -and $line -match 'frames drawn=(\d+) presented=(\d+)') { return @([int64]$Matches[1], [int64]$Matches[2]) }
  $null
}

$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$slug = ($Label.ToLower() -replace '[^a-z0-9]+', '-').Trim('-')
$dir = Split-Path -Parent $Results
if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force $dir | Out-Null }
$out = Join-Path $dir "$Client-$slug-$stamp"

if ($Settle -gt 0) { Write-Host "settling $Settle s"; Start-Sleep -Seconds $Settle }
$before = Frames
& (Join-Path $perf 'measure.ps1') -ProcessId $ProcessId -Seconds $Seconds -Label "$Client $Label" -Out $out | Out-Host
$after = Frames
& (Join-Path $perf 'threads.ps1') -ProcessId $ProcessId -Seconds 10 -Top $Top | Out-String | Set-Content -Encoding utf8 "$out-threads.txt"
Get-Content "$out-threads.txt" | Out-Host

$json = Get-Content -Raw "$out.json" | ConvertFrom-Json
$s = $json.summary
$frames = ''
if ($before -and $after) { $frames = "{0} / {1}" -f ($after[0] - $before[0]), ($after[1] - $before[1]) }
function Cell($metric, $field) {
  $m = $s.$metric
  if ($null -eq $m) { return '' }
  [string]$m.$field
}
$row = "| {0} | {1} | {2} | {3} | {4} | {5} | {6} | {7} | {8} | {9} | {10} | {11} | {12} |" -f `
  $stamp, $Client, $Label, (Cell 'private_ws_mb' 'mean'), (Cell 'private_bytes_mb' 'mean'), (Cell 'cpu_core_pct' 'mean'),
  (Cell 'ctx_switches_s' 'p50'), $frames, (Cell 'gpu_3D_pct' 'mean'), (Cell 'gpu_VideoDecode_pct' 'mean'),
  (Cell 'gpu_dedicated_mb' 'mean'), (Cell 'threads' 'p50'), (Cell 'procs' 'p50')
if (-not (Test-Path $Results)) {
  @(
    "# Windows measurements: the native client against Electron",
    "",
    "Rows appended by tools/perf/windows/row.ps1 ($Seconds s windows after a settle; columns explained in the script). Machine: $($json.meta.cpu); $($json.meta.gpu); $($json.meta.os).",
    "",
    "| run | client | scenario | private_ws_mb | commit_mb | cpu_core_pct | wakeups_s | frames drawn / presented | gpu_3d_pct | gpu_decode_pct | gpu_mb | threads | procs |",
    "|---|---|---|---|---|---|---|---|---|---|---|---|---|"
  ) | Set-Content -Encoding utf8 $Results
}
Add-Content -Encoding utf8 -Path $Results -Value $row
Write-Host ""
Write-Host $row
Write-Host "appended to $Results (process $($root.ProcessName) $ProcessId; samples $out.csv, threads $out-threads.txt)"
