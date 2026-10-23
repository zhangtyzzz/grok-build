# cc-rs 1.2.48 bypasses RUSTC_WRAPPER for registry-discovered MSVC tools.
# Select the same compiler explicitly, with its SDK environment, so it can cache C/C++.
param([string]$ExportPath)
$ErrorActionPreference = 'Stop'

if ($ExportPath) {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
    $installation = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if ($LASTEXITCODE -ne 0 -or -not $installation) { throw 'Visual Studio C++ installation not found' }
    & (Join-Path $installation 'Common7/Tools/Launch-VsDevShell.ps1') -Arch amd64 -HostArch amd64 -SkipAutomaticLocation | Out-Null
    @{
        compiler = (Get-Command cl.exe -ErrorAction Stop).Source
        include = $env:INCLUDE
        lib = $env:LIB
        libpath = $env:LIBPATH
    } | ConvertTo-Json | Set-Content $ExportPath
    exit 0
}

if ($env:RUNNER_OS -ne 'Windows' -or $env:RUNNER_ARCH -ne 'X64') { return }
# Preserve explicitly configured compilers/SDKs. This optimization is for the
# default GitHub Windows runner environment, not cross-compilation or user overrides.
$existing = @('CC', 'CXX', 'HOST_CC', 'HOST_CXX', 'TARGET_CC', 'TARGET_CXX',
    'CC_x86_64_pc_windows_msvc', 'CXX_x86_64_pc_windows_msvc',
    'CC_x86_64-pc-windows-msvc', 'CXX_x86_64-pc-windows-msvc',
    'INCLUDE', 'LIB', 'LIBPATH', 'VSCMD_VER')
foreach ($name in $existing) {
    if ([Environment]::GetEnvironmentVariable($name)) {
        Write-Host "Preserving preconfigured native toolchain ($name)"
        return
    }
}
if (-not $env:GITHUB_ENV) { throw 'GITHUB_ENV is required' }
$exportFile = Join-Path $env:RUNNER_TEMP 'grok-msvc-native-environment.json'
# Do not import the whole developer shell: VSCMD_VER/VSINSTALLDIR change rustc
# linker discovery and let Git Bash's Unix link.exe shadow the MSVC linker.
& pwsh -NoProfile -File $PSCommandPath -ExportPath $exportFile
if ($LASTEXITCODE -ne 0) { throw 'MSVC environment export failed' }
$native = Get-Content -Raw $exportFile | ConvertFrom-Json
if (-not (Test-Path -LiteralPath $native.compiler -PathType Leaf) -or -not $native.include -or -not $native.lib) {
    throw 'Incomplete MSVC compiler or SDK environment'
}
$values = [ordered]@{
    CC_x86_64_pc_windows_msvc = $native.compiler
    CXX_x86_64_pc_windows_msvc = $native.compiler
    INCLUDE = $native.include
    LIB = $native.lib
    LIBPATH = $native.libpath
    MSYS2_ENV_CONV_EXCL = (($env:MSYS2_ENV_CONV_EXCL, 'CC_x86_64_pc_windows_msvc;CXX_x86_64_pc_windows_msvc;INCLUDE;LIB;LIBPATH') | Where-Object { $_ }) -join ';'
}
$lines = foreach ($entry in $values.GetEnumerator()) {
    if ($entry.Value -match '[\r\n]') { throw "Unexpected newline in $($entry.Key)" }
    "$($entry.Key)=$($entry.Value)"
}
# Write only after validating every value; a failed optional setup leaves no partial environment.
$lines | Add-Content $env:GITHUB_ENV
Write-Host "Enabled native compiler cache for $($native.compiler)"
