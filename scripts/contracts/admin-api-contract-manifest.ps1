param(
    [ValidateSet('check', 'update')]
    [string]$Mode = 'check'
)

$ErrorActionPreference = 'Stop'

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..' '..')).Path
$contractDirectory = Join-Path $repositoryRoot 'contracts/admin-api/v2'
$fixtureDirectory = Join-Path $contractDirectory 'fixtures'
$manifestPath = Join-Path $contractDirectory 'manifest.json'
$contractName = 'promptdock-relay-admin-api-v2'
$newline = [string][char]10

if (-not (Test-Path -LiteralPath $fixtureDirectory -PathType Container)) {
    throw 'contracts/admin-api/v2/fixtures is missing'
}

$files = @(Get-ChildItem -LiteralPath $fixtureDirectory -File -Filter '*-v2.json' | Sort-Object -Property Name)
if ($files.Count -eq 0) {
    throw 'no Admin API v2 fixtures were found'
}

$lines = [System.Collections.Generic.List[string]]::new()
$lines.Add('{')
$lines.Add('  "manifestVersion": 1,')
$lines.Add(('  "contract": "{0}",' -f $contractName))
$lines.Add('  "fixtures": [')
for ($index = 0; $index -lt $files.Count; $index++) {
    $file = $files[$index]
    if ($file.Name -cnotmatch '^[a-z0-9][a-z0-9.-]*-v2\.json$') {
        throw "fixture name is not canonical: $($file.Name)"
    }
    $sha256 = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    $lines.Add('    {')
    $lines.Add(('      "path": "{0}",' -f $file.Name))
    $lines.Add(('      "sha256": "{0}",' -f $sha256))
    $lines.Add('      "mediaType": "application/json",')
    $lines.Add('      "schemaVersion": 2')
    $suffix = if ($index -eq $files.Count - 1) { '    }' } else { '    },' }
    $lines.Add($suffix)
}
$lines.Add('  ]')
$lines.Add('}')
$expected = [string]::Join($newline, $lines) + $newline

if ($Mode -eq 'check') {
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        throw 'contracts/admin-api/v2/manifest.json is missing'
    }
    $actual = [System.IO.File]::ReadAllText($manifestPath)
    if ($actual -cne $expected) {
        throw 'Admin API contract manifest drifted; run scripts/contracts/admin-api-contract-manifest.ps1 update'
    }
    Write-Output 'Admin API contract manifest is current'
    exit 0
}

$temporaryPath = Join-Path $contractDirectory ".manifest.$PID.tmp"
try {
    [System.IO.File]::WriteAllText(
        $temporaryPath,
        $expected,
        [System.Text.UTF8Encoding]::new($false)
    )
    Move-Item -LiteralPath $temporaryPath -Destination $manifestPath -Force
} finally {
    if (Test-Path -LiteralPath $temporaryPath) {
        Remove-Item -LiteralPath $temporaryPath -Force
    }
}
Write-Output 'Admin API contract manifest updated'
