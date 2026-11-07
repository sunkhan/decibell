<#
.SYNOPSIS
  Sets up a Windows machine to build Decibell's clients: the Electron native addon (electron-client/HANDOFF.md
  §6.1) and the Qt client (qt-client/HANDOFF.md). Idempotent; re-run to resume after a failure.

.DESCRIPTION
  Installs per user, portable, into C:\dev (no admin), except Visual Studio 2022 Build Tools (winget, UAC):
    VS 2022 Build Tools (C++ workload: MSVC, Windows SDK, CMake/Ninja component)
    Node 20 (CI pins 20), CMake 4.x, protoc 27.x (CI pins 27.x), LLVM (libclang for ffmpeg-sys-next's bindgen),
    uv (runs Python tools with a managed interpreter), Rust stable (MSVC host),
    vcpkg at CI's pinned commit + ffmpeg[nvcodec,amf,qsv]:x64-windows (~11 min on a 12700K),
    Qt (default 6.12.0 msvc2022_64 + qtshadertools + qtimageformats) into C:\Qt via tools/qt/fetch_qt.py.
  Sets user env: PATH (+ node, cmake, protoc, uv, cargo), VCPKG_ROOT, LIBCLANG_PATH.
  Per build shell you still need CMAKE_POLICY_VERSION_MINIMUM=3.5 (audiopus_sys vs CMake 4), as CI does.

  Native tools run through `cmd /c ... 2>&1`: under $ErrorActionPreference = 'Stop' PowerShell 5.1 turns any
  stderr line of a native tool (rustup's "info:" lines) into a terminating error.
#>
param(
  [string] $Dev = 'C:\dev',
  [string] $QtVersion = '6.12.0',
  [switch] $SkipVisualStudio,
  [switch] $SkipFFmpeg,
  [switch] $SkipQt
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$dl = Join-Path $Dev '_downloads'
New-Item -ItemType Directory -Force $Dev, $dl | Out-Null

$NodeVer = '20.20.2'; $CMakeVer = '4.4.5'; $ProtocVer = '27.5'; $LlvmVer = '23.1.5'
$VcpkgCommit = 'e5a1490e1409d175932ef6014519e9ae149ddb7c'   # same as .github/workflows/*.yml

function Fetch($url, $name) {
  $out = Join-Path $dl $name
  if (-not (Test-Path $out)) { Write-Host "download $name"; Invoke-WebRequest -UseBasicParsing $url -OutFile $out }
  $out
}
function ExtractZipTo($zip, $dest) {
  if (Test-Path $dest) { return }
  $tmp = "$dest.tmp"; if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
  Expand-Archive $zip $tmp
  $kids = @(Get-ChildItem $tmp)
  if ($kids.Count -eq 1 -and $kids[0].PSIsContainer) { Move-Item $kids[0].FullName $dest; Remove-Item $tmp } else { Move-Item $tmp $dest }
  Write-Host "installed $dest"
}
function Run($cmdline) { cmd /c "$cmdline 2>&1" | ForEach-Object { "  $_" }; if ($LASTEXITCODE -ne 0) { throw "failed: $cmdline" } }

if (-not $SkipVisualStudio) {
  $vsw = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
  $have = (Test-Path $vsw) -and (& $vsw -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath)
  if (-not $have) {
    Write-Host 'installing VS 2022 Build Tools (a UAC prompt will appear)'
    Run 'winget install --id Microsoft.VisualStudio.2022.BuildTools -e --source winget --accept-package-agreements --accept-source-agreements --override "--passive --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"'
  }
}

ExtractZipTo (Fetch "https://nodejs.org/dist/v$NodeVer/node-v$NodeVer-win-x64.zip" "node-$NodeVer.zip") "$Dev\node20"
ExtractZipTo (Fetch "https://github.com/Kitware/CMake/releases/download/v$CMakeVer/cmake-$CMakeVer-windows-x86_64.zip" "cmake-$CMakeVer.zip") "$Dev\cmake"
ExtractZipTo (Fetch "https://github.com/protocolbuffers/protobuf/releases/download/v$ProtocVer/protoc-$ProtocVer-win64.zip" "protoc-$ProtocVer.zip") "$Dev\protoc"
ExtractZipTo (Fetch 'https://github.com/astral-sh/uv/releases/latest/download/uv-x86_64-pc-windows-msvc.zip' 'uv.zip') "$Dev\uv"

if (-not (Test-Path "$Dev\llvm\bin\libclang.dll")) {
  # Windows' bsdtar can't decode LLVM's .tar.zst (zstd window too large), so take the .tar.xz.
  $xz = Fetch "https://github.com/llvm/llvm-project/releases/download/llvmorg-$LlvmVer/clang+llvm-$LlvmVer-x86_64-pc-windows-msvc.tar.xz" "llvm-$LlvmVer.tar.xz"
  $tmp = "$Dev\llvm.tmp"; if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
  New-Item -ItemType Directory -Force $tmp | Out-Null
  Run "tar -xf `"$xz`" -C `"$tmp`""
  Move-Item (Get-ChildItem $tmp | Select-Object -First 1).FullName "$Dev\llvm"; Remove-Item -Recurse -Force $tmp
  Write-Host "installed $Dev\llvm"
}

$cargoBin = "$env:USERPROFILE\.cargo\bin"
if (-not (Test-Path "$cargoBin\rustup.exe")) {
  $ri = Fetch 'https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe' 'rustup-init.exe'
  Run "`"$ri`" -y --no-modify-path --default-toolchain none --default-host x86_64-pc-windows-msvc"
}
Run "`"$cargoBin\rustup.exe`" toolchain install stable-x86_64-pc-windows-msvc --profile minimal -c clippy -c rustfmt"
Run "`"$cargoBin\rustup.exe`" default stable-x86_64-pc-windows-msvc"

if (-not (Test-Path "$Dev\vcpkg\.git")) { Run "git clone --quiet https://github.com/microsoft/vcpkg `"$Dev\vcpkg`"" }
Run "git -C `"$Dev\vcpkg`" fetch --quiet origin"
Run "git -C `"$Dev\vcpkg`" checkout --quiet $VcpkgCommit"
if (-not (Test-Path "$Dev\vcpkg\vcpkg.exe")) { Run "`"$Dev\vcpkg\bootstrap-vcpkg.bat`" -disableMetrics" }
if (-not $SkipFFmpeg) { Run "`"$Dev\vcpkg\vcpkg.exe`" install ffmpeg[nvcodec,amf,qsv]:x64-windows" }

if (-not $SkipQt) {
  $env:UV_PYTHON_INSTALL_DIR = "$Dev\uv\python"; $env:UV_CACHE_DIR = "$Dev\uv\cache"
  if (-not (Test-Path "C:\Qt\$QtVersion\msvc2022_64\bin\qtpaths.exe")) {
    Run "`"$Dev\uv\uv.exe`" run --no-project --python 3.12 --with py7zr --with truststore python -I `"$repo\tools\qt\fetch_qt.py`" --version $QtVersion --arch msvc2022_64 --dest C:\Qt --addons qtshadertools qtimageformats"
  }
}

$add = @("$Dev\node20", "$Dev\cmake\bin", "$Dev\protoc\bin", "$Dev\uv", $cargoBin)
$cur = [Environment]::GetEnvironmentVariable('Path', 'User'); if (-not $cur) { $cur = '' }
$parts = @($cur.Split(';') | Where-Object { $_ })
foreach ($a in $add) { if ($parts -notcontains $a) { $parts += $a } }
[Environment]::SetEnvironmentVariable('Path', ($parts -join ';'), 'User')
[Environment]::SetEnvironmentVariable('VCPKG_ROOT', "$Dev\vcpkg", 'User')
[Environment]::SetEnvironmentVariable('LIBCLANG_PATH', "$Dev\llvm\bin", 'User')
Write-Host 'done. Open a new shell for the PATH change; set CMAKE_POLICY_VERSION_MINIMUM=3.5 in build shells.'
