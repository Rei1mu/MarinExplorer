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
  #define MyAppVersion "0.1.16"
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

; Per-user install location, but the installer itself runs elevated.
; Why not Program Files: MarinExplorer keeps its own store (folder-size
; cache) next to the executable so the install stays self-contained, and
; the app runs unelevated whenever its UAC prompt is declined — Program
; Files would not be writable then, silently pushing the cache into
; %LOCALAPPDATA%. {localappdata} is always writable by this user.
; Why the installer is elevated anyway: since v0.1.5 the app relaunches
; itself with "runas" (the Recycle Bin can hold other accounts' items,
; which only an elevated process can delete). An unelevated installer
; cannot close an elevated process — UIPI blocks the close request — so
; upgrading while the app runs used to fail with "DeleteFile failed;
; error code 5" (file in use). Elevated, Setup's CloseApplications works
; and the old instance is shut down silently before the exe is replaced.
; Cost: one UAC prompt per install/upgrade — the same prompt the app
; itself shows on every launch.
DefaultDirName={localappdata}\Programs\{#MyAppName}
PrivilegesRequired=admin
; Inno 7 closes apps that lock files being replaced (the default, made
; explicit here): this is what shuts the running elevated instance down.
CloseApplications=yes

; Start-menu shortcuts: the location is written explicitly in [Icons] as
; {userprograms}\... — never via {group}. In administrative install mode
; {group} would resolve to {commonprograms} (all users), which is wrong for
; a per-user install: other users would get a shortcut into this user's
; profile. DefaultGroupName below is only a *name* (never a path): in admin
; mode Inno appends it to {commonprograms}, and a path here embeds a ":"
; in a directory name, which fails CreateDirectory with ERROR_INVALID_NAME
; ("文件名称不能包含下列任何字符"). The group page is disabled, so this
; value is effectively unused.
DefaultGroupName={#MyAppName}
LicenseFile=..\LICENSE
WizardStyle=modern
; Let the user pick another drive/directory.
DisableDirPage=no
DisableProgramGroupPage=yes
OutputDir=Output
OutputBaseFilename={#MyAppName}-Setup-Windows-{#MyAppArch}-{#MyAppVersion}
; Add/Remove Programs entry. In administrative mode (PrivilegesRequired=
; admin) Inno writes it under HKLM, so every user of this machine sees it;
; uninstalling then also needs the same one UAC prompt.
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
; {userprograms} on purpose — see the DefaultGroupName comment: the entry
; must live in the *installing user's* Start Menu, not {commonprograms}.
Name: "{userprograms}\{#MyAppName}"; Filename: "{app}\MarinExplorer.exe"; Tasks: startmenu
; NOTE: the shortcut name must not contain "/" — it is an illegal filename
; character on Windows, and IPersistFile::Save then fails with 0x80070003
; (path not found) because "A / B.lnk" parses as "B.lnk" inside dir "A ".
Name: "{userprograms}\{#MyAppName}\卸载 {#MyAppName}"; Filename: "{uninstallexe}"; Tasks: startmenu
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
