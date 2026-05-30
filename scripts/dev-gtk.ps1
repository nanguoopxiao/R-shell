param(
    [ValidateSet('run', 'check', 'build', 'clippy', 'dist')]
    [string]$Command = 'run'
)

$ErrorActionPreference = 'Stop'

$Root = Resolve-Path (Join-Path $PSScriptRoot '..')
$MsysRoot = if ($env:MSYS2_ROOT) {
    $env:MSYS2_ROOT
} elseif ($env:MSYS2_LOCATION) {
    $env:MSYS2_LOCATION
} else {
    Join-Path $Root '.msys64'
}
$MingwBin = Join-Path $MsysRoot 'mingw64\bin'
$UsrBin = Join-Path $MsysRoot 'usr\bin'
$CargoBin = Join-Path $HOME '.cargo\bin'

if (-not (Test-Path $MingwBin)) {
    throw "MSYS2 GTK4 dependencies were not found at $MingwBin. See README.md for setup steps."
}

Write-Host "Using MSYS2 root: $MsysRoot"

if (-not (Test-Path (Join-Path $CargoBin 'cargo.exe'))) {
    $CargoCommand = Get-Command cargo -ErrorAction SilentlyContinue
    if ($null -eq $CargoCommand) {
        throw 'cargo.exe was not found. Install Rust with rustup first.'
    }
    $CargoBin = Split-Path $CargoCommand.Source
}

$env:PATH = "$CargoBin;$MingwBin;$UsrBin;$env:PATH"
$env:PKG_CONFIG_PATH = "$(Join-Path $MsysRoot 'mingw64\lib\pkgconfig');$(Join-Path $MsysRoot 'mingw64\share\pkgconfig')"
$env:XDG_DATA_DIRS = "$(Join-Path $MsysRoot 'mingw64\share');$(Join-Path $MsysRoot 'usr\share')"

Set-Location $Root

switch ($Command) {
    'run'    { cargo run -p shell-app --features gtk-ui }
    'check'  { cargo check -p shell-app --features gtk-ui }
    'build'  { cargo build -p shell-app --features gtk-ui }
    'clippy' { cargo clippy -p shell-app --features gtk-ui --all-targets -- -D warnings }
    'dist'   {
        & (Join-Path $PSScriptRoot 'package-gtk.ps1')
    }
}

exit $LASTEXITCODE
