<#
.SYNOPSIS
  Attributes a client's wakeups to threads: context switches per second per thread, with the thread's name
  (SetThreadDescription) and the module + offset its start address falls in.

.DESCRIPTION
  measure.ps1 says how often a client wakes; this says who. Two raw-counter snapshots of every thread of every
  process named -ProcessName, -Seconds apart; Electron processes are labelled by their --type.
  Found with it (2026-11-07): Qt's QDxgiVSyncThread waking on every vblank (279/s on a 279 Hz panel; off with
  QT_D3D_NO_VBLANK_THREAD=1) and NVIDIA's D3D11 driver thread (nvwgf2umx.dll, ~80/s in any D3D11 process with a
  swap chain, Electron's GPU and utility processes included).

.EXAMPLE
  .\threads.ps1 -ProcessName decibell-qt -Seconds 10 -Top 8
#>
param(
  [Parameter(Mandatory)] [string] $ProcessName,
  [int] $Seconds = 10,
  [int] $Top = 10
)
$ErrorActionPreference = 'Stop'
if (-not ('PerfThreads.Native' -as [type])) {
  Add-Type -Namespace PerfThreads -Name Native -MemberDefinition @'
[DllImport("kernel32.dll")] public static extern IntPtr OpenThread(int access, bool inherit, int tid);
[DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
[DllImport("ntdll.dll")] public static extern int NtQueryInformationThread(IntPtr h, int cls, out IntPtr info, int len, IntPtr retLen);
[DllImport("kernel32.dll")] public static extern int GetThreadDescription(IntPtr h, out IntPtr desc);
[DllImport("kernel32.dll")] public static extern IntPtr LocalFree(IntPtr p);
'@
}
$procs = @(Get-Process -Name $ProcessName -ErrorAction SilentlyContinue)
if ($procs.Count -eq 0) { throw "no process named $ProcessName" }

function Snap {
  $h = @{}
  foreach ($pp in $procs) {
    Get-CimInstance Win32_PerfRawData_PerfProc_Thread -Filter "IDProcess=$($pp.Id)" |
      ForEach-Object { $h["$($pp.Id):$($_.IDThread)"] = [uint64]$_.ContextSwitchesPersec }
  }
  $h
}
$a = Snap; Start-Sleep -Seconds $Seconds; $b = Snap

$rows = foreach ($pp in $procs) {
  $pp.Refresh()
  $mods = @($pp.Modules | ForEach-Object { [pscustomobject]@{ Name = $_.ModuleName; Base = $_.BaseAddress.ToInt64(); End = $_.BaseAddress.ToInt64() + $_.ModuleMemorySize } })
  $cmd = (Get-CimInstance Win32_Process -Filter "ProcessId=$($pp.Id)").CommandLine
  $type = if ($cmd -match '--type=([a-z-]+)') { $Matches[1] } else { 'main' }
  foreach ($t in $pp.Threads) {
    $k = "$($pp.Id):$($t.Id)"
    if (-not ($a.ContainsKey($k) -and $b.ContainsKey($k))) { continue }
    $h = [PerfThreads.Native]::OpenThread(0x0840, $false, $t.Id)   # QUERY_INFORMATION | QUERY_LIMITED_INFORMATION
    $sa = 0; $name = ''
    if ($h -ne [IntPtr]::Zero) {
      $info = [IntPtr]::Zero
      if ([PerfThreads.Native]::NtQueryInformationThread($h, 9, [ref]$info, 8, [IntPtr]::Zero) -eq 0) { $sa = $info.ToInt64() }
      $d = [IntPtr]::Zero
      if ([PerfThreads.Native]::GetThreadDescription($h, [ref]$d) -ge 0 -and $d -ne [IntPtr]::Zero) {
        $name = [Runtime.InteropServices.Marshal]::PtrToStringUni($d); [void][PerfThreads.Native]::LocalFree($d)
      }
      [void][PerfThreads.Native]::CloseHandle($h)
    }
    $m = $mods | Where-Object { $sa -ge $_.Base -and $sa -lt $_.End } | Select-Object -First 1
    [pscustomobject]@{
      Proc = $type; Pid = $pp.Id; Tid = $t.Id
      PerSec = [math]::Round(($b[$k] - $a[$k]) / $Seconds, 1)
      Name = $name; Module = $m.Name; Offset = if ($m) { '+0x{0:x}' -f ($sa - $m.Base) } else { '' }
    }
  }
}
"{0}: {1} context switches/s over {2} s" -f $ProcessName, [math]::Round(($rows | Measure-Object PerSec -Sum).Sum, 1), $Seconds
$rows | Group-Object Proc | ForEach-Object { "  {0,-18} {1,8}/s" -f $_.Name, [math]::Round(($_.Group | Measure-Object PerSec -Sum).Sum, 1) }
$rows | Sort-Object PerSec -Descending | Select-Object -First $Top | Format-Table -AutoSize
