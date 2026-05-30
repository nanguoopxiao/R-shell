param(
    [string]$Destination = (Join-Path (Resolve-Path (Join-Path $PSScriptRoot '..')) 'dist\windows-gtk'),
    [switch]$IncludeSymbols,
    [switch]$SmokeTest
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
$MingwShare = Join-Path $MsysRoot 'mingw64\share'
$MingwLib = Join-Path $MsysRoot 'mingw64\lib'
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

cargo build -p shell-app --release --features gtk-ui
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

$BinDir = Join-Path $Destination 'bin'
$ShareDir = Join-Path $Destination 'share'
$LibDir = Join-Path $Destination 'lib'
$ToolsRoot = Join-Path $Destination 'tools\msys64'

function Stop-PackagedRuntimeProcesses {
    param(
        [string]$PackageBinDir
    )

    if (-not (Test-Path $PackageBinDir)) {
        return
    }

    $resolvedBinDir = (Resolve-Path $PackageBinDir).Path.TrimEnd('\')
    $processNames = @('gdbus', 'gspawn-win64-helper', 'gspawn-win64-helper-console')
    Get-Process -Name $processNames -ErrorAction SilentlyContinue |
        Where-Object {
            $_.Path -and $_.Path.StartsWith($resolvedBinDir, [StringComparison]::OrdinalIgnoreCase)
        } |
        Stop-Process -Force
}

if (Test-Path $Destination) {
    Stop-PackagedRuntimeProcesses $BinDir
    Remove-Item $Destination -Recurse -Force
}

New-Item -ItemType Directory -Path $BinDir | Out-Null
New-Item -ItemType Directory -Path $ShareDir | Out-Null
New-Item -ItemType Directory -Path $LibDir | Out-Null
New-Item -ItemType Directory -Path $ToolsRoot | Out-Null

Copy-Item (Join-Path $Root 'target\release\shell-app.exe') $BinDir

$PdbPath = Join-Path $Root 'target\release\shell_app.pdb'
if ($IncludeSymbols -and (Test-Path $PdbPath)) {
    Copy-Item $PdbPath $BinDir
}

Get-ChildItem -Path $MingwBin -Filter '*.dll' | Copy-Item -Destination $BinDir

foreach ($helper in @('gspawn-win64-helper.exe', 'gspawn-win64-helper-console.exe', 'gdbus.exe')) {
    $helperPath = Join-Path $MingwBin $helper
    if (Test-Path $helperPath) {
        Copy-Item $helperPath $BinDir
    }
}

$shareDirs = @('glib-2.0', 'gtk-4.0', 'icons', 'fontconfig')
foreach ($name in $shareDirs) {
    $source = Join-Path $MingwShare $name
    if (Test-Path $source) {
        Copy-Item $source (Join-Path $ShareDir $name) -Recurse
    }
}

$libDirs = @('gdk-pixbuf-2.0', 'gio')
foreach ($name in $libDirs) {
    $source = Join-Path $MingwLib $name
    if (Test-Path $source) {
        Copy-Item $source (Join-Path $LibDir $name) -Recurse
    }
}

function Copy-ToolchainPath {
    param(
        [string]$RelativePath
    )

    $source = Join-Path $MsysRoot $RelativePath
    if (-not (Test-Path $source)) {
        return
    }

    $target = Join-Path $ToolsRoot $RelativePath
    $parent = Split-Path $target -Parent
    if (-not (Test-Path $parent)) {
        New-Item -ItemType Directory -Path $parent | Out-Null
    }
    Copy-Item $source $target -Recurse
}

$toolchainConfigPaths = @(
    'etc\msystem',
    'etc\msystem.d',
    'etc\pki',
    'etc\profile',
    'etc\profile.d',
    'etc\protocols',
    'etc\services',
    'etc\ssh',
    'etc\wgetrc',
    'mingw64\etc',
    'mingw64\ssl',
    'mingw64\share\licenses',
    'usr\etc',
    'usr\ssl',
    'usr\share\licenses',
    'usr\share\terminfo'
)

foreach ($relativePath in $toolchainConfigPaths) {
    Copy-ToolchainPath $relativePath
}

$MsysRootFull = (Resolve-Path $MsysRoot).Path.TrimEnd('\')
$toolchainBinaryDirs = @($UsrBin, $MingwBin)
$ObjdumpPath = Join-Path $MingwBin 'objdump.exe'
if (-not (Test-Path $ObjdumpPath)) {
    $ObjdumpCommand = Get-Command objdump -ErrorAction SilentlyContinue
    if ($null -eq $ObjdumpCommand) {
        throw 'objdump.exe was not found. It is required to build a slim command toolchain package.'
    }
    $ObjdumpPath = $ObjdumpCommand.Source
}

function Get-ToolchainRelativePath {
    param(
        [string]$Path
    )

    $fullPath = (Resolve-Path $Path).Path
    if (-not $fullPath.StartsWith($MsysRootFull, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Toolchain file is outside MSYS2 root: $fullPath"
    }
    $relativePath = $fullPath.Substring($MsysRootFull.Length)
    if ($relativePath.StartsWith('\')) {
        $relativePath = $relativePath.Substring(1)
    }
    $relativePath
}

function Copy-ToolchainFile {
    param(
        [string]$SourcePath
    )

    if (-not (Test-Path $SourcePath -PathType Leaf)) {
        return $false
    }

    $relativePath = Get-ToolchainRelativePath $SourcePath
    $target = Join-Path $ToolsRoot $relativePath
    $parent = Split-Path $target -Parent
    if (-not (Test-Path $parent)) {
        New-Item -ItemType Directory -Path $parent | Out-Null
    }
    Copy-Item $SourcePath $target -Force
    $true
}

function Resolve-ToolchainBinary {
    param(
        [string]$CommandName
    )

    $fileName = if ([System.IO.Path]::GetExtension($CommandName)) {
        $CommandName
    } else {
        "$CommandName.exe"
    }

    foreach ($dir in $toolchainBinaryDirs) {
        $candidate = Join-Path $dir $fileName
        if (Test-Path $candidate -PathType Leaf) {
            return $candidate
        }
    }
    $null
}

function Get-PeDllNames {
    param(
        [string]$FilePath
    )

    & $ObjdumpPath -p $FilePath 2>$null |
        ForEach-Object {
            if ($_ -match 'DLL Name:\s*(.+)$') {
                $matches[1].Trim()
            }
        }
}

$copiedToolFiles = @{}
$queuedToolFiles = New-Object 'System.Collections.Generic.Queue[string]'

function Add-ToolchainFileWithDependencies {
    param(
        [string]$SourcePath
    )

    if (-not (Test-Path $SourcePath -PathType Leaf)) {
        return $false
    }

    $fullPath = (Resolve-Path $SourcePath).Path
    $key = $fullPath.ToLowerInvariant()
    if ($copiedToolFiles.ContainsKey($key)) {
        return $true
    }

    if (Copy-ToolchainFile $fullPath) {
        $copiedToolFiles[$key] = $true
        $queuedToolFiles.Enqueue($fullPath)
        return $true
    }
    $false
}

$requiredToolCommands = @('bash', 'sh', 'curl', 'wget', 'ssh', 'telnet')
$optionalToolCommands = @(
    'scp', 'sftp', 'ssh-add', 'ssh-agent', 'ssh-keygen', 'ssh-keyscan',
    'cat', 'cp', 'mv', 'rm', 'mkdir', 'rmdir', 'ls', 'pwd', 'env', 'echo',
    'test', 'true', 'false', 'uname', 'date', 'touch', 'head', 'tail',
    'sort', 'uniq', 'wc', 'find', 'xargs', 'grep', 'sed', 'awk', 'gawk',
    'less', 'tar', 'bsdtar', 'gzip', 'gunzip', 'xz', 'unxz', 'zip', 'unzip'
)
$missingToolCommands = @()

foreach ($commandName in $requiredToolCommands) {
    $commandPath = Resolve-ToolchainBinary $commandName
    if ($null -eq $commandPath) {
        $missingToolCommands += $commandName
        continue
    }
    Add-ToolchainFileWithDependencies $commandPath | Out-Null
}

if ($missingToolCommands.Count -gt 0) {
    throw "Packaged toolchain is missing required commands: $($missingToolCommands -join ', ')"
}

foreach ($commandName in $optionalToolCommands) {
    $commandPath = Resolve-ToolchainBinary $commandName
    if ($null -ne $commandPath) {
        Add-ToolchainFileWithDependencies $commandPath | Out-Null
    }
}

while ($queuedToolFiles.Count -gt 0) {
    $currentFile = $queuedToolFiles.Dequeue()
    foreach ($dllName in Get-PeDllNames $currentFile) {
        foreach ($dir in $toolchainBinaryDirs) {
            $candidate = Join-Path $dir $dllName
            if (Test-Path $candidate -PathType Leaf) {
                Add-ToolchainFileWithDependencies $candidate | Out-Null
                break
            }
        }
    }
}

foreach ($runtimeDir in @('home', 'tmp', 'var\tmp')) {
    $path = Join-Path $ToolsRoot $runtimeDir
    if (-not (Test-Path $path)) {
        New-Item -ItemType Directory -Path $path | Out-Null
    }
}

function Test-ToolchainCommand {
    param(
        [string]$CommandName
    )

    $candidates = @(
        (Join-Path $ToolsRoot "usr\bin\$CommandName.exe"),
        (Join-Path $ToolsRoot "mingw64\bin\$CommandName.exe")
    )
    foreach ($candidate in $candidates) {
        if (Test-Path $candidate) {
            return $true
        }
    }
    return $false
}

$missingPackagedToolCommands = @($requiredToolCommands | Where-Object { -not (Test-ToolchainCommand $_) })
if ($missingPackagedToolCommands.Count -gt 0) {
    throw "Packaged toolchain is missing required commands: $($missingPackagedToolCommands -join ', ')"
}

$ExePath = Join-Path $BinDir 'shell-app.exe'
Write-Host "Packaged GTK app to $ExePath"
Write-Host "Bundled command toolchain to $ToolsRoot"

if ($SmokeTest) {
    Write-Host 'Running startup smoke test...'
    $process = Start-Process -FilePath $ExePath -WorkingDirectory $BinDir -PassThru
    try {
        $ready = $false
        try {
            $ready = $process.WaitForInputIdle(5000)
        } catch {
            $ready = $false
        }

        $process.Refresh()
        if ($process.HasExited) {
            throw "Smoke test failed: packaged app exited early with code $($process.ExitCode)."
        }
        if (-not $ready -and $process.MainWindowHandle -eq 0) {
            throw 'Smoke test failed: packaged app did not reach an interactive GUI state.'
        }

        Write-Host "Smoke test passed: $ExePath started successfully"
    } finally {
        if (Get-Process -Id $process.Id -ErrorAction SilentlyContinue) {
            Stop-Process -Id $process.Id -Force
        }
        Stop-PackagedRuntimeProcesses $BinDir
    }
}