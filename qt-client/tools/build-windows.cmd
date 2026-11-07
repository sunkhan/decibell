@echo off
rem Builds the Qt client on Windows: tools\build-windows.cmd [Release|Debug] [build dir]
rem Needs tools\windows\setup-dev.ps1 done once. Keep the build dir short (MAX_PATH, see HANDOFF.md).
setlocal
set CFG=%~1
if "%CFG%"=="" set CFG=Release
set BUILD=%~2
if "%BUILD%"=="" set BUILD=C:\b\qt
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" >nul || exit /b 1
set PATH=C:\dev\cmake\bin;C:\dev\node20;%USERPROFILE%\.cargo\bin;%PATH%
set CMAKE_PREFIX_PATH=C:\Qt\6.12.0\msvc2022_64
set QMAKE=C:\Qt\6.12.0\msvc2022_64\bin\qmake.exe
rem FFmpeg for the video module: the vcpkg build the Electron native addon links. Not VCPKG_ROOT: vcvars64 points
rem that at Visual Studio's own vcpkg.
if not defined DECIBELL_FFMPEG_ROOT set DECIBELL_FFMPEG_ROOT=C:\dev\vcpkg\installed\x64-windows
node "%~dp0fetch-assets.mjs" || exit /b 1
node "%~dp0gen-theme\gen-theme.mjs" --check || exit /b 1
cmake -S "%~dp0.." -B "%BUILD%" -G Ninja -DCMAKE_BUILD_TYPE=%CFG% "-DDECIBELL_FFMPEG_ROOT=%DECIBELL_FFMPEG_ROOT%" || exit /b 1
cmake --build "%BUILD%" || exit /b 1
echo QT_CLIENT_BUILD_OK %BUILD%\decibell-qt.exe
