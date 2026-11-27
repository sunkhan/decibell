<#
.SYNOPSIS
  Installs the Windows installer silently into a temporary folder, checks what it put where (files,
  the decibell:// keys HANDOFF.md §6 lists, Apps & features, the Start menu shortcut with the
  AppUserModelID), runs `decibell.exe --version` from the install (every DLL it imports must load),
  then uninstalls silently and checks that nothing is left. For CI (the dry run) and the owner.
#>
param([Parameter(Mandatory)] [string] $Installer, [Parameter(Mandatory)] [string] $Version)
$ErrorActionPreference = 'Stop'
$dir = Join-Path $env:RUNNER_TEMP "decibell-install-test"
if (-not $env:RUNNER_TEMP) { $dir = Join-Path $env:TEMP "decibell-install-test" }
if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }
function Fail($m) { Write-Host "::error::$m"; throw $m }

$p = Start-Process -FilePath $Installer -ArgumentList '/S', "/D=$dir" -Wait -PassThru
if ($p.ExitCode -ne 0) { Fail "the installer exited $($p.ExitCode)" }
foreach ($f in 'decibell.exe', 'resources\install.json', 'licenses\THIRD_PARTY_LICENSES.md', 'Uninstall Decibell.exe') {
  if (-not (Test-Path "$dir\$f")) { Fail "not installed: $f" }
}
$kind = Get-Content "$dir\resources\install.json" -Raw | ConvertFrom-Json
if ($kind.kind -ne 'nsis' -or $kind.version -ne $Version) { Fail "install.json says $($kind.kind) $($kind.version)" }

$key = 'HKCU:\Software\Classes\decibell'
$want = @{
  "$key" = @{ '(default)' = 'URL:Decibell'; 'URL Protocol' = '' }
  "$key\DefaultIcon" = @{ '(default)' = "`"$dir\decibell.exe`",0" }
  "$key\shell\open\command" = @{ '(default)' = "`"$dir\decibell.exe`" `"%1`"" }
}
foreach ($path in $want.Keys) {
  foreach ($name in $want[$path].Keys) {
    $value = if ($name -eq '(default)') { (Get-Item $path).GetValue('') } else { (Get-ItemProperty $path).$name }
    if ($value -ne $want[$path][$name]) { Fail "$path [$name] = '$value', expected '$($want[$path][$name])'" }
  }
}
$uninstall = Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\DecibellNative'
if ($uninstall.DisplayVersion -ne $Version) { Fail "Apps & features shows $($uninstall.DisplayVersion)" }

$lnk = Join-Path ([Environment]::GetFolderPath('Programs')) 'Decibell Native.lnk'
if (-not (Test-Path $lnk)) { Fail "no Start menu shortcut at $lnk" }
$shell = New-Object -ComObject Shell.Application
$item = $shell.NameSpace((Split-Path $lnk)).ParseName((Split-Path $lnk -Leaf))
$aumid = $item.ExtendedProperty('System.AppUserModel.ID')
Write-Host "shortcut AppUserModelID: '$aumid'"
if ($aumid -ne 'com.decibell.native') { Fail "the shortcut's AppUserModelID is '$aumid'" }

$out = Join-Path $env:TEMP 'decibell-version.txt'
$v = Start-Process -FilePath "$dir\decibell.exe" -ArgumentList '--version' -Wait -PassThru -NoNewWindow -RedirectStandardOutput $out
if ($v.ExitCode -ne 0) { Fail "decibell.exe --version exited $($v.ExitCode) (a DLL that doesn't load: 0xC0000135)" }
$said = (Get-Content $out -Raw).Trim()
if ($said -ne "decibell $Version") { Fail "--version said '$said'" }
Write-Host "installed and ran: $said"

$u = Start-Process -FilePath "$dir\Uninstall Decibell.exe" -ArgumentList '/S' -Wait -PassThru
if ($u.ExitCode -ne 0) { Fail "the uninstaller exited $($u.ExitCode)" }
# NSIS's uninstaller copies itself to %TEMP% and returns at once: wait for the folder to go.
for ($i = 0; $i -lt 60 -and (Test-Path "$dir\decibell.exe"); $i++) { Start-Sleep -Milliseconds 500 }
Start-Sleep -Seconds 2
if (Test-Path $dir) { Fail "left behind: $((Get-ChildItem -Recurse $dir | Select-Object -ExpandProperty FullName) -join ', ')" }
if (Test-Path $key) { Fail "decibell:// is still registered" }
if (Test-Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\DecibellNative') { Fail "still in Apps & features" }
if (Test-Path $lnk) { Fail "the shortcut is still there" }
Write-Host "uninstalled cleanly"
