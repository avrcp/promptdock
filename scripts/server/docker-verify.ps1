param(
    [switch]$Pull
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (Test-Path -LiteralPath Variable:PSNativeCommandUseErrorActionPreference) {
    $PSNativeCommandUseErrorActionPreference = $false
}

function Get-FreeLoopbackPort {
    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    try {
        $listener.Start()
        ([Net.IPEndPoint]$listener.LocalEndpoint).Port
    }
    finally {
        $listener.Stop()
    }
}

$previousRelayPort = $env:PROMPTDOCK_RELAY_PORT
$previousWechatPort = $env:PROMPTDOCK_RELAY_WECHAT_PORT
$previousSourceCommit = $env:PROMPTDOCK_SOURCE_COMMIT
$sourceCommit = (& git rev-parse --verify HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $sourceCommit -notmatch '^[0-9a-f]{40}$' -or $sourceCommit -match '^0{40}$') {
    throw 'could not resolve a non-zero full Git commit for Docker build identity'
}
$env:PROMPTDOCK_SOURCE_COMMIT = $sourceCommit

$hostPort = if ($env:PROMPTDOCK_RELAY_PORT) {
    $parsedPort = 0
    if (-not [int]::TryParse($env:PROMPTDOCK_RELAY_PORT, [ref]$parsedPort) -or
        $parsedPort -lt 1 -or $parsedPort -gt 65535) {
        throw 'PROMPTDOCK_RELAY_PORT must be an integer between 1 and 65535'
    }
    $parsedPort
}
else {
    Get-FreeLoopbackPort
}

$wechatHostPort = if ($env:PROMPTDOCK_RELAY_WECHAT_PORT) {
    $parsedWechatPort = 0
    if (-not [int]::TryParse($env:PROMPTDOCK_RELAY_WECHAT_PORT, [ref]$parsedWechatPort) -or
        $parsedWechatPort -lt 1 -or $parsedWechatPort -gt 65535) {
        throw 'PROMPTDOCK_RELAY_WECHAT_PORT must be an integer between 1 and 65535'
    }
    $parsedWechatPort
}
else {
    do { $candidatePort = Get-FreeLoopbackPort } while ($candidatePort -eq $hostPort)
    $candidatePort
}
$env:PROMPTDOCK_RELAY_PORT = [string]$hostPort
$env:PROMPTDOCK_RELAY_WECHAT_PORT = [string]$wechatHostPort

$previousComposeProject = $env:COMPOSE_PROJECT_NAME
$composeProject = "promptdock-relay-verify-$([Guid]::NewGuid().ToString('N').Substring(0, 12))"
$env:COMPOSE_PROJECT_NAME = $composeProject
$cargoVolume = "${composeProject}_cargo-cache"
$dataVolume = "${composeProject}_relay-data"
$credentialVolume = "${composeProject}_relay-credential"
$image = 'promptdock-relay:dev'

function Invoke-Docker {
    param([Parameter(Mandatory)][string[]]$Arguments)

    & docker @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "docker $($Arguments -join ' ') failed with exit code $LASTEXITCODE"
    }
}

function Test-DockerVolume {
    param([Parameter(Mandatory)][string]$Name)

    $matches = @(& docker volume ls --quiet --filter "name=^$Name$")
    if ($LASTEXITCODE -ne 0) {
        throw 'docker volume list failed'
    }
    $matches -contains $Name
}

function Invoke-DockerCapture {
    param([Parameter(Mandatory)][string[]]$Arguments)

    $previousPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        $output = (& docker @Arguments 2>$null) -join "`n"
        $exitCode = $LASTEXITCODE
    }
    finally {
        $ErrorActionPreference = $previousPreference
    }
    [pscustomobject]@{ Output = $output; ExitCode = $exitCode }
}

function Invoke-RelayRequest {
    param(
        [Parameter(Mandatory)][string]$Path,
        [string]$Authorization,
        [ValidateSet('GET', 'POST', 'DELETE')][string]$Method = 'GET',
        [string]$Body,
        [int]$Port = $hostPort
    )

    $parameters = @{
        Uri = "http://127.0.0.1:$Port$Path"
        UseBasicParsing = $true
        Method = $Method
    }
    if ($Authorization) {
        $parameters.Headers = @{ Authorization = $Authorization }
    }
    if ($PSBoundParameters.ContainsKey('Body')) {
        $parameters.Body = $Body
        $parameters.ContentType = 'application/json'
    }
    $invokeWebRequest = Get-Command Invoke-WebRequest
    if ($invokeWebRequest.Parameters.ContainsKey('SkipHttpErrorCheck')) {
        $parameters.SkipHttpErrorCheck = $true
        return Invoke-WebRequest @parameters
    }

    try {
        return Invoke-WebRequest @parameters
    }
    catch [System.Net.WebException] {
        $errorContent = $_.ErrorDetails.Message
        $rawResponse = $_.Exception.Response
        if ($null -eq $rawResponse) {
            throw
        }
        $statusCode = [int]$rawResponse.StatusCode
        $headers = $rawResponse.Headers
        $stream = $rawResponse.GetResponseStream()
        $reader = New-Object System.IO.StreamReader($stream)
        try {
            $content = $reader.ReadToEnd()
        }
        finally {
            $reader.Dispose()
            $rawResponse.Dispose()
        }
        if ([string]::IsNullOrEmpty($content) -and -not [string]::IsNullOrEmpty($errorContent)) {
            $content = $errorContent
        }
        return [pscustomobject]@{
            StatusCode = $statusCode
            Content = $content
            Headers = $headers
        }
    }
}

function Assert-SafetyHeaders {
    param([Parameter(Mandatory)]$Response)

    if ($Response.Headers['Content-Type'] -ne 'application/json' -or
        $Response.Headers['Cache-Control'] -ne 'no-store' -or
        $Response.Headers['X-Content-Type-Options'] -ne 'nosniff') {
        throw 'HTTP safety header contract mismatch'
    }
    $requestId = [string]$Response.Headers['X-Request-Id']
    $parsedRequestId = [Guid]::Empty
    if (-not [Guid]::TryParse($requestId, [ref]$parsedRequestId)) {
        throw 'response did not return a UUID request ID'
    }
}

function Assert-JsonResponse {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][int]$Status,
        [string]$Body,
        [string]$Authorization
    )

    $response = Invoke-RelayRequest -Path $Path -Authorization $Authorization
    if ($response.StatusCode -ne $Status -or
        ($PSBoundParameters.ContainsKey('Body') -and $response.Content -ne $Body)) {
        throw "$Path response contract mismatch: status=$($response.StatusCode) body=$($response.Content)"
    }
    Assert-SafetyHeaders -Response $response
    $response
}

function Stop-RelayAndAssertGracefulShutdown {
    $containerId = (& docker compose ps --quiet relay).Trim()
    if ($LASTEXITCODE -ne 0 -or -not $containerId) {
        throw 'could not resolve the relay container ID'
    }
    Invoke-Docker @('compose', 'stop', 'relay')
    $exitCode = (& docker inspect --format '{{.State.ExitCode}}' $containerId).Trim()
    $logs = (& docker compose logs --no-color relay) -join "`n"
    if ($LASTEXITCODE -ne 0 -or $exitCode -ne '0' -or
        $logs -notmatch 'shutdown requested' -or $logs -notmatch 'shutdown complete') {
        throw "SIGTERM shutdown contract failed; exit code: $exitCode"
    }
}

Invoke-Docker @('compose', 'config', '--quiet')

$buildArguments = @('compose', 'build')
if ($Pull) {
    $buildArguments += '--pull'
}
$buildArguments += @('quality', 'relay')
Invoke-Docker $buildArguments
$qualityImage = "${composeProject}-quality"
& docker image inspect $qualityImage *> $null
if ($LASTEXITCODE -ne 0) {
    throw 'could not resolve the quality image ID'
}

Invoke-Docker @('compose', '--profile', 'wechat-bootstrap', 'down', '--remove-orphans')
if (Test-DockerVolume -Name $dataVolume) {
    Invoke-Docker @('volume', 'rm', $dataVolume)
}

try {
    Invoke-Docker @('volume', 'create', $dataVolume)
    & docker run --rm --user 0 --volume "${dataVolume}:/schema-upgrade" --entrypoint /bin/sh $image -c 'chown 10001:10001 /schema-upgrade'
    if ($LASTEXITCODE -ne 0) {
        throw 'could not prepare the same-volume legacy schema fixture'
    }
    $legacySchemaMount = "${dataVolume}:/schema-upgrade"
    Invoke-Docker @(
        'compose', 'run', '--rm', '--no-deps',
        '--volume', $legacySchemaMount,
        '--env', 'PROMPTDOCK_SCHEMA_UPGRADE_DB=/schema-upgrade/relay.db',
        'quality', 'cargo', 'test', '--locked', '--test', 'schema_upgrade_docker',
        'seed_v3_named_volume', '--', '--ignored', '--exact'
    )
    $legacyInitResult = Invoke-DockerCapture @(
        'compose', 'run', '--rm', '--no-deps', 'relay',
        'init', '--config', '/etc/promptdock-relay/config.toml'
    )
    if ($legacyInitResult.ExitCode -eq 0) {
        throw 'legacy v3 same-volume runtime init unexpectedly succeeded'
    }
    Invoke-Docker @(
        'compose', 'run', '--rm', '--no-deps',
        '--volume', $legacySchemaMount,
        '--env', 'PROMPTDOCK_SCHEMA_UPGRADE_DB=/schema-upgrade/relay.db',
        'quality', 'cargo', 'test', '--locked', '--test', 'schema_upgrade_docker',
        'verify_v3_named_volume_unchanged', '--', '--ignored', '--exact'
    )
    Invoke-Docker @('volume', 'rm', $dataVolume)

    Invoke-Docker @('compose', 'run', '--rm', 'quality')

    $initResult = Invoke-DockerCapture @('compose', 'run', '--rm', '--no-deps', 'relay', 'init', '--config', '/etc/promptdock-relay/config.toml')
    if ($initResult.ExitCode -ne 0) {
        throw 'clean-volume init failed in the runtime image'
    }
    $initReport = $initResult.Output | ConvertFrom-Json
    if ($initReport.status -ne 'initialized' -or
        $initReport.schemaIdentity -ne 'promptdock-relay-v4' -or
        $initReport.schemaRevision -ne 3) {
        throw 'clean-volume schema identity or revision contract failed'
    }

    $initialStatusResult = Invoke-DockerCapture @('compose', 'run', '--rm', '--no-deps', 'relay', 'status', '--config', '/etc/promptdock-relay/config.toml')
    if ($initialStatusResult.ExitCode -ne 0) {
        throw 'clean-volume status failed in the runtime image'
    }
    $initialStatus = $initialStatusResult.Output | ConvertFrom-Json
    if ($initialStatus.status -ne 'ok' -or
        $initialStatus.devices.total -ne 0 -or
        $initialStatus.selections.contexts -ne 0 -or
        $initialStatus.selections.entries -ne 0 -or
        $initialStatus.retentionBacklog.outbox -ne 0 -or
        $initialStatus.retentionBacklog.inbound -ne 0 -or
        $initialStatus.retentionBacklog.selections -ne 0 -or
        $initialStatusResult.Output -match '(?i)body|messageKey|fingerprint|handle|databasePath') {
        throw 'clean-volume safe status aggregation contract failed'
    }

    $doctorResult = Invoke-DockerCapture @('compose', 'run', '--rm', '--no-deps', 'relay', 'doctor', '--config', '/etc/promptdock-relay/config.toml')
    if ($doctorResult.ExitCode -ne 0 -or ($doctorResult.Output | ConvertFrom-Json).status -ne 'ok') {
        throw 'clean-volume doctor contract failed'
    }

    $createResult = Invoke-DockerCapture @('compose', 'run', '--rm', '--no-deps', 'relay', 'device', 'create', '--name', 'DOCKER-TEST', '--scope', 'notify:write', '--scope', 'notify:read_own', '--scope', 'channel:read', '--scope', 'channel:manage', '--config', '/etc/promptdock-relay/config.toml')
    $createOutput = $createResult.Output
    if ($createResult.ExitCode -ne 0) {
        throw 'device create failed in the runtime image'
    }
    $idMatch = [regex]::Match($createOutput, '(?m)^device_id=([0-9a-f-]{36})\r?$')
    $tokenMatch = [regex]::Match($createOutput, '(?m)^device_token=(pdv2\.[0-9a-f-]{36}\.[A-Za-z0-9_-]{43})\r?$')
    if (-not $idMatch.Success -or -not $tokenMatch.Success) {
        throw 'device create output did not match the credential contract'
    }
    $deviceId = $idMatch.Groups[1].Value
    $deviceToken = $tokenMatch.Groups[1].Value
    $secret = $deviceToken.Split('.')[2]
    if ([regex]::Matches($createOutput, [regex]::Escape($deviceToken)).Count -ne 1) {
        throw 'device token was not printed exactly once'
    }

    $listResult = Invoke-DockerCapture @('compose', 'run', '--rm', '--no-deps', 'relay', 'device', 'list', '--config', '/etc/promptdock-relay/config.toml')
    $listOutput = $listResult.Output
    if ($listResult.ExitCode -ne 0 -or $listOutput -match [regex]::Escape($secret) -or
        $listOutput -match 'token_hash') {
        throw 'safe device list contract failed'
    }
    $listed = $listOutput | ConvertFrom-Json
    if ($listed.Count -ne 1 -or $listed[0].id -ne $deviceId -or
        $listed[0].name -ne 'DOCKER-TEST' -or -not $listed[0].enabled) {
        throw 'created device did not persist into SQLite'
    }

    Invoke-Docker @('compose', 'up', '--detach', '--wait', 'relay')
    $containerId = (& docker compose ps --quiet relay).Trim()
    $hardening = (& docker inspect --format 'User={{.Config.User}} ReadonlyRootfs={{.HostConfig.ReadonlyRootfs}} CapDrop={{json .HostConfig.CapDrop}} SecurityOpt={{json .HostConfig.SecurityOpt}} Init={{.HostConfig.Init}} PidsLimit={{.HostConfig.PidsLimit}} Memory={{.HostConfig.Memory}} NanoCpus={{.HostConfig.NanoCpus}} Health={{.State.Health.Status}}' $containerId).Trim()
    $expectedHardening = 'User=promptdock ReadonlyRootfs=true CapDrop=["ALL"] SecurityOpt=["no-new-privileges:true"] Init=true PidsLimit=256 Memory=2147483648 NanoCpus=2000000000 Health=healthy'
    if ($LASTEXITCODE -ne 0 -or $hardening -ne $expectedHardening) {
        throw "unexpected relay hardening: $hardening"
    }
    $rootfsProbe = Invoke-DockerCapture @('compose', 'exec', '-T', 'relay', 'sh', '-c', 'touch /rootfs-write-probe')
    if ($rootfsProbe.ExitCode -eq 0) {
        throw 'relay root filesystem is unexpectedly writable'
    }

    Assert-JsonResponse -Path '/health/live' -Status 200 -Body '{"status":"ok"}' | Out-Null
    Assert-JsonResponse -Path '/health/ready' -Status 200 -Body '{"status":"ready"}' | Out-Null
    $unauthorized = Assert-JsonResponse -Path '/v1/server-info' -Status 401
    $unauthorizedBody = $unauthorized.Content | ConvertFrom-Json
    if ($unauthorizedBody.error.code -ne 'UNAUTHORIZED' -or
        $unauthorized.Headers['WWW-Authenticate'] -ne 'Bearer') {
        throw 'missing credential response contract failed'
    }
    Assert-JsonResponse -Path '/v1/server-info' -Status 200 -Body '{"apiVersion":1,"serverVersion":"0.6.0-rc.1","features":["notifications","device_status_v1","device_scopes_v1","remote_gateway_v5","remote_runs_v2","remote_harness_control_v2","wechat_handoff_preflight_v1"],"wechatProtocolReference":"2.4.6"}' -Authorization "Bearer $deviceToken" | Out-Null
    Assert-JsonResponse -Path '/v1/gateway/ws' -Status 404 | Out-Null
    Assert-JsonResponse -Path '/v1/gateway/ws' -Status 404 -Authorization "Bearer $deviceToken" | Out-Null
    Assert-JsonResponse -Path '/v4/gateway/ws' -Status 404 -Authorization "Bearer $deviceToken" | Out-Null
    $handoffPreflight = Invoke-RelayRequest -Path '/v1/authorization/wechat-handoff' -Authorization "Bearer $deviceToken"
    if ($handoffPreflight.StatusCode -ne 204 -or -not [string]::IsNullOrEmpty($handoffPreflight.Content)) {
        throw 'WeChat handoff preflight contract failed'
    }
    $wrongToken = "pdv2.$deviceId.$('A' * 43)"
    $wrong = Invoke-RelayRequest -Path '/v1/server-info' -Authorization "Bearer $wrongToken"
    if ($wrong.StatusCode -ne 401 -or ($wrong.Content | ConvertFrom-Json).error.code -ne 'UNAUTHORIZED') {
        throw 'wrong credential response contract failed'
    }

    $deviceLogin = Invoke-RelayRequest -Path '/v1/channels/wechat/login' -Method POST -Body '{"forceFresh":false}' -Authorization "Bearer $deviceToken"
    if ($deviceLogin.StatusCode -ne 403 -or
        ($deviceLogin.Content | ConvertFrom-Json).error.code -ne 'CHANNEL_MANAGED_BY_ADMIN' -or
        $deviceLogin.Headers['Cache-Control'] -ne 'no-store, max-age=0' -or
        $deviceLogin.Headers['Pragma'] -ne 'no-cache') {
        throw 'Admin-owned WeChat Device rejection contract failed'
    }

    $renderedSentinel = 'SENTINEL_DOCKER_RENDERED_BODY_DO_NOT_LOG'
    $notificationId = "docker-notification-$($deviceId.Substring(0, 8))"
    $createdAt = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    $notification = [ordered]@{
        schemaVersion = 1
        notificationId = $notificationId
        dedupeKey = "docker-dedupe-$($deviceId.Substring(0, 8))"
        kind = 'test'
        priority = 100
        title = 'Docker notification'
        body = $renderedSentinel
        correlationKey = 'docker-correlation'
        createdAt = $createdAt
        expiresAt = $createdAt + 300000
    }
    $notificationJson = $notification | ConvertTo-Json -Compress
    $acceptedResponse = Invoke-RelayRequest -Path '/v1/notifications' -Method POST -Body $notificationJson -Authorization "Bearer $deviceToken"
    Assert-SafetyHeaders -Response $acceptedResponse
    $accepted = $acceptedResponse.Content | ConvertFrom-Json
    if ($acceptedResponse.StatusCode -ne 202 -or $accepted.notificationId -ne $notificationId -or
        $accepted.relayStatus -ne 'accepted' -or $accepted.existing -or -not $accepted.acceptedAt) {
        throw 'notification durable acceptance contract failed'
    }
    $acceptedAt = $accepted.acceptedAt

    $statusBody = $null
    for ($attempt = 0; $attempt -lt 30; $attempt++) {
        $statusResponse = Invoke-RelayRequest -Path "/v1/notifications/$notificationId" -Authorization "Bearer $deviceToken"
        if ($statusResponse.StatusCode -ne 200) {
            throw 'notification status lookup failed'
        }
        $statusBody = $statusResponse.Content | ConvertFrom-Json
        if ($statusBody.status -eq 'blocked_reconnect') {
            break
        }
        Start-Sleep -Milliseconds 100
    }
    if ($statusBody.status -ne 'blocked_reconnect' -or
        $statusBody.lastErrorCode -ne 'RECONNECT_REQUIRED' -or
        $null -ne $statusBody.providerMessageId -or
        $null -ne $statusBody.providerAcceptedAt) {
        throw 'disabled WeChat channel falsely reported provider acceptance'
    }
    if ($statusResponse.Content -match [regex]::Escape($renderedSentinel)) {
        throw 'notification status exposed rendered content'
    }

    $replayResponse = Invoke-RelayRequest -Path '/v1/notifications' -Method POST -Body $notificationJson -Authorization "Bearer $deviceToken"
    $replay = $replayResponse.Content | ConvertFrom-Json
    if ($replayResponse.StatusCode -ne 202 -or -not $replay.existing -or
        $replay.acceptedAt -ne $acceptedAt) {
        throw 'exact notification replay contract failed'
    }
    $conflict = $notificationJson | ConvertFrom-Json
    $conflict.body = 'different rendered body'
    $conflictResponse = Invoke-RelayRequest -Path '/v1/notifications' -Method POST -Body ($conflict | ConvertTo-Json -Compress) -Authorization "Bearer $deviceToken"
    if ($conflictResponse.StatusCode -ne 409 -or
        ($conflictResponse.Content | ConvertFrom-Json).error.code -ne 'IDEMPOTENCY_CONFLICT') {
        throw 'notification idempotency conflict contract failed'
    }

    Invoke-RelayRequest -Path '/health/live?probe=SENTINEL_DOCKER_QUERY_DO_NOT_LOG' -Authorization 'Bearer SENTINEL_DOCKER_SECRET_DO_NOT_LOG' | Out-Null
    $runningLogs = (& docker compose logs --no-color relay) -join "`n"
    if ($LASTEXITCODE -ne 0 -or
        $runningLogs -match 'SENTINEL_DOCKER_SECRET_DO_NOT_LOG|SENTINEL_DOCKER_QUERY_DO_NOT_LOG|SENTINEL_DOCKER_RENDERED_BODY_DO_NOT_LOG' -or
        $runningLogs -match [regex]::Escape($secret)) {
        throw 'a credential or sentinel leaked into relay logs'
    }

    Invoke-Docker @('kill', '--signal', 'KILL', $containerId)
    $crashExitCode = (& docker inspect --format '{{.State.ExitCode}}' $containerId).Trim()
    if ($LASTEXITCODE -ne 0 -or $crashExitCode -ne '137') {
        throw "SIGKILL crash contract failed; exit code: $crashExitCode"
    }
    Invoke-Docker @('compose', 'up', '--detach', '--wait', 'relay')
    $afterCrashStatus = Invoke-RelayRequest -Path "/v1/notifications/$notificationId" -Authorization "Bearer $deviceToken"
    if ($afterCrashStatus.StatusCode -ne 200 -or
        ($afterCrashStatus.Content | ConvertFrom-Json).status -ne 'blocked_reconnect') {
        throw 'notification did not survive SIGKILL and restart'
    }
    $afterCrashReplay = Invoke-RelayRequest -Path '/v1/notifications' -Method POST -Body $notificationJson -Authorization "Bearer $deviceToken"
    $afterCrashReplayBody = $afterCrashReplay.Content | ConvertFrom-Json
    if ($afterCrashReplay.StatusCode -ne 202 -or -not $afterCrashReplayBody.existing -or
        $afterCrashReplayBody.acceptedAt -ne $acceptedAt) {
        throw 'post-crash replay did not preserve durable acceptance identity'
    }

    Stop-RelayAndAssertGracefulShutdown

    $revokeResult = Invoke-DockerCapture @('compose', 'run', '--rm', '--no-deps', 'relay', 'device', 'revoke', $deviceId, '--config', '/etc/promptdock-relay/config.toml')
    $revokeOutput = $revokeResult.Output
    if ($revokeResult.ExitCode -ne 0 -or $revokeOutput -notmatch "revoked_device_id=$deviceId") {
        throw 'device revoke failed after server restart migration'
    }
    $firstRevokedList = (Invoke-DockerCapture @('compose', 'run', '--rm', '--no-deps', 'relay', 'device', 'list', '--config', '/etc/promptdock-relay/config.toml')).Output | ConvertFrom-Json
    $firstRevokedAt = $firstRevokedList[0].revokedAt
    $repeatRevoke = Invoke-DockerCapture @('compose', 'run', '--rm', '--no-deps', 'relay', 'device', 'revoke', $deviceId, '--config', '/etc/promptdock-relay/config.toml')
    if ($repeatRevoke.ExitCode -ne 0) {
        throw 'repeated device revoke was not idempotent'
    }
    $secondRevokedList = (Invoke-DockerCapture @('compose', 'run', '--rm', '--no-deps', 'relay', 'device', 'list', '--config', '/etc/promptdock-relay/config.toml')).Output | ConvertFrom-Json
    if ($secondRevokedList[0].enabled -or -not $firstRevokedAt -or
        $secondRevokedList[0].revokedAt -ne $firstRevokedAt) {
        throw 'repeated revoke changed the original revocation state'
    }

    Invoke-Docker @('compose', 'up', '--detach', '--wait', 'relay')
    Assert-JsonResponse -Path '/health/ready' -Status 200 -Body '{"status":"ready"}' | Out-Null
    $revoked = Invoke-RelayRequest -Path '/v1/server-info' -Authorization "Bearer $deviceToken"
    if ($revoked.StatusCode -ne 403 -or ($revoked.Content | ConvertFrom-Json).error.code -ne 'DEVICE_REVOKED') {
        throw 'revoked credential response contract failed'
    }
    $wrongAfterRevoke = Invoke-RelayRequest -Path '/v1/server-info' -Authorization "Bearer $wrongToken"
    if ($wrongAfterRevoke.StatusCode -ne 401) {
        throw 'wrong secret for a revoked device disclosed revocation state'
    }
    Stop-RelayAndAssertGracefulShutdown

    $finalLogs = (& docker compose logs --no-color relay) -join "`n"
    if ($LASTEXITCODE -ne 0 -or $finalLogs -match [regex]::Escape($secret) -or
        $finalLogs -match [regex]::Escape($renderedSentinel)) {
        throw 'credential or rendered notification leaked into logs after restart or revocation'
    }

    & docker run --rm --volume "${dataVolume}:/data:ro" --entrypoint /bin/sh $image -c 'for file in /data/*; do [ -f "$file" ] || continue; if grep -a -F -- "$1" "$file" >/dev/null; then exit 1; fi; done' sh $secret
    if ($LASTEXITCODE -ne 0) {
        throw 'device token secret leaked into SQLite files'
    }

    if (Test-DockerVolume -Name $credentialVolume) {
        Invoke-Docker @('volume', 'rm', $credentialVolume)
    }
    $missingCredential = Invoke-DockerCapture @('compose', '--profile', 'wechat-bootstrap', 'run', '--rm', '--no-deps', 'relay-wechat')
    if ($missingCredential.ExitCode -eq 0) {
        throw 'WeChat-enabled relay started without its systemd credential'
    }

    Invoke-Docker @('volume', 'create', $credentialVolume)
    & docker run --rm --user 0 --volume "${credentialVolume}:/run/credentials" --entrypoint /bin/sh $image -c 'umask 077; printf invalid > /run/credentials/relay-master-key; chown 10001:10001 /run/credentials/relay-master-key'
    if ($LASTEXITCODE -ne 0) {
        throw 'could not create invalid credential fixture'
    }
    $invalidCredential = Invoke-DockerCapture @('compose', '--profile', 'wechat-bootstrap', 'run', '--rm', '--no-deps', 'relay-wechat')
    if ($invalidCredential.ExitCode -eq 0) {
        throw 'WeChat-enabled relay accepted an invalid master credential'
    }

    $masterKeySentinel = 'PD_MASTER_KEY_SENTINEL_20260824!'
    $masterKeyEncoded = [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes($masterKeySentinel))
    & docker run --rm --user 0 --volume "${credentialVolume}:/run/credentials" --entrypoint /bin/sh $image -c 'umask 077; printf %s "$1" > /run/credentials/relay-master-key; chown 10001:10001 /run/credentials/relay-master-key' sh $masterKeyEncoded
    if ($LASTEXITCODE -ne 0) {
        throw 'could not create valid credential fixture'
    }
    $missingConfirmationCredential = Invoke-DockerCapture @('compose', '--profile', 'wechat-bootstrap', 'run', '--rm', '--no-deps', 'relay-wechat')
    if ($missingConfirmationCredential.ExitCode -eq 0) {
        throw 'WeChat-enabled relay started without its confirmation credential'
    }

    $confirmationKeySentinel = 'PD_CONFIRMATION_KEY_F6_20260827!'
    $confirmationKeyEncoded = [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes($confirmationKeySentinel))
    & docker run --rm --user 0 --volume "${credentialVolume}:/run/credentials" --entrypoint /bin/sh $image -c 'umask 077; printf %s "$1" > /run/credentials/relay-confirmation-key; chown 10001:10001 /run/credentials/relay-confirmation-key' sh $confirmationKeyEncoded
    if ($LASTEXITCODE -ne 0) {
        throw 'could not create valid confirmation credential fixture'
    }
    $wechatCreateResult = Invoke-DockerCapture @('compose', '--profile', 'wechat-bootstrap', 'run', '--rm', '--no-deps', 'relay-wechat', 'device', 'create', '--name', 'DOCKER-WECHAT', '--scope', 'notify:write', '--scope', 'notify:read_own', '--scope', 'channel:read', '--scope', 'channel:manage', '--config', '/etc/promptdock-relay/wechat.toml')
    $wechatCreateOutput = $wechatCreateResult.Output
    $wechatTokenMatch = [regex]::Match($wechatCreateOutput, '(?m)^device_token=(pdv2\.[0-9a-f-]{36}\.[A-Za-z0-9_-]{43})\r?$')
    if ($wechatCreateResult.ExitCode -ne 0 -or -not $wechatTokenMatch.Success) {
        throw 'could not create WeChat bootstrap device'
    }
    $wechatToken = $wechatTokenMatch.Groups[1].Value
    $wechatSecret = $wechatToken.Split('.')[2]
    Invoke-Docker @('compose', '--profile', 'wechat-bootstrap', 'up', '--detach', '--wait', 'relay-wechat')
    $wechatReady = Invoke-RelayRequest -Path '/health/ready' -Port $wechatHostPort
    if ($wechatReady.StatusCode -ne 200) {
        throw 'WeChat-enabled relay did not become ready'
    }
    $wechatInfo = Invoke-RelayRequest -Path '/v1/server-info' -Authorization "Bearer $wechatToken" -Port $wechatHostPort
    $wechatInfoBody = $wechatInfo.Content | ConvertFrom-Json
    if ($wechatInfo.StatusCode -ne 200 -or
        $wechatInfoBody.features.Count -ne 10 -or
        $wechatInfoBody.features[0] -ne 'notifications' -or
        $wechatInfoBody.features[1] -ne 'device_status_v1' -or
        $wechatInfoBody.features[2] -ne 'device_scopes_v1' -or
        $wechatInfoBody.features[3] -ne 'remote_gateway_v5' -or
        $wechatInfoBody.features[4] -ne 'remote_runs_v2' -or
        $wechatInfoBody.features[5] -ne 'remote_harness_control_v2' -or
        $wechatInfoBody.features[6] -ne 'wechat_handoff_preflight_v1' -or
        $wechatInfoBody.features[7] -ne 'wechat_login' -or
        $wechatInfoBody.features[8] -ne 'wechat_channel' -or
        $wechatInfoBody.features[9] -ne 'wechat_admin_login_grant_v1') {
        throw 'WeChat login capability negotiation failed'
    }
    $wechatHandoffPreflight = Invoke-RelayRequest -Path '/v1/authorization/wechat-handoff' -Authorization "Bearer $wechatToken" -Port $wechatHostPort
    if ($wechatHandoffPreflight.StatusCode -ne 204 -or -not [string]::IsNullOrEmpty($wechatHandoffPreflight.Content)) {
        throw 'WeChat handoff preflight failed in enabled runtime'
    }
    $wechatDeviceLogin = Invoke-RelayRequest -Path '/v1/channels/wechat/login' -Method POST -Body '{"forceFresh":false,"unknown":true}' -Authorization "Bearer $wechatToken" -Port $wechatHostPort
    if ($wechatDeviceLogin.StatusCode -ne 403 -or
        ($wechatDeviceLogin.Content | ConvertFrom-Json).error.code -ne 'CHANNEL_MANAGED_BY_ADMIN' -or
        $wechatDeviceLogin.Headers['Cache-Control'] -ne 'no-store, max-age=0' -or
        $wechatDeviceLogin.Headers['Pragma'] -ne 'no-cache') {
        throw 'WeChat-enabled Admin-owned Device rejection contract failed'
    }
    Invoke-Docker @('compose', '--profile', 'wechat-bootstrap', 'stop', 'relay-wechat')
    $wechatLogs = (& docker compose --profile wechat-bootstrap logs --no-color relay-wechat) -join "`n"
    if ($LASTEXITCODE -ne 0 -or
        $wechatLogs -match [regex]::Escape($wechatToken) -or
        $wechatLogs -match [regex]::Escape($wechatSecret) -or
        $wechatLogs -match [regex]::Escape($masterKeySentinel) -or
        $wechatLogs -match [regex]::Escape($masterKeyEncoded.Substring(0, 24)) -or
        $wechatLogs -match [regex]::Escape($confirmationKeySentinel) -or
        $wechatLogs -match [regex]::Escape($confirmationKeyEncoded.Substring(0, 24))) {
        throw 'WeChat bootstrap credential leaked into logs'
    }

    & docker run --rm --volume "${dataVolume}:/data:ro" --entrypoint /bin/sh $image -c 'for file in /data/*; do [ -f "$file" ] || continue; if grep -a -F -- "$1" "$file" >/dev/null || grep -a -F -- "$2" "$file" >/dev/null || grep -a -F -- "$3" "$file" >/dev/null; then exit 1; fi; done' sh $wechatSecret $masterKeyEncoded $confirmationKeyEncoded
    if ($LASTEXITCODE -ne 0) {
        throw 'WeChat device, master, or confirmation credential leaked into SQLite files'
    }

    Write-Output "Docker verification passed for isolated project ${composeProject}: v3 same-volume fail-before-write, v4 init/status/doctor, quality, encrypted SecretStore bootstrap, Admin-owned Device rejection boundary, auth, durable outbox, SIGKILL recovery, replay, revocation, and SIGTERM."
}
finally {
    & docker compose --profile wechat-bootstrap down --remove-orphans
    if ($LASTEXITCODE -ne 0) {
        Write-Warning 'Docker Compose cleanup failed; inspect with docker compose ps --all.'
    }
    if (Test-DockerVolume -Name $dataVolume) {
        & docker volume rm $dataVolume *> $null
        if ($LASTEXITCODE -ne 0) {
            Write-Warning "Docker data volume $dataVolume could not be removed."
        }
    }
    if (Test-DockerVolume -Name $credentialVolume) {
        & docker volume rm $credentialVolume *> $null
        if ($LASTEXITCODE -ne 0) {
            Write-Warning "Docker credential volume $credentialVolume could not be removed."
        }
    }
    if (Test-DockerVolume -Name $cargoVolume) {
        & docker volume rm $cargoVolume *> $null
        if ($LASTEXITCODE -ne 0) {
            Write-Warning "Docker cargo volume $cargoVolume could not be removed."
        }
    }
    if ($null -eq $previousComposeProject) {
        Remove-Item Env:COMPOSE_PROJECT_NAME -ErrorAction SilentlyContinue
    }
    else {
        $env:COMPOSE_PROJECT_NAME = $previousComposeProject
    }
    if ($null -eq $previousRelayPort) { Remove-Item Env:PROMPTDOCK_RELAY_PORT -ErrorAction SilentlyContinue }
    else { $env:PROMPTDOCK_RELAY_PORT = $previousRelayPort }
    if ($null -eq $previousWechatPort) { Remove-Item Env:PROMPTDOCK_RELAY_WECHAT_PORT -ErrorAction SilentlyContinue }
    else { $env:PROMPTDOCK_RELAY_WECHAT_PORT = $previousWechatPort }
    if ($null -eq $previousSourceCommit) { Remove-Item Env:PROMPTDOCK_SOURCE_COMMIT -ErrorAction SilentlyContinue }
    else { $env:PROMPTDOCK_SOURCE_COMMIT = $previousSourceCommit }
}
