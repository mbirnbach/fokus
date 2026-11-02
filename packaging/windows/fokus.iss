; Windows installer for Fokus, built by scripts/package.sh with Inno Setup:
;   iscc /DVersion=<version> /DSource=<folder with fokus.exe> /DOutput=<dir> /DOutputName=<name> fokus.iss
; Installs for the current user without asking for admin rights (the dialog
; offers all users too), adds a Start menu entry, opens .fokus projects with
; Fokus and comes with an uninstaller.

#ifndef Version
  #define Version "0.0.0"
#endif

[Setup]
; Never change the AppId: Windows uses it to find the installed version for updates and uninstall.
AppId={{8996F97D-20AB-4BCF-BB68-26A248CDF1A3}
AppName=Fokus
AppVersion={#Version}
AppVerName=Fokus {#Version}
AppPublisher=Fokus
AppPublisherURL=https://github.com/mbirnbach/fokus
AppSupportURL=https://github.com/mbirnbach/fokus/issues
DefaultDirName={autopf}\Fokus
DefaultGroupName=Fokus
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
ChangesAssociations=yes
SetupIconFile={#SourcePath}\..\..\assets\brand\icon\fokus.ico
UninstallDisplayIcon={app}\fokus.exe
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
OutputDir={#Output}
OutputBaseFilename={#OutputName}

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "de"; MessagesFile: "compiler:Languages\German.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#Source}\fokus.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Fokus"; Filename: "{app}\fokus.exe"
Name: "{autodesktop}\Fokus"; Filename: "{app}\fokus.exe"; Tasks: desktopicon

[Registry]
; .fokus projects open in Fokus. HKA is the current user or all users, depending on the install mode.
Root: HKA; Subkey: "Software\Classes\.fokus"; ValueType: string; ValueName: ""; ValueData: "Fokus.Project"; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Fokus.Project"; ValueType: string; ValueName: ""; ValueData: "Fokus project"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Fokus.Project\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\fokus.exe,0"
Root: HKA; Subkey: "Software\Classes\Fokus.Project\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\fokus.exe"" ""%1"""

[Run]
Filename: "{app}\fokus.exe"; Description: "{cm:LaunchProgram,Fokus}"; Flags: nowait postinstall skipifsilent
