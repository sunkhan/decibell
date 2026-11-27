<#
.SYNOPSIS
  The Windows installer of the GPUI client (gpui-client/PACKAGING.md, "Windows"): stages decibell.exe,
  every DLL it needs (packaging/windows/dlls.py walks the imports through vcpkg's install tree and the
  Visual C++ runtime), resources\install.json (kind nsis) and the baked resources, the licences; then
  builds Decibell-<v>-x64-setup.exe with NSIS (decibell.nsi).

.EXAMPLE
  packaging\windows\package.ps1 -Binary target\release\decibell.exe -Out target\packages [-Resources target\resources]
  (from gpui-client\, with VCPKG_ROOT set and makensis on PATH)
#>
param(
  [Parameter(Mandatory)] [string] $Binary,
  [Parameter(Mandatory)] [string] $Out,
  [string] $Resources = "",
  [string] $Vcpkg = $env:VCPKG_ROOT
)
$ErrorActionPreference = 'Stop'
$client = (Resolve-Path "$PSScriptRoot\..\..").Path
$repo = (Resolve-Path "$client\..").Path
$version = (Get-Content "$repo\electron-client\package.json" -Raw | ConvertFrom-Json).version
$version4 = (($version -split '[-+]')[0]) + ".0"
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$stage = Join-Path $Out "stage"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force -Path "$stage\resources", "$stage\licenses" | Out-Null

Copy-Item $Binary "$stage\decibell.exe"
# The binary is a ship build of this version (Git for Windows' bash runs the shared check).
& bash "$client/packaging/check-ship.sh" "$stage/decibell.exe" $version
if ($LASTEXITCODE -ne 0) { throw "check-ship failed" }

[IO.File]::WriteAllText("$stage\resources\install.json", "{`"kind`":`"nsis`",`"version`":`"$version`"}`n")
foreach ($f in 'sentry.json', 'gifs.json') {
  if ($Resources -and (Test-Path "$Resources\$f")) { Copy-Item "$Resources\$f" "$stage\resources\$f" }
}

# The Visual C++ runtime, app-local, from this machine's Visual Studio (vswhere).
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$redist = $null
if (Test-Path $vswhere) {
  $vs = & $vswhere -latest -products * -property installationPath
  $redist = Get-ChildItem "$vs\VC\Redist\MSVC\*\x64\Microsoft.VC14*.CRT" -Directory -ErrorAction SilentlyContinue |
    Sort-Object FullName -Descending | Select-Object -First 1
}
$dllArgs = @('--exe', "$stage\decibell.exe", '--search', "$Vcpkg\installed\x64-windows\bin",
             '--system', "$env:SystemRoot\System32", '--copy-to', $stage, '--list', "$Out\dlls.txt")
if ($redist) { $dllArgs += @('--redist', $redist.FullName) }
& python -I "$client\packaging\windows\dlls.py" @dllArgs
if ($LASTEXITCODE -ne 0) { throw "dlls.py failed" }

& python -I "$client\packaging\licenses.py" --target x86_64-pc-windows-msvc --vcpkg "$Vcpkg\installed\x64-windows" `
  --version $version --out "$stage\licenses\THIRD_PARTY_LICENSES.md"
if ($LASTEXITCODE -ne 0) { throw "licenses.py failed" }

$setup = Join-Path $Out "Decibell-$version-x64-setup.exe"
& makensis /V2 "/DVERSION=$version" "/DVERSION4=$version4" "/DSTAGE=$stage" `
  "/DICON=$repo\electron-client\resources\icon.ico" "/DOUTFILE=$setup" "$client\packaging\windows\decibell.nsi"
if ($LASTEXITCODE -ne 0) { throw "makensis failed" }
Get-Item $setup | Select-Object Name, Length
Get-ChildItem $stage -Recurse -File | Select-Object @{n='File';e={$_.FullName.Substring($stage.Length + 1)}}, Length
