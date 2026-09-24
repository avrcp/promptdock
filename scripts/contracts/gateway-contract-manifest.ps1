param(
    [ValidateSet('check', 'update')]
    [string]$Mode = 'check'
)

$ErrorActionPreference = 'Stop'

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..' '..')).Path
$fixtureDirectory = Join-Path $repositoryRoot 'contracts/node-link/v5'
$manifestPath = Join-Path $fixtureDirectory 'manifest.json'
$newline = [string][char]10

if (-not (Test-Path -LiteralPath $fixtureDirectory -PathType Container)) {
    throw 'contracts/node-link/v5 is missing'
}

$files = @(Get-ChildItem -LiteralPath $fixtureDirectory -File -Filter '*-v5.json' | Sort-Object -Property Name)
if ($files.Count -eq 0) {
    throw 'no Gateway v5 fixtures were found'
}

$lines = [System.Collections.Generic.List[string]]::new()
$lines.Add('{')
$lines.Add('  "manifestVersion": 1,')
$lines.Add('  "contract": "promptdock-relay-gateway-v5",')
$lines.Add('  "fixtures": [')
for ($index = 0; $index -lt $files.Count; $index++) {
    $file = $files[$index]
    if ($file.Name -cnotmatch '^[a-z0-9][a-z0-9.-]*-v5\.json$') {
        throw "fixture name is not canonical: $($file.Name)"
    }
    $sha256 = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    $lines.Add('    {')
    $lines.Add(('      "path": "{0}",' -f $file.Name))
    $lines.Add(('      "sha256": "{0}",' -f $sha256))
    $lines.Add('      "mediaType": "application/json",')
    $lines.Add('      "schemaVersion": 5')
    $lines.Add($(if ($index -eq $files.Count - 1) { '    }' } else { '    },' }))
}
$lines.Add('  ]')
$lines.Add('}')
$expected = [string]::Join($newline, $lines) + $newline

if ($Mode -eq 'check') {
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        throw 'contracts/node-link/v5/manifest.json is missing'
    }
    if ([System.IO.File]::ReadAllText($manifestPath) -cne $expected) {
        throw 'Gateway v5 contract manifest drifted; run scripts/contracts/gateway-contract-manifest.ps1 update'
    }
    Write-Output 'Gateway v5 contract manifest is current'
    exit 0
}

$temporaryPath = Join-Path $fixtureDirectory ".manifest.$PID.tmp"
try {
    [System.IO.File]::WriteAllText($temporaryPath, $expected, [System.Text.UTF8Encoding]::new($false))
    Move-Item -LiteralPath $temporaryPath -Destination $manifestPath -Force
} finally {
    if (Test-Path -LiteralPath $temporaryPath) {
        Remove-Item -LiteralPath $temporaryPath -Force
    }
}
Write-Output 'Gateway v5 contract manifest updated'
