# Builds dist\Muninn (portable), the zip, and the installer.
# No model is bundled: Muninn downloads the one you pick from its Models tab,
# so the installer stays small. tools\package_models.py prepares those files.
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
Set-Location $root
cargo build --release
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$out = Join-Path $root 'dist\Muninn'
Remove-Item -Recurse -Force $out -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $out | Out-Null
Copy-Item target\release\muninn.exe $out
Copy-Item runtime\*.dll $out
Copy-Item README.md, LICENSE $out
Compress-Archive -Force -Path "$out\*" -DestinationPath (Join-Path $root "dist\Muninn-$version-x64.zip")
$env:MUNINN_VERSION = $version
& "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe" /Q packaging\muninn.iss
Write-Host "Done: dist\Muninn-$version-x64-setup.exe"
