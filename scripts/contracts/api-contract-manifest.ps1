param(
    [ValidateSet('check', 'update')]
    [string]$Mode = 'check'
)

$ErrorActionPreference = 'Stop'

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..' '..')).Path
$fixtureDirectory = Join-Path $repositoryRoot 'contracts/server-http/v1'
$manifestPath = Join-Path $fixtureDirectory 'manifest.json'
$files = @(Get-ChildItem -LiteralPath $fixtureDirectory -File -Filter '*-v1.json' | Sort-Object -Property Name)
if ($files.Count -eq 0) {
    throw 'no public API v1 fixtures were found'
}

$lines = [System.Collections.Generic.List[string]]::new()
$lines.Add('{')
$lines.Add('  "manifestVersion": 1,')
$lines.Add('  "contract": "promptdock-relay-api-v1",')
$lines.Add('  "fixtures": [')
for ($index = 0; $index -lt $files.Count; $index++) {
    $file = $files[$index]
    if ($file.Name -cnotmatch '^[a-z0-9][a-z0-9.-]*-v1\.json$') {
        throw "fixture name is not canonical: $($file.Name)"
    }
    $sha256 = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    $lines.Add('    {')
    $lines.Add("      `"path`": `"$($file.Name)`",")
    $lines.Add("      `"sha256`": `"$sha256`",")
    $lines.Add('      "mediaType": "application/json",')
    $lines.Add('      "schemaVersion": 1')
    $suffix = if ($index -eq $files.Count - 1) { '    }' } else { '    },' }
    $lines.Add($suffix)
}
$lines.Add('  ]')
$lines.Add('}')
$expected = [string]::Join("`n", $lines) + "`n"

if ($Mode -eq 'check') {
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        throw 'contracts/server-http/v1/manifest.json is missing'
    }
    $actual = [System.IO.File]::ReadAllText($manifestPath)
    if ($actual -cne $expected) {
        throw 'API contract manifest drifted; run scripts/contracts/api-contract-manifest.ps1 update'
    }
    Write-Output 'API contract manifest is current'
    exit 0
}

$temporaryPath = Join-Path $fixtureDirectory ".manifest.$PID.tmp"
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
Write-Output 'API contract manifest updated'
