# Read-only health check for driving lobby-rs (see ../SKILL.md, Doctor). Changes nothing.
$ErrorActionPreference = 'Continue'
$root = (Resolve-Path (Join-Path $PSScriptRoot '../../../..')).Path
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
$debug = Join-Path $target 'debug'

Write-Output "repo    $root"
Write-Output "target  $target"
Write-Output "head    $(git -C $root log -1 --format='%h %s')"

Write-Output "`n== toolchain"
Push-Location $root
cargo xtask check-toolchain 2>&1 | Select-Object -Last 3
Pop-Location

Write-Output "`n== drive binaries (cargo xtask drive rebuilds them unless --no-build)"
$newestSrc = Get-ChildItem -Path (Join-Path $root 'crates') -Recurse -File -Include *.rs, *.toml |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
foreach ($bin in 'lobby.exe', 'lobby-server.exe') {
    $p = Join-Path $debug $bin
    if (Test-Path $p) {
        $t = (Get-Item $p).LastWriteTime
        $state = if ($newestSrc -and $t -lt $newestSrc.LastWriteTime) { 'OLDER than newest source; do not use --no-build' } else { 'up to date' }
        Write-Output ("{0,-17} {1:yyyy-MM-dd HH:mm:ss}  {2}" -f $bin, $t, $state)
    } else {
        Write-Output ("{0,-17} missing (the first drive builds it)" -f $bin)
    }
}

Write-Output "`n== lobby / lobby-server processes from this target (not necessarily yours: never kill by name)"
$procs = Get-CimInstance Win32_Process -Filter "Name='lobby.exe' OR Name='lobby-server.exe' OR Name='xtask.exe'" |
    Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith($target, [StringComparison]::OrdinalIgnoreCase) }
if ($procs) {
    $procs | ForEach-Object {
        Write-Output ("pid {0,-7} started {1:HH:mm:ss}  {2}" -f $_.ProcessId, $_.CreationDate, $_.CommandLine)
    }
} else {
    Write-Output 'none'
}

Write-Output "`n== Credential Manager entries of driven clients (removed by the next drive)"
$creds = cmdkey /list | Select-String 'lobby-rs/tV-drive-'
if ($creds) { $creds | ForEach-Object { $_.Line.Trim() } } else { Write-Output 'none' }
