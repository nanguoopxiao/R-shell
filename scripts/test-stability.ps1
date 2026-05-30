param(
    [switch]$Sftp,
    [int]$SftpLargeMb = 8,
    [int]$SftpFileCount = 64
)

$ErrorActionPreference = 'Stop'

$Root = Resolve-Path (Join-Path $PSScriptRoot '..')
Set-Location $Root

Write-Host '== Terminal long-text stability tests =='
cargo test -p shell-terminal parses_very_long_wrapped_output_with_bounded_scrollback
cargo test -p shell-terminal parses_long_split_utf8_stream_without_losing_tail

Write-Host '== Renderer GTK regression tests =='
$MsysRoot = Join-Path $Root '.msys64'
$MingwBin = Join-Path $MsysRoot 'mingw64\bin'
$UsrBin = Join-Path $MsysRoot 'usr\bin'
$CargoBin = Join-Path $HOME '.cargo\bin'
if (Test-Path $MingwBin) {
    $env:PATH = "$CargoBin;$MingwBin;$UsrBin;$env:PATH"
    $env:PKG_CONFIG_PATH = "$(Join-Path $MsysRoot 'mingw64\lib\pkgconfig');$(Join-Path $MsysRoot 'mingw64\share\pkgconfig')"
    $env:XDG_DATA_DIRS = "$(Join-Path $MsysRoot 'mingw64\share');$(Join-Path $MsysRoot 'usr\share')"
    cargo test -p shell-renderer --features gtk
} else {
    Write-Warning "MSYS2 GTK dependencies were not found at $MingwBin; skipping renderer GTK tests."
}

if ($Sftp -or $env:SHELL_SFTP_TEST_HOST) {
    if (-not $env:SHELL_SFTP_TEST_LARGE_MB) {
        $env:SHELL_SFTP_TEST_LARGE_MB = [string]$SftpLargeMb
    }
    if (-not $env:SHELL_SFTP_TEST_FILE_COUNT) {
        $env:SHELL_SFTP_TEST_FILE_COUNT = [string]$SftpFileCount
    }

    Write-Host '== Live SFTP stability test =='
    Write-Host 'Required env: SHELL_SFTP_TEST_HOST, SHELL_SFTP_TEST_USERNAME, SHELL_SFTP_TEST_PASSWORD'
    Write-Host 'Optional env: SHELL_SFTP_TEST_PORT, SHELL_SFTP_TEST_REMOTE_ROOT, SHELL_SFTP_TEST_LARGE_MB, SHELL_SFTP_TEST_FILE_COUNT'
    cargo test -p shell-protocol sftp_stability_large_file_resume_and_directory_roundtrip -- --ignored --nocapture
} else {
    Write-Host '== Live SFTP stability test skipped =='
    Write-Host 'Set SHELL_SFTP_TEST_HOST, SHELL_SFTP_TEST_USERNAME and SHELL_SFTP_TEST_PASSWORD, then rerun with -Sftp.'
}
