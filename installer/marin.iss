; ---------------------------------------------------------------------------
; MarinExplorer installer
;
; Built by GitHub Actions (see .github/workflows/build.yml) with Inno Setup 7.
; One architecture per installer — the payload exe is not interchangeable, so
; each setup ships the build that matches the machine it will run on:
;
;   ISCC.exe marin.iss /DMyAppVersion=1.2.3 /DMyAppArch=x64-msvc  (MSVC x64)
;   ISCC.exe marin.iss /DMyAppVersion=1.2.3 /DMyAppArch=arm64    (MSVC ARM64)
;
; The version can also be edited here for local builds.
; ---------------------------------------------------------------------------

#ifndef MyAppVersion
  #define MyAppVersion "0.1.3"
#endif

; `VersionInfoVersion` must be strictly numeric (x[.x.x.x]), while a display
; version may carry a suffix ("1.2.3-beta"). Strip anything from "-" onwards so
; a pre-release tag can never break the build.
#if Pos("-", MyAppVersion) > 0
  #define MyNumericVersion Copy(MyAppVersion, 1, Pos("-", MyAppVersion) - 1)
#else
  #define MyNumericVersion MyAppVersion
#endif

#define MyAppName "MarinExplorer"
#define MyAppPublisher "Rei1mu"
#define MyAppURL "https://github.com/Rei1mu/MarinExplorer"
; Target architecture: "x64" or "arm64".
#ifndef MyAppArch
  #define MyAppArch "x64"
#endif

#if MyAppArch == "arm64"
  ; Refuse to install the ARM64 payload on an x64 machine (and vice versa) —
  ; a mismatched exe fails at launch with an unhelpful OS error, so block it
  ; during setup instead.
  #define MyArchAllowed "arm64"
  #ifndef MyAppExe
    #define MyAppExe "..\target\aarch64-pc-windows-msvc\release\MarinExplorer.exe"
  #endif
#elif MyAppArch == "x64-msvc"
  ; `x64os` = a genuine 64-bit x86 Windows (refuses ARM64, like the GNU x64 build).
  #define MyArchAllowed "x64os"
  ; The exe bundled into the installer is the x64 MSVC build.
  #ifndef MyAppExe
    #define MyAppExe "..\target\x86_64-pc-windows-msvc\release\MarinExplorer.exe"
  #endif
#else
  #error "Unknown MyAppArch — expected \"x64-msvc\" or \"arm64\""
#endif

[Setup]
AppId=MarinExplorer
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}/issues
AppUpdatesURL={#MyAppURL}/releases
VersionInfoVersion={#MyNumericVersion}

; Default install location: the per-user "Programs" directory.
; Why not Program Files: MarinExplorer keeps its own store (folder-size cache)
; next to the executable so the install stays self-contained. Program Files is
; not writable without elevation, which would silently push the cache back
; into %LOCALAPPDATA% — exactly what we want to avoid. This location needs no
; administrator rights and is writable by the installing user.
DefaultDirName={localappdata}\Programs\{#MyAppName}
; Install for the current user only; never ask for elevation.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog

DefaultGroupName={#MyAppName}
LicenseFile=..\LICENSE
WizardStyle=modern
; Let the user pick another drive/directory.
DisableDirPage=no
DisableProgramGroupPage=yes
OutputDir=Output
OutputBaseFilename={#MyAppName}-Setup-{#MyAppArch}-{#MyAppVersion}
; A per-user install still gets its own Add/Remove Programs entry.
CreateUninstallRegKey=yes
UninstallDisplayName={#MyAppName} {#MyAppVersion}
SolidCompression=yes
; Refuse to launch on a machine that cannot run this payload.
ArchitecturesAllowed={#MyArchAllowed}
ArchitecturesInstallIn64BitMode=x64os arm64
SetupIconFile=..\logo.ico
UninstallDisplayIcon={app}\MarinExplorer.exe,0

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "japanese"; MessagesFile: "compiler:Languages\Japanese.isl"
Name: "simplifiedchinese"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"

; ---------------------------------------------------------------------------
; Additional options — both are ticked on a fresh install (`checkedonce`
; remembers the choice on later upgrades instead of forcing it again).
; The descriptions stay plain literals: no `{#...}` constants here, because a
; preprocessor hiccup on the tasks page is invisible until a user hits it.
; ---------------------------------------------------------------------------
[Tasks]
Name: "startmenu"; Description: "加入开始菜单 / Add to Start menu"; GroupDescription: "附加选项 / Additional options:"; Flags: checkedonce
Name: "contextmenu"; Description: "在右键菜单中加入「通过 MarinExplorer 打开」/ Add ""Open with MarinExplorer"" to the right-click menu"; GroupDescription: "附加选项 / Additional options:"; Flags: checkedonce
Name: "desktopicon"; Description: "创建桌面快捷方式 / Create a desktop shortcut"; GroupDescription: "附加选项 / Additional options:"

[Files]
Source: "{#MyAppExe}"; DestDir: "{app}"; DestName: "MarinExplorer.exe"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\MarinExplorer.exe"; Tasks: startmenu
Name: "{group}\卸载 {#MyAppName} / Uninstall {#MyAppName}"; Filename: "{uninstallexe}"; Tasks: startmenu
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\MarinExplorer.exe"; Flags: createonlyiffileexists; Tasks: desktopicon

; ---------------------------------------------------------------------------
; Right-click menu (per-user hive: no elevation needed).
; These are the exact same keys the app writes from Settings → 集成, so the
; toggle in the app keeps working afterwards and uninstall cleans up here.
;   *            → any file              (%1 = the file)
;   Directory    → a folder              (%1 = the folder)
;   Directory\Background → empty area    (%V = the open folder)
;   Drive        → a disk                (%1 = the drive)
; ---------------------------------------------------------------------------
[Registry]
; --- files ---
Root: HKCU; Subkey: "Software\Classes\*\shell\{#MyAppName}"; ValueType: string; ValueData: "通过 {#MyAppName} 打开"; Tasks: contextmenu; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\*\shell\{#MyAppName}"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\MarinExplorer.exe,0"; Tasks: contextmenu; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\*\shell\{#MyAppName}\command"; ValueType: string; ValueData: """{app}\MarinExplorer.exe"" ""%1"""; Tasks: contextmenu; Flags: uninsdeletekey
; --- folders ---
Root: HKCU; Subkey: "Software\Classes\Directory\shell\{#MyAppName}"; ValueType: string; ValueData: "通过 {#MyAppName} 打开"; Tasks: contextmenu; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Directory\shell\{#MyAppName}"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\MarinExplorer.exe,0"; Tasks: contextmenu; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Directory\shell\{#MyAppName}\command"; ValueType: string; ValueData: """{app}\MarinExplorer.exe"" ""%1"""; Tasks: contextmenu; Flags: uninsdeletekey
; --- empty area inside a folder ---
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\{#MyAppName}"; ValueType: string; ValueData: "通过 {#MyAppName} 打开"; Tasks: contextmenu; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\{#MyAppName}"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\MarinExplorer.exe,0"; Tasks: contextmenu; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\{#MyAppName}\command"; ValueType: string; ValueData: """{app}\MarinExplorer.exe"" ""%V"""; Tasks: contextmenu; Flags: uninsdeletekey
; --- drives ---
Root: HKCU; Subkey: "Software\Classes\Drive\shell\{#MyAppName}"; ValueType: string; ValueData: "通过 {#MyAppName} 打开"; Tasks: contextmenu; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Drive\shell\{#MyAppName}"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\MarinExplorer.exe,0"; Tasks: contextmenu; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Drive\shell\{#MyAppName}\command"; ValueType: string; ValueData: """{app}\MarinExplorer.exe"" ""%1"""; Tasks: contextmenu; Flags: uninsdeletekey
; --- "Open with → choose another app" listing ---
Root: HKCU; Subkey: "Software\Classes\Applications\MarinExplorer.exe"; ValueType: string; ValueName: "FriendlyAppName"; ValueData: "{#MyAppName}"; Tasks: contextmenu; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Applications\MarinExplorer.exe\shell\open\command"; ValueType: string; ValueData: """{app}\MarinExplorer.exe"" ""%1"""; Tasks: contextmenu; Flags: uninsdeletekey

; Leave no trace of the per-install cache (folder sizes only — the app
; recomputes it, so nothing of value is lost).
[UninstallDelete]
Type: filesandordirs; Name: "{app}\{#MyAppName}"
