; The GPUI client's Windows installer (gpui-client/PACKAGING.md, "Windows").
;
; NSIS, as Electron's (electron-builder generates its script; it can't package a native executable,
; so this one is written by hand with the same behaviour): per user, no administrator rights,
; %LOCALAPPDATA%\Programs\<dir>, a Start menu shortcut, "Apps & features" entry, the decibell:// keys
; HANDOFF.md §6 lists, and an uninstaller that removes exactly what was installed (the profile, like
; Electron's, stays).
;
;   makensis /DVERSION=0.9.4 /DVERSION4=0.9.4.0 /DSTAGE=<staged dir> /DICON=<icon.ico> \
;            /DOUTFILE=<Decibell-0.9.4-x64-setup.exe> packaging\windows\decibell.nsi
;
; Silent: `/S`. The updater (decibell-release, `apply`) runs the next version's installer as
; `/S /UPDATE /D=<install dir>` while the previous version (which supervises a trial start of the new
; one) still runs from the install folder: /UPDATE moves every installed file into `previous\`
; (Windows lets a running image be renamed), then installs; the new version removes `previous\` at
; its next start, or the supervisor moves it back if the new one doesn't start.

Unicode true
!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"

!macro REQUIRE SYMBOL
  !ifndef ${SYMBOL}
    !error "makensis /DVERSION=<x.y.z> /DVERSION4=<x.y.z.0> /DSTAGE=<dir> /DICON=<ico> /DOUTFILE=<exe> decibell.nsi (${SYMBOL} is missing)"
  !endif
!macroend
!insertmacro REQUIRE VERSION
!insertmacro REQUIRE VERSION4
!insertmacro REQUIRE STAGE
!insertmacro REQUIRE ICON
!insertmacro REQUIRE OUTFILE

!define PRODUCT "Decibell"
; While the Electron client ships, its installer owns %LOCALAPPDATA%\Programs\decibell-electron-client
; (electron-builder's one-click default: the package name) and the shortcut "Decibell": the native client
; installs beside it under its own names (owner decision at the switch-over).
!define DIR_NAME "Decibell Native"
!define SHORTCUT "Decibell Native"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\DecibellNative"
!define PROTOCOL_KEY "Software\Classes\decibell"
!define UNINSTALLER "Uninstall Decibell.exe"

Name "${PRODUCT}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\${DIR_NAME}"
RequestExecutionLevel user
SetCompressor /SOLID lzma
ShowInstDetails nevershow
ShowUninstDetails nevershow
BrandingText " "
ManifestDPIAware true

VIProductVersion "${VERSION4}"
VIFileVersion "${VERSION4}"
VIAddVersionKey "ProductName" "${PRODUCT}"
VIAddVersionKey "CompanyName" "${PRODUCT}"
VIAddVersionKey "FileDescription" "${PRODUCT} installer"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "Copyright (c) 2026 ${PRODUCT}"

!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_FINISHPAGE_RUN "$INSTDIR\decibell.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Start Decibell"
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Var Update

Function .onInit
  ${GetParameters} $R0
  ClearErrors
  ${GetOptions} $R0 "/UPDATE" $R1
  ${IfNot} ${Errors}
    StrCpy $Update "1"
    SetSilent silent
  ${EndIf}
FunctionEnd

; Waits up to 30 s for decibell.exe to be writable (a version that is closing), else gives up.
Function WaitForExit
  StrCpy $R9 0
  ${DoWhile} $R9 < 60
    ClearErrors
    FileOpen $R8 "$INSTDIR\decibell.exe" a
    ${IfNot} ${Errors}
      FileClose $R8
      Return
    ${EndIf}
    Sleep 500
    IntOp $R9 $R9 + 1
  ${Loop}
  MessageBox MB_OK|MB_ICONSTOP "Decibell is still running. Close it and run the installer again." /SD IDOK
  Abort
FunctionEnd

Section "Install"
  SetShellVarContext current
  ${If} $Update == "1"
    ; The supervising previous version runs from here: move everything aside (renaming a running
    ; image is allowed), keeping a leftover `previous\` of an earlier update out of the way first.
    RMDir /r "$INSTDIR\previous"
    CreateDirectory "$INSTDIR\previous"
    FindFirst $0 $1 "$INSTDIR\*"
    ${DoWhile} $1 != ""
      ${If} $1 != "."
      ${AndIf} $1 != ".."
      ${AndIf} $1 != "previous"
        Rename "$INSTDIR\$1" "$INSTDIR\previous\$1"
      ${EndIf}
      FindNext $0 $1
    ${Loop}
    FindClose $0
  ${ElseIf} ${FileExists} "$INSTDIR\decibell.exe"
    Call WaitForExit
  ${EndIf}

  SetOutPath "$INSTDIR"
  File /r "${STAGE}\*"
  WriteUninstaller "$INSTDIR\${UNINSTALLER}"

  ; decibell:// (HANDOFF.md §6): the app itself never writes the registry.
  WriteRegStr HKCU "${PROTOCOL_KEY}" "" "URL:Decibell"
  WriteRegStr HKCU "${PROTOCOL_KEY}" "URL Protocol" ""
  WriteRegStr HKCU "${PROTOCOL_KEY}\DefaultIcon" "" '"$INSTDIR\decibell.exe",0'
  WriteRegStr HKCU "${PROTOCOL_KEY}\shell\open\command" "" '"$INSTDIR\decibell.exe" "%1"'

  ; Apps & features.
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${PRODUCT}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "${PRODUCT}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" '"$INSTDIR\decibell.exe",0'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\${UNINSTALLER}"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\${UNINSTALLER}" /S'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "URLInfoAbout" "https://github.com/sunkhan/decibell"
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "EstimatedSize" $0

  ; The Start menu shortcut carries the AppUserModelID the app sets at start (so the taskbar groups
  ; and pins them together); NSIS can't set that property, the app writes the shortcut itself.
  ExecWait '"$INSTDIR\decibell.exe" --create-shortcut "$SMPROGRAMS\${SHORTCUT}.lnk"' $0
  ${If} $0 != 0
    CreateShortcut "$SMPROGRAMS\${SHORTCUT}.lnk" "$INSTDIR\decibell.exe"
  ${EndIf}
SectionEnd

Section "Uninstall"
  SetShellVarContext current
  ClearErrors
  FileOpen $0 "$INSTDIR\decibell.exe" a
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "Decibell is running. Close it, then uninstall." /SD IDOK
    Abort
  ${EndIf}
  FileClose $0

  Delete "$SMPROGRAMS\${SHORTCUT}.lnk"
  ; decibell:// only if it is still ours (Electron's client may have taken it since).
  ReadRegStr $0 HKCU "${PROTOCOL_KEY}\shell\open\command" ""
  ${If} $0 == '"$INSTDIR\decibell.exe" "%1"'
    DeleteRegKey HKCU "${PROTOCOL_KEY}"
  ${EndIf}
  DeleteRegKey HKCU "${UNINSTALL_KEY}"

  ; Exactly what the installer put here (no recursive delete of a folder a user may have chosen).
  Delete "$INSTDIR\decibell.exe"
  Delete "$INSTDIR\*.dll"
  RMDir /r "$INSTDIR\resources"
  RMDir /r "$INSTDIR\licenses"
  RMDir /r "$INSTDIR\previous"
  Delete "$INSTDIR\${UNINSTALLER}"
  RMDir "$INSTDIR"
SectionEnd
