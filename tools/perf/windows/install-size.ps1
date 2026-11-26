<#
.SYNOPSIS
  Install size of a client: the bytes and the number of files under a folder (or of one file). Runs nothing.
  The twin of ..\install-size.sh.

.EXAMPLE
  .\install-size.ps1 "$env:LOCALAPPDATA\Programs\decibell-electron-client"
  .\install-size.ps1 .\gpui-client\target\release\decibell.exe C:\dev\vcpkg-gpui\installed\x64-windows\bin
#>
param([Parameter(Mandatory, ValueFromRemainingArguments)] [string[]] $Path)
$ErrorActionPreference = 'Stop'
$total = 0; $count = 0
foreach ($p in $Path) {
  $files = @(Get-ChildItem -LiteralPath $p -Recurse -File -Force -ErrorAction Stop)
  if ((Get-Item -LiteralPath $p) -isnot [System.IO.DirectoryInfo]) { $files = @(Get-Item -LiteralPath $p) }
  $bytes = ($files | Measure-Object Length -Sum).Sum
  "{0,10:N1} MB  {1,6} files  {2}" -f ($bytes / 1MB), $files.Count, $p
  $total += $bytes; $count += $files.Count
}
"{0,10:N1} MB  {1,6} files  total" -f ($total / 1MB), $count
