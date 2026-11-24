# SSH Remote for Windows, for this user. In PowerShell:
#   irm https://sshremote.dev/install.ps1 | iex
# Takes the current version and its SHA-256 from https://sshremote.dev/download/index.json,
# downloads that version from GitHub Releases, checks it, and puts it in
# %LOCALAPPDATA%\Programs\SSH Remote with a Start menu shortcut and the folder on this user's
# PATH, then opens it. With $env:SSHREMOTE_INSTALL_DIR set it only puts the program there.
# To remove it: `sshremote remote-access uninstall` if the service was installed, then delete the
# folder and the Start menu shortcut "SSH Remote", and take the folder off PATH.

& {
  $ErrorActionPreference = 'Stop'
  $ProgressPreference = 'SilentlyContinue'
  # Windows PowerShell 5.1 on an older Windows 10 offers only older TLS unless told.
  [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
  if (-not [Environment]::Is64BitOperatingSystem) { throw 'SSH Remote is built for 64-bit Windows only.' }
  $name = 'sshremote-windows-x86_64.exe'
  $index = Invoke-RestMethod 'https://sshremote.dev/download/index.json'
  $file = $index.files | Where-Object { $_.name -eq $name }
  if (-not $file) { throw "sshremote.dev lists no $name." }
  $version = $index.version
  $custom = [bool]$env:SSHREMOTE_INSTALL_DIR
  $dir = if ($custom) { $env:SSHREMOTE_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\SSH Remote' }
  New-Item -ItemType Directory -Force -Path $dir | Out-Null
  $exe = Join-Path $dir 'sshremote.exe'
  $new = Join-Path $dir 'sshremote.new.exe'
  Write-Host "Downloading SSH Remote $version from GitHub Releases..."
  Invoke-WebRequest "https://github.com/sshremote-dev/sshremote/releases/download/v$version/$name" -OutFile $new -UseBasicParsing
  $sum = (Get-FileHash $new -Algorithm SHA256).Hash.ToLower()
  if ($sum -ne $file.sha256) {
    Remove-Item $new
    throw "The download's SHA-256 ($sum) is not the one sshremote.dev lists; nothing was changed."
  }
  # A copy that runs is moved aside, as the program's own update does; it removes it at its next start.
  if (Test-Path $exe) {
    $old = Join-Path $dir 'sshremote.old.exe'
    Remove-Item $old -ErrorAction SilentlyContinue
    Move-Item $exe $old -Force
  }
  Move-Item $new $exe
  Write-Host "SSH Remote $version is in $exe"
  if ($custom) { return }
  $link = Join-Path ([Environment]::GetFolderPath('Programs')) 'SSH Remote.lnk'
  $shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut($link)
  $shortcut.TargetPath = $exe
  $shortcut.WorkingDirectory = $dir
  $shortcut.Description = 'SSH Remote'
  $shortcut.Save()
  Write-Host 'Start menu: SSH Remote'
  $path = [Environment]::GetEnvironmentVariable('Path', 'User')
  if (($path -split ';') -notcontains $dir) {
    [Environment]::SetEnvironmentVariable('Path', ((@($path -split ';') + $dir | Where-Object { $_ }) -join ';'), 'User')
    Write-Host "On your PATH from the next terminal: $dir"
  }
  Start-Process $exe
}
