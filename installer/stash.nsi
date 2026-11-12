; Stash installer (NSIS 3). Per-user: installs to %LOCALAPPDATA%\Programs\Stash
; without asking for admin rights. Built by installer\build.ps1, which passes
; /DVERSION=<version from Cargo.toml>.

Unicode true
!include "MUI2.nsh"
!include "LogicLib.nsh"

!ifndef VERSION
  !error "Pass /DVERSION=x.y.z (installer\build.ps1 does this)"
!endif

!define APP "Stash"
; Installed as StashDownloads.exe, not stash.exe: Discord's list of games has one
; called "Stash" that it spots as "stash\stash.exe", which is exactly where an older
; installer put us (Programs\Stash\stash.exe), so Discord showed you "playing Stash".
!define EXE "StashDownloads.exe"
; what the build makes, and the name older installers used
!define BUILT "stash.exe"
!define LEGACY "stash.exe"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP}"
!define RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"

Name "${APP}"
OutFile "..\dist\Stash-Setup-${VERSION}.exe"
InstallDir "$LOCALAPPDATA\Programs\${APP}"
InstallDirRegKey HKCU "${UNINSTALL_KEY}" "InstallLocation"
RequestExecutionLevel user
SetCompressor /SOLID lzma
BrandingText "${APP} ${VERSION}"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${APP}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "FileDescription" "${APP} installer"
VIAddVersionKey "CompanyName" "Sandesh Pandey"
VIAddVersionKey "LegalCopyright" "Copyright 2026 Sandesh Pandey. PolyForm Noncommercial 1.0.0"

!define MUI_ICON "..\assets\stash.ico"
!define MUI_UNICON "..\assets\stash.ico"
!define MUI_ABORTWARNING
!define MUI_COMPONENTSPAGE_NODESC
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "Start ${APP} now"
!define MUI_FINISHPAGE_TEXT "${APP} is installed.$\r$\n$\r$\nWindows hides new tray icons behind the arrow next to the clock. To open ${APP} with one click, drag its icon onto the taskbar, or turn it on in Settings under Personalization, Taskbar, Other system tray icons."

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "..\LICENSE.md"
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

; A running copy holds the exe open; close it first (under its old name too).
!macro StopStash
  nsExec::Exec 'taskkill /F /IM ${EXE}'
  Pop $0
  nsExec::Exec 'taskkill /F /IM ${LEGACY}'
  Pop $0
  Sleep 300
!macroend

; ...and wait until it lets go (a copy that's still exiting, or being looked at by
; Windows Error Reporting, keeps the file locked): up to 15 seconds.
; $3 is the file to wait for.
Function WaitForFile
  StrCpy $1 0
  loop:
    IfFileExists "$3" 0 done
    ClearErrors
    FileOpen $2 "$3" a
    IfErrors 0 free
    IntOp $1 $1 + 1
    IntCmp $1 60 done
    nsExec::Exec 'taskkill /F /IM ${EXE}'
    Pop $0
    nsExec::Exec 'taskkill /F /IM ${LEGACY}'
    Pop $0
    Sleep 250
    Goto loop
  free:
    FileClose $2
  done:
FunctionEnd

Section "${APP}" SecApp
  SectionIn RO
  !insertmacro StopStash
  StrCpy $3 "$INSTDIR\${EXE}"
  Call WaitForFile
  StrCpy $3 "$INSTDIR\${LEGACY}"
  Call WaitForFile
  SetOutPath "$INSTDIR"
  File "/oname=${EXE}" "..\target\release\${BUILT}"
  File "..\LICENSE.md"
  ; the old name goes, and whatever pointed at it follows the new one
  Delete "$INSTDIR\${LEGACY}"
  ReadRegStr $0 HKCU "${RUN_KEY}" "${APP}"
  ${If} $0 != ""
    WriteRegStr HKCU "${RUN_KEY}" "${APP}" '"$INSTDIR\${EXE}"'
  ${EndIf}
  ; the browser extensions, to load unpacked until they're in the stores
  SetOutPath "$INSTDIR\browser"
  File "..\browser\README.md"
  File /r "..\browser\dist\chrome"
  File /r "..\browser\dist\firefox"
  SetOutPath "$INSTDIR"
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  CreateShortcut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\${EXE}"

  ; Apps & features entry
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${APP}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\${EXE}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "Sandesh Pandey"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "URLInfoAbout" "https://github.com/frstycodes/stash"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
SectionEnd

; Same value the app's own "Start at Login" menu item writes.
Section "Start ${APP} when I sign in" SecLogin
  WriteRegStr HKCU "${RUN_KEY}" "${APP}" '"$INSTDIR\${EXE}"'
SectionEnd

; An update from inside Stash runs this installer silently (/S): it leaves the sign-in
; setting as the user set it and starts the new Stash when it's done.
Function .onInit
  IfSilent 0 +2
    SectionSetFlags ${SecLogin} 0
FunctionEnd

; --updated: the new Stash waits for the old one to be gone before starting
Function .onInstSuccess
  IfSilent 0 +2
    Exec '"$INSTDIR\${EXE}" --updated'
FunctionEnd

Section "Uninstall"
  !insertmacro StopStash
  RMDir /r "$INSTDIR\browser"
  Delete "$INSTDIR\${EXE}"
  Delete "$INSTDIR\${LEGACY}"
  Delete "$INSTDIR\LICENSE.md"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${APP}.lnk"
  DeleteRegValue HKCU "${RUN_KEY}" "${APP}"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  ; Settings in %APPDATA%\Stash are kept, as most apps do.
SectionEnd
