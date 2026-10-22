# A changed proc-macro DLL hash invalidates dependent Rust compiler-cache keys.
$ErrorActionPreference = 'Stop'
$probeDir = Join-Path $env:RUNNER_TEMP ([System.IO.Path]::GetRandomFileName())
New-Item -ItemType Directory $probeDir | Out-Null
Push-Location $probeDir
try {
    'extern crate proc_macro; #[proc_macro] pub fn identity(input: proc_macro::TokenStream) -> proc_macro::TokenStream { input }' | Set-Content probe.rs
    $compilerArgs = @('--crate-type', 'proc-macro', '--crate-name', 'probe', '--edition', '2024', 'probe.rs', '-C', 'opt-level=3', '-C', 'debuginfo=1', '-o', 'probe.dll')
    $hashes = @()
    foreach ($attempt in @(1, 2)) {
        Remove-Item probe.dll, probe.pdb, probe.dll.lib, probe.dll.exp -ErrorAction SilentlyContinue
        rustup run $env:RUST_CHANNEL rustc @compilerArgs
        if ($LASTEXITCODE -ne 0) { throw 'Failed to build the proc-macro fixture' }
        $hashes += (Get-FileHash probe.dll -Algorithm SHA256).Hash
        if ($attempt -eq 1) { Start-Sleep -Seconds 2 }
    }
    if ($hashes[0] -ne $hashes[1]) {
        throw 'Identical proc-macro builds produced different DLLs; compiler-cache reuse would fail'
    }
    'fn main() { assert_eq!(probe::identity!(42), 42); }' | Set-Content consumer.rs
    rustup run $env:RUST_CHANNEL rustc --edition 2024 --extern probe=probe.dll consumer.rs -o consumer.exe
    if ($LASTEXITCODE -ne 0) { throw 'Failed to load the reproducible proc macro' }
    & ./consumer.exe
    if ($LASTEXITCODE -ne 0) { throw 'The proc-macro consumer failed' }
    Write-Output 'Repeated proc-macro DLLs are identical and the macro executes successfully'
} finally {
    Pop-Location
    Remove-Item -Recurse -Force $probeDir
}
