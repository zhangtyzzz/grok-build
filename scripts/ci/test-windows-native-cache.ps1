$ErrorActionPreference = 'Stop'
$probeRoot = Join-Path $env:RUNNER_TEMP ('grok-native-cache-test-' + [System.IO.Path]::GetRandomFileName())
$evidence = Join-Path $probeRoot 'evidence'
$fixture = Join-Path $probeRoot 'fixture'
$wrapperDir = Join-Path $probeRoot 'wrapper'
New-Item -ItemType Directory -Force $evidence, $fixture, $wrapperDir | Out-Null
$realCache = $env:SCCACHE_PATH
if ($env:RUST_CHANNEL) { $env:RUSTUP_TOOLCHAIN = $env:RUST_CHANNEL }
$wrapper = Join-Path $wrapperDir 'sccache.exe'
rustc scripts/ci/sccache-fallback.rs -o $wrapper
if ($LASTEXITCODE -ne 0) { throw 'wrapper compilation failed' }
# Use only runner-local cache; no remote cache or production snapshot writes.
& $realCache --stop-server 2>&1 | Out-Null
try {
    $env:GROK_SCCACHE_BIN = $realCache
    $env:RUSTC_WRAPPER = $wrapper
    $env:SCCACHE_DIR = Join-Path $probeRoot 'cache'
    $env:SCCACHE_CACHE_SIZE = '100M'
    $env:SCCACHE_LOCAL_RW_MODE = 'READ_WRITE'
    $env:SCCACHE_IDLE_TIMEOUT = '0'
    $env:LINK = (($env:LINK, '/Brepro') | Where-Object { $_ }) -join ' '
    $env:MSYS2_ENV_CONV_EXCL = (($env:MSYS2_ENV_CONV_EXCL, 'LINK') | Where-Object { $_ }) -join ';'
    $compilerVars = @('CC', 'CXX', 'CC_x86_64_pc_windows_msvc', 'CXX_x86_64_pc_windows_msvc', 'CC_x86_64-pc-windows-msvc', 'CXX_x86_64-pc-windows-msvc')
    foreach ($name in $compilerVars) {
        if ([Environment]::GetEnvironmentVariable($name)) { throw "Unexpected preconfigured compiler: $name" }
    }
    @'
    [package]
    name = "native-cache-fixture"
    version = "0.1.0"
    edition = "2021"
    [workspace]
    [build-dependencies]
    cc = "=1.2.48"
    find-msvc-tools = "=0.1.14"
    [profile.release]
    opt-level = 3
    debug = 1
    codegen-units = 16
'@ | Set-Content (Join-Path $fixture 'Cargo.toml')
    @'
    fn main() {
        let mut c = cc::Build::new();
        c.file("sample.c");
        let tool = c.get_compiler();
        println!("cargo:warning=C compiler: {}", tool.path().display());
        println!("cargo:warning=C launcher: {}", tool.to_command().get_program().to_string_lossy());
        c.compile("native_c");
        let mut cpp = cc::Build::new();
        cpp.cpp(true).flag("/EHsc").file("sample.cpp");
        let tool = cpp.get_compiler();
        println!("cargo:warning=C++ compiler: {}", tool.path().display());
        println!("cargo:warning=C++ launcher: {}", tool.to_command().get_program().to_string_lossy());
        cpp.compile("native_cpp");
    }
'@ | Set-Content (Join-Path $fixture 'build.rs')
    @'
    #include <stdio.h>
    int native_c(void) { char value[16]; return sprintf(value, "%d", 42) + 40; }
'@ | Set-Content (Join-Path $fixture 'sample.c')
    @'
    #include <vector>
    #include <numeric>
    extern "C" int native_cpp(void) { std::vector<int> items{20, 30, 8}; return std::accumulate(items.begin(), items.end(), 0); }
'@ | Set-Content (Join-Path $fixture 'sample.cpp')
    New-Item -ItemType Directory -Force (Join-Path $fixture 'src') | Out-Null
    @'
    extern "C" { fn native_c() -> i32; fn native_cpp() -> i32; }
    #[test]
    fn native_output_is_correct() { assert_eq!(unsafe { native_c() + native_cpp() }, 100); }
'@ | Set-Content (Join-Path $fixture 'src/lib.rs')
    $gitBash = 'C:\Program Files\Git\bin\bash.exe'
    $env:PROBE_FIXTURE_DIR = $fixture
    function Invoke-Fixture([string]$label) {
        $env:PROBE_TARGET_DIR = Join-Path $probeRoot $label
        $timer = [System.Diagnostics.Stopwatch]::StartNew()
        & $gitBash --noprofile --norc -c 'set -euo pipefail; cd "$PROBE_FIXTURE_DIR"; cargo test --locked --release --target x86_64-pc-windows-msvc --target-dir "$PROBE_TARGET_DIR" -- --nocapture' 2>&1 | Tee-Object -FilePath (Join-Path $evidence "$label.log") | Out-Host
        $result = $LASTEXITCODE
        $timer.Stop()
        if ($result -ne 0) { throw "Fixture failed: $label ($result)" }
        return $timer.Elapsed.TotalSeconds
    }
    function Get-CacheStats([string]$label) {
        $stats = (& $realCache --show-stats) -join "`n"
        $stats | Set-Content (Join-Path $evidence "$label.txt")
        Write-Host $stats
        return $stats
    }
    function Get-Counter([string]$stats, [string]$pattern) {
        $match = [regex]::Match($stats, "(?m)^$pattern\s+(\d+)\s*$")
        if (-not $match.Success) { throw "Missing cache counter: $pattern" }
        return [int]$match.Groups[1].Value
    }
    Push-Location $fixture
    try { cargo generate-lockfile; if ($LASTEXITCODE -ne 0) { throw 'lock generation failed' } }
    finally { Pop-Location }
    & $realCache --zero-stats | Out-Null
    $baselineCold = Invoke-Fixture 'baseline-cold'
    $baselineHot = Invoke-Fixture 'baseline-hot'
    $baseline = Get-CacheStats 'baseline-stats'
    # Exercise the exact production helper, importing its future-step environment
    # into this process without modifying the workflow's real environment file.
    $helperEnv = Join-Path $probeRoot 'helper.env'
    $savedGithubEnv = $env:GITHUB_ENV
    try {
        $env:GITHUB_ENV = $helperEnv
        $env:CC = 'keep-custom-compiler'
        & (Join-Path $PSScriptRoot 'enable-windows-native-cache.ps1')
        if ($env:CC -ne 'keep-custom-compiler' -or (Test-Path $helperEnv)) {
            throw 'Helper changed a preconfigured compiler environment'
        }
        $env:CC = $null
        & (Join-Path $PSScriptRoot 'enable-windows-native-cache.ps1')
    } finally {
        $env:CC = $null
        $env:GITHUB_ENV = $savedGithubEnv
    }
    foreach ($line in Get-Content $helperEnv) {
        $parts = $line -split '=', 2
        [Environment]::SetEnvironmentVariable($parts[0], $parts[1], 'Process')
    }
    $compiler = $env:CC_x86_64_pc_windows_msvc
    if (-not $compiler) { throw 'Production helper did not configure compiler cache' }
    $compiler | Set-Content (Join-Path $evidence 'explicit-compiler.txt')
    & $realCache --zero-stats | Out-Null
    $explicitCold = Invoke-Fixture 'explicit-cold'
    $explicitHot = Invoke-Fixture 'explicit-hot'
    $explicit = Get-CacheStats 'explicit-stats'
    $baselineHits = 0
    if ($baseline -match 'Cache hits \(C/C\+\+\)') { $baselineHits = Get-Counter $baseline 'Cache hits \(C/C\+\+\)' }
    $explicitHits = Get-Counter $explicit 'Cache hits \(C/C\+\+\)'
    $explicitMisses = Get-Counter $explicit 'Cache misses \(C/C\+\+\)'
    $writeErrors = Get-Counter $explicit 'Cache write errors'
    $result = [ordered]@{
        baseline_c_hits = $baselineHits
        explicit_c_hits = $explicitHits
        explicit_c_misses = $explicitMisses
        cache_write_errors = $writeErrors
        compiler = $compiler
        seconds = @{ baseline_cold = $baselineCold; baseline_hot = $baselineHot; explicit_cold = $explicitCold; explicit_hot = $explicitHot }
    }
    $result | ConvertTo-Json -Depth 5 | Tee-Object -FilePath (Join-Path $evidence 'result.json')
    if ($baselineHits -ne 0) { throw 'Baseline unexpectedly used native cache; investigate before changing production' }
    if ($explicitHits -lt 2 -or $explicitMisses -lt 2 -or $writeErrors -ne 0) { throw 'Explicit native compiler did not provide clean cache reuse' }
    $baselineCompiler = Select-String -Path (Join-Path $evidence 'baseline-cold.log') -Pattern 'C compiler: (.+)$'
    $baselineCompilerPath = $baselineCompiler.Matches[0].Groups[1].Value.Trim()
    if ($baselineCompilerPath -ne $compiler) { throw "Compiler changed: $baselineCompilerPath -> $compiler" }

} finally {
    & $realCache --stop-server 2>&1 | Out-Null
    Remove-Item -LiteralPath $probeRoot -Recurse -Force
}
