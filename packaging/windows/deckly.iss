; Inno Setup script for deckly-setup-windows-x64.exe. Built by
; `cargo dev dist --target x86_64-pc-windows-msvc --setup`, which passes the
; defines below; do not run ISCC on it by hand.
;
; Installs for the current user (no administrator rights) into
; %LOCALAPPDATA%\Programs\Deckly, or for all users when chosen in the
; dialog. Silent installs (/VERYSILENT) start Deckly afterwards only with
; /LAUNCH=1, which Deckly's own updater passes.

#ifndef AppVersion
  #error AppVersion is not defined
#endif
#ifndef SourceDir
  #error SourceDir is not defined
#endif
#ifndef OutputDir
  #error OutputDir is not defined
#endif

[Setup]
; Never change AppId: Windows recognises updates of Deckly by it.
AppId={{6FBB25A8-2160-4C30-A7B9-DDE2B6057C12}
AppName=Deckly
AppVersion={#AppVersion}
AppVerName=Deckly {#AppVersion}
AppPublisher=auuruum
AppPublisherURL=https://github.com/auuruum/Deckly
AppSupportURL=https://github.com/auuruum/Deckly/issues
AppUpdatesURL=https://github.com/auuruum/Deckly/releases
DefaultDirName={autopf}\Deckly
DefaultGroupName=Deckly
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=deckly-setup-windows-x64
SetupIconFile={#SourceDir}\deckly.ico
UninstallDisplayIcon={app}\deckly.ico
UninstallDisplayName=Deckly
LicenseFile={#SourceDir}\LICENSE
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; Closes a running Deckly (and its plugin pages) before replacing files.
CloseApplications=force
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs

[Icons]
Name: "{autoprograms}\Deckly"; Filename: "{app}\deckly.exe"; IconFilename: "{app}\deckly.ico"
Name: "{autodesktop}\Deckly"; Filename: "{app}\deckly.exe"; IconFilename: "{app}\deckly.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\deckly.exe"; Description: "{cm:LaunchProgram,Deckly}"; Flags: nowait postinstall skipifsilent
Filename: "{app}\deckly.exe"; Flags: nowait; Check: LaunchAfterSilentInstall

[Code]
function LaunchAfterSilentInstall: Boolean;
begin
  Result := WizardSilent and (ExpandConstant('{param:LAUNCH|0}') = '1');
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  { Deckly's "Open automatically at login" writes this value; remove it with
    the program so Windows does not try to start a missing file. }
  if CurUninstallStep = usPostUninstall then
    RegDeleteValue(HKEY_CURRENT_USER, 'Software\Microsoft\Windows\CurrentVersion\Run', 'Deckly');
end;
