$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0
$sourcePath = Join-Path (Split-Path -Parent $PSScriptRoot) 'packaging/smoke-desktop-startup.ps1'
$tokens = $null
$parseErrors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($sourcePath, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count -ne 0) { throw 'Script parse failed.' }
foreach ($definition in $ast.FindAll({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] }, $false)) {
    . ([scriptblock]::Create($definition.Extent.Text))
}
$cleanupToken = [guid]::NewGuid().ToString('D')
$probeRoot = Join-Path ([IO.Path]::GetTempPath()) ('PromptDock-native-startup-' + $cleanupToken.Replace('-', ''))
$outsideRoot = Join-Path ([IO.Path]::GetTempPath()) ('PromptDock-native-cleanup-target-' + [guid]::NewGuid().ToString('N'))
$marker = @{ schemaVersion = 1; scenario = 'fresh'; cleanupToken = $cleanupToken; buildCommit = '93e2800e129489899bc5f47f07794cfaf7a7ab76' }
$markerPath = Join-Path $probeRoot '.startup-probe.json'
$utf8 = New-Object Text.UTF8Encoding($false)
function Write-TestMarker { [IO.File]::WriteAllText($markerPath, ($marker | ConvertTo-Json), $utf8) }
function Assert-CleanupRejected {
    $rejected = $false
    try { $null = Remove-ValidatedProbeDirectory -Scenario fresh -CleanupToken $cleanupToken -ProbeDirectory $probeRoot } catch { $rejected = $true }
    if (-not $rejected -or -not (Test-Path -LiteralPath $probeRoot)) { throw 'Unsafe cleanup was not refused.' }
}
New-Item -ItemType Directory -Path $probeRoot | Out-Null
Assert-CleanupRejected
Write-TestMarker
$marker.cleanupToken = [guid]::NewGuid().ToString('D')
Write-TestMarker
Assert-CleanupRejected
$marker.cleanupToken = $cleanupToken
Write-TestMarker
New-Item -ItemType Directory -Path $outsideRoot | Out-Null
[IO.File]::WriteAllText((Join-Path $outsideRoot 'keep.txt'), 'outside sentinel', $utf8)
$junction = Join-Path $probeRoot 'outside'
New-Item -ItemType Junction -Path $junction -Target $outsideRoot | Out-Null
Assert-CleanupRejected
if ([IO.File]::ReadAllText((Join-Path $outsideRoot 'keep.txt')) -cne 'outside sentinel') { throw 'Outside data changed.' }
[IO.Directory]::Delete($junction)
$result = Remove-ValidatedProbeDirectory -Scenario fresh -CleanupToken $cleanupToken -ProbeDirectory $probeRoot
if ($result -cne $marker.buildCommit -or (Test-Path -LiteralPath $probeRoot)) { throw 'Owned cleanup did not complete.' }
if ([IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($outsideRoot)) -cne [IO.Path]::GetTempPath().TrimEnd('\')) { throw 'Invalid test target cleanup scope.' }
Remove-Item -LiteralPath $outsideRoot -Recurse -Force
Write-Host 'Cleanup guard checks passed: missing marker, wrong token, junction rejection/outside preservation, owned cleanup.'
