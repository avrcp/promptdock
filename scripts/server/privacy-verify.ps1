$ErrorActionPreference = 'Stop'

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Push-Location $repositoryRoot
try {
    cargo test --locked -p promptdock-server --lib outbox::tests::sensitive_terminal_redaction_scans_database_wal_and_shm -- --exact
    $privacyExitCode = $LASTEXITCODE
}
finally {
    Pop-Location
}
if ($privacyExitCode -ne 0) { exit $privacyExitCode }

Write-Output 'Relay privacy DB/WAL/SHM scan passed'
