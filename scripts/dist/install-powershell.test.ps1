# Exercise the shipped payload function AND its actual call site, with network
# fixtures instead of copying URL construction into a second implementation.
param(
    [string]$InstallerPath = (Join-Path $PSScriptRoot '../../crates/codegen/xai-grok-pager/scripts/install.ps1')
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$tokens = $null
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    (Resolve-Path $InstallerPath).Path, [ref]$tokens, [ref]$errors)
if ($errors.Count -ne 0) { throw "Installer parse failed: $errors" }
$functionNodes = @($ast.FindAll({
    param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -eq 'Install-WindowsPayload'
}, $true))
$callNodes = @($ast.FindAll({
    param($node)
    $node -is [System.Management.Automation.Language.CommandAst] -and
        $node.GetCommandName() -eq 'Install-WindowsPayload'
}, $true))
if ($functionNodes.Count -ne 1 -or $callNodes.Count -ne 1) {
    throw 'Expected exactly one shipped payload function and one call site'
}
. ([scriptblock]::Create($functionNodes[0].Extent.Text))

$script:Requests = [System.Collections.Generic.List[string]]::new()
function Download-File([string]$Url, [string]$OutFile) {
    $script:Requests.Add($Url)
    throw 'fixture: optional payload asset is not published'
}
function Download-String([string]$Url) {
    $script:Requests.Add($Url)
    return $null
}

$temp = Join-Path ([IO.Path]::GetTempPath()) ('grok-powershell-test-' + [guid]::NewGuid())
$oldLocalAppData = $env:LOCALAPPDATA
try {
    New-Item -ItemType Directory -Path $temp -Force | Out-Null
    $env:LOCALAPPDATA = $temp
    $ReleaseBase = 'https://github.com/zhangtyzzz/grok-build/releases/download/v1.2.3'
    $LatestReleaseBase = 'https://github.com/zhangtyzzz/grok-build/releases/latest/download'
    $resolvedVersion = '1.2.3'
    $BinDir = Join-Path $temp 'bin'
    $DownloadDir = Join-Path $temp 'downloads'
    foreach ($platform in @('windows-x86_64', 'windows-aarch64')) {
        $script:Requests.Clear()
        # Execute the production call, not a test-owned recreation. Before the
        # fix this references undefined $BaseUrl and fails under strict mode.
        . ([scriptblock]::Create($callNodes[0].Extent.Text))
        $expected = @(
            "$ReleaseBase/grove-$resolvedVersion-$platform.exe",
            "$ReleaseBase/grok-$resolvedVersion-$platform-mingit.version"
        )
        if ($script:Requests.Count -ne $expected.Count) {
            throw "Unexpected request count: $($script:Requests.Count)"
        }
        for ($index = 0; $index -lt $expected.Count; $index++) {
            if ($script:Requests[$index] -cne $expected[$index]) {
                throw "Payload URL mismatch: $($script:Requests[$index])"
            }
        }
    }
    Write-Host 'PowerShell installer tagged-payload request tests passed'
} finally {
    $env:LOCALAPPDATA = $oldLocalAppData
    if (Test-Path $temp) { Remove-Item $temp -Recurse -Force }
}
