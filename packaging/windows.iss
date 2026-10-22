; Inno Setup script. iscc /DAppVersion=1.0.0 /DSourceDir=..\apps\flutter\build\windows\x64\runner\Release packaging\windows.iss
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\apps\flutter\build\windows\x64\runner\Release"
#endif

[Setup]
AppId={{6F0B8A3E-5C1D-4E6B-9A57-2C4B7D1E0F11}
AppName=Hokkabaz
AppVersion={#AppVersion}
AppPublisher=Hokkabaz
DefaultDirName={autopf}\Hokkabaz
DefaultGroupName=Hokkabaz
OutputDir=..\dist
OutputBaseFilename=hokkabaz-windows-x64-setup
Compression=lzma2
SolidCompression=yes
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
UninstallDisplayIcon={app}\hokkabaz.exe

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: recursesubdirs ignoreversion

[Icons]
Name: "{group}\Hokkabaz"; Filename: "{app}\hokkabaz.exe"
Name: "{autodesktop}\Hokkabaz"; Filename: "{app}\hokkabaz.exe"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

[Run]
Filename: "{app}\hokkabaz.exe"; Description: "Start Hokkabaz"; Flags: nowait postinstall skipifsilent
