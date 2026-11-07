; Per-user installer for Muninn. Built by packaging\build.ps1.
#define Version GetEnv("MUNINN_VERSION")

[Setup]
AppId={{6F1D3A52-9C4B-4E7A-B0E1-2D7C5A9F4E11}
AppName=Muninn
AppVersion={#Version}
AppPublisher=Ethan Dsouza (Kryzern)
AppPublisherURL=https://github.com/Kryzern/muninn
DefaultDirName={localappdata}\Programs\Muninn
DefaultGroupName=Muninn
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=..\dist
OutputBaseFilename=Muninn-{#Version}-x64-setup
SetupIconFile=muninn.ico
UninstallDisplayIcon={app}\muninn.exe
Compression=lzma2/max
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

[Files]
Source: "..\dist\Muninn\*"; DestDir: "{app}"; Flags: recursesubdirs ignoreversion

[Icons]
Name: "{autoprograms}\Muninn"; Filename: "{app}\muninn.exe"
Name: "{autodesktop}\Muninn"; Filename: "{app}\muninn.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\muninn.exe"; Description: "Open Muninn"; Flags: nowait postinstall skipifsilent unchecked
; An update runs this setup silently from inside Muninn: open it again afterwards.
Filename: "{app}\muninn.exe"; Flags: nowait; Check: WizardSilent

[UninstallDelete]
Type: filesandordirs; Name: "{localappdata}\Muninn"
