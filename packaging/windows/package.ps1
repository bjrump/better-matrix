# Packages a Windows release build (target\release\better-matrix.exe) with its icon and the
# license as dist\out\better-matrix-windows-x86_64.zip, which the README's install script unpacks.
$ErrorActionPreference = "Stop"
New-Item -ItemType Directory -Force -Path dist\zip, dist\out | Out-Null
Copy-Item target\release\better-matrix.exe, packaging\icons\better-matrix.ico, LICENSE -Destination dist\zip
Compress-Archive -Path dist\zip\* -DestinationPath dist\out\better-matrix-windows-x86_64.zip -Force
