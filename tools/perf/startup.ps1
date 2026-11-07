<#
.SYNOPSIS
  Times a desktop client's startup: launch -> first visible top-level window, and launch -> settled.

.DESCRIPTION
  Client-agnostic (Electron or Qt). Each run launches -Exe, waits until a process named
  -ProcessName owns a visible top-level window ("visible"), then until the whole process tree has
  used under -SettleCorePct % of one core for -SettleSeconds ("settled": signed in, first screen
  drawn, startup work done). Then it closes the window (WM_CLOSE), waits for every process to exit
  (killing leftovers after 15 s) and pauses before the next run.

  Warm starts: one extra first run is discarded (it pays the cold file cache). A client with a saved
  sign-in signs in on every run; keep -Runs small (central allows 30 password sign-ins per address,
  then one per 2 s).

  Writes <Out>.json with every run and the medians.

.EXAMPLE
  .\startup.ps1 -Exe "$env:LOCALAPPDATA\Programs\decibell-electron-client\Decibell.exe" -Out results\electron-startup
#>
param(
  [Parameter(Mandatory)] [string] $Exe,
  [string] $ProcessName = '',
  [int] $Runs = 5,
  [string[]] $ExeArgs = @(),
  [double] $SettleCorePct = 2.0,
  [int] $SettleSeconds = 2,
  [int] $TimeoutSeconds = 90,
  [int] $PauseSeconds = 5,
  [Parameter(Mandatory)] [string] $Out
)
$ErrorActionPreference = 'Stop'
if (-not $ProcessName) { $ProcessName = [IO.Path]::GetFileNameWithoutExtension($Exe) }

Add-Type -Namespace Perf -Name Win -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
'@

function Tree { @(Get-Process -Name $ProcessName -ErrorAction SilentlyContinue) }
function CpuSeconds {
  $t = 0.0
  foreach ($p in (Tree)) { try { $t += $p.TotalProcessorTime.TotalSeconds } catch {} }
  $t
}
function CloseAll {
  foreach ($p in (Tree)) { try { if ($p.MainWindowHandle -ne 0) { [void]$p.CloseMainWindow() } } catch {} }
  $sw = [Diagnostics.Stopwatch]::StartNew()
  while ((Tree).Count -gt 0 -and $sw.Elapsed.TotalSeconds -lt 15) { Start-Sleep -Milliseconds 200 }
  $left = Tree
  if ($left.Count -gt 0) { Write-Host "  killing $($left.Count) leftover process(es)"; $left | Stop-Process -Force; Start-Sleep 1 }
}

if ((Tree).Count -gt 0) { throw "$ProcessName is already running; close it first" }

$results = @()
for ($i = 0; $i -le $Runs; $i++) {
  $sw = [Diagnostics.Stopwatch]::StartNew()
  if ($ExeArgs.Count -gt 0) { Start-Process -FilePath $Exe -ArgumentList $ExeArgs | Out-Null }
  else { Start-Process -FilePath $Exe | Out-Null }

  $visible = $null
  while (-not $visible -and $sw.Elapsed.TotalSeconds -lt $TimeoutSeconds) {
    foreach ($p in (Tree)) {
      $h = $p.MainWindowHandle
      if ($h -ne 0 -and [Perf.Win]::IsWindowVisible($h)) { $visible = $sw.Elapsed.TotalMilliseconds; break }
    }
    if (-not $visible) { Start-Sleep -Milliseconds 10 }
  }
  if (-not $visible) { CloseAll; throw "run ${i}: no visible window within $TimeoutSeconds s" }

  # Settled: the tree's CPU over the last SettleSeconds stays under the threshold.
  $win = [math]::Max(1, [int]($SettleSeconds * 4))
  $hist = New-Object System.Collections.Generic.Queue[object]
  $settled = $null
  while (-not $settled -and $sw.Elapsed.TotalSeconds -lt $TimeoutSeconds) {
    $hist.Enqueue(@($sw.Elapsed.TotalSeconds, (CpuSeconds)))
    while ($hist.Count -gt ($win + 1)) { [void]$hist.Dequeue() }
    if ($hist.Count -eq ($win + 1)) {
      $a = $hist.Peek(); $b = @($hist)[-1]
      $pct = 100.0 * ($b[1] - $a[1]) / ($b[0] - $a[0])
      if ($pct -lt $SettleCorePct) { $settled = $a[0] * 1000.0 }
    }
    Start-Sleep -Milliseconds 250
  }

  $r = [ordered]@{ run = $i; discarded = ($i -eq 0); visible_ms = [math]::Round($visible); settled_ms = if ($settled) { [math]::Round($settled) } else { $null } }
  $results += [pscustomobject]$r
  "run {0}{1}: visible {2} ms, settled {3} ms" -f $i, $(if ($i -eq 0) { ' (discarded)' } else { '' }), $r.visible_ms, $r.settled_ms
  CloseAll
  Start-Sleep -Seconds $PauseSeconds
}

function Median($v) { $a = @($v | Where-Object { $_ -ne $null } | Sort-Object); if ($a.Count -eq 0) { return $null }; $a[[math]::Floor(($a.Count - 1) / 2)] }
$kept = $results | Where-Object { -not $_.discarded }
$summary = [ordered]@{
  exe = $Exe; runs = $Runs
  visible_ms_median = Median ($kept | ForEach-Object { $_.visible_ms })
  settled_ms_median = Median ($kept | ForEach-Object { $_.settled_ms })
  settle_rule = "tree under $SettleCorePct % of one core for $SettleSeconds s"
}
$dir = Split-Path -Parent $Out
if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force $dir | Out-Null }
[ordered]@{ summary = $summary; runs = $results } | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8 "$Out.json"
"median: visible {0} ms, settled {1} ms" -f $summary.visible_ms_median, $summary.settled_ms_median
