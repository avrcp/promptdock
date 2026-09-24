param(
    [string]$Tag = $env:GITHUB_REF_NAME
)

$ErrorActionPreference = "Stop"
$projectDir = Split-Path -Parent $PSScriptRoot
$package = Get-Content -LiteralPath (Join-Path $projectDir "package.json") -Raw | ConvertFrom-Json
$tauri = Get-Content -LiteralPath (Join-Path $projectDir "src-tauri\tauri.conf.json") -Raw | ConvertFrom-Json
$cargoText = Get-Content -LiteralPath (Join-Path $projectDir "src-tauri\Cargo.toml") -Raw
$cargoPackage = [regex]::Match(
    $cargoText,
    '(?ms)^\[package\]\s*.*?^version\s*=\s*"(?<version>[^"]+)"'
)
if (-not $cargoPackage.Success) { throw "Cargo package version was not found." }

$versions = @{
    package = [string]$package.version
    cargo = [string]$cargoPackage.Groups['version'].Value
    tauri = [string]$tauri.version
}
$uniqueVersions = @($versions.Values | Sort-Object -Unique)
if ($uniqueVersions.Count -ne 1) {
    throw "Version mismatch: package=$($versions.package), Cargo=$($versions.cargo), Tauri=$($versions.tauri)"
}

if (-not [string]::IsNullOrWhiteSpace($Tag) -and $Tag.StartsWith('v', [StringComparison]::OrdinalIgnoreCase)) {
    $numericIdentifier = '(?:0|[1-9]\d*)'
    $prereleaseIdentifier = '(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)'
    $buildIdentifier = '[0-9A-Za-z-]+'
    $semVerPattern = "(?<version>$numericIdentifier\.$numericIdentifier\.$numericIdentifier(?:-$prereleaseIdentifier(?:\.$prereleaseIdentifier)*)?(?:\+$buildIdentifier(?:\.$buildIdentifier)*)?)"
    if ($Tag -notmatch "^v$semVerPattern$") {
        throw "Release tag $Tag must use a complete SemVer value such as v1.3.0 or v1.3.0-beta.1."
    }
    if ($Matches['version'] -ne $versions.package) {
        throw "Release tag $Tag does not match project version $($versions.package)."
    }
}

Write-Host "Version contract: $($versions.package)" -ForegroundColor Green
