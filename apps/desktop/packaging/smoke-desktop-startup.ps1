param(
    [Parameter(Mandatory = $true)]
    [ValidateScript({ Test-Path -LiteralPath $_ -PathType Leaf })]
    [string]$Executable,
    [ValidatePattern('^[0-9a-f]{40}$')]
    [string]$ExpectedClientCommit
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0

function Assert-ReceiptProperty {
    param(
        [Parameter(Mandatory = $true)]$Receipt,
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$Scenario
    )

    if ($Receipt.PSObject.Properties.Name -notcontains $Name) {
        throw "$Scenario self-test receipt is missing $Name."
    }
}

function Test-JsonInteger {
    param([Parameter(Mandatory = $true)]$Value)

    return $Value -is [sbyte] -or $Value -is [byte] -or $Value -is [int16] -or $Value -is [uint16] -or $Value -is [int32] -or $Value -is [uint32] -or $Value -is [int64] -or $Value -is [uint64]
}

function Test-ProbeEntryDisappeared {
    param([Parameter(Mandatory = $true)][Exception]$Exception)

    if ($Exception -is [IO.FileNotFoundException] -or $Exception -is [IO.DirectoryNotFoundException]) {
        return $true
    }
    if ($Exception -is [Management.Automation.MethodInvocationException]) {
        return $Exception.InnerException -is [IO.FileNotFoundException] -or $Exception.InnerException -is [IO.DirectoryNotFoundException]
    }
    return $false
}

function Assert-OrdinaryProbeTree {
    param([Parameter(Mandatory = $true)][string]$Root)

    $pending = New-Object 'System.Collections.Generic.Stack[string]'
    $pending.Push($Root)
    while ($pending.Count -gt 0) {
        $current = $pending.Pop()
        try {
            $attributes = [IO.File]::GetAttributes($current)
        } catch {
            if (Test-ProbeEntryDisappeared -Exception $_.Exception) { continue }
            throw
        }
        if (($attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Probe cleanup refused a reparse point: $current"
        }
        if (($attributes -band [IO.FileAttributes]::Directory) -ne 0) {
            try {
                foreach ($child in [IO.Directory]::EnumerateFileSystemEntries($current)) {
                    $pending.Push($child)
                }
            } catch {
                if (Test-ProbeEntryDisappeared -Exception $_.Exception) { continue }
                throw
            }
        }
    }
}

function Remove-ValidatedProbeDirectory {
    param(
        [Parameter(Mandatory = $true)][string]$Scenario,
        [Parameter(Mandatory = $true)][string]$CleanupToken,
        [Parameter(Mandatory = $true)][string]$ProbeDirectory
    )

    if ($CleanupToken -notmatch '^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$') {
        throw "$Scenario cleanup token is not a lowercase UUIDv4."
    }
    if (-not [IO.Path]::IsPathRooted($ProbeDirectory)) { throw "$Scenario probeDirectory is not an absolute path." }
    $rawProbeDirectory = $ProbeDirectory
    $probeDirectory = [IO.Path]::GetFullPath($rawProbeDirectory)
    if ($probeDirectory -cne $rawProbeDirectory.TrimEnd('\')) { throw "$Scenario probeDirectory is not normalized." }
    $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')
    $parent = [IO.Path]::GetDirectoryName($probeDirectory)
    if ([string]::IsNullOrEmpty($parent) -or -not [string]::Equals($parent.TrimEnd('\'), $tempRoot, [StringComparison]::OrdinalIgnoreCase)) {
        throw "$Scenario probeDirectory is not a direct child of the system temporary directory."
    }
    $expectedLeaf = 'PromptDock-native-startup-' + $CleanupToken.Replace('-', '')
    if ([IO.Path]::GetFileName($probeDirectory) -cne $expectedLeaf) {
        throw "$Scenario probeDirectory does not match the cleanup token."
    }
    if (-not (Test-Path -LiteralPath $probeDirectory -PathType Container)) {
        throw "$Scenario probeDirectory does not exist for cleanup."
    }
    $rootItem = Get-Item -LiteralPath $probeDirectory -Force -ErrorAction Stop
    if (($rootItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "$Scenario probeDirectory is a reparse point."
    }
    $markerPath = Join-Path $probeDirectory '.startup-probe.json'
    if (-not (Test-Path -LiteralPath $markerPath -PathType Leaf)) {
        throw "$Scenario probe marker does not exist."
    }
    $markerItem = Get-Item -LiteralPath $markerPath -Force -ErrorAction Stop
    if (($markerItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or $markerItem.PSIsContainer -or $markerItem.Length -gt 4096) {
        throw "$Scenario probe marker is not an ordinary file within 4 KiB."
    }
    try {
        $marker = [IO.File]::ReadAllText($markerPath, [Text.UTF8Encoding]::new($false, $true)) | ConvertFrom-Json -ErrorAction Stop
    } catch {
        throw "$Scenario probe marker is not valid UTF-8 JSON."
    }
    foreach ($name in @('schemaVersion', 'scenario', 'buildCommit', 'cleanupToken')) {
        Assert-ReceiptProperty -Receipt $marker -Name $name -Scenario $Scenario
    }
    if (-not (Test-JsonInteger $marker.schemaVersion) -or $marker.schemaVersion -ne 1 -or $marker.scenario -isnot [string] -or $marker.scenario -cne $Scenario -or $marker.buildCommit -isnot [string] -or $marker.buildCommit -notmatch '^[0-9a-f]{40}$' -or $marker.cleanupToken -isnot [string] -or $marker.cleanupToken -cne $CleanupToken) {
        throw "$Scenario probe marker does not match this probe's known ownership."
    }
    Assert-OrdinaryProbeTree -Root $probeDirectory
    $deadline = [Diagnostics.Stopwatch]::StartNew()
    $lastFailure = $null
    do {
        try {
            Remove-Item -LiteralPath $probeDirectory -Recurse -Force -ErrorAction Stop
            if (-not (Test-Path -LiteralPath $probeDirectory)) { return $marker.buildCommit }
        } catch {
            $lastFailure = $_.Exception.Message
        }
        Start-Sleep -Milliseconds 200
    } while ($deadline.ElapsedMilliseconds -lt 10000)
    throw "$Scenario probe cleanup did not finish within 10 seconds: $lastFailure"
}

function Assert-ReceiptProbeOwnership {
    param(
        [Parameter(Mandatory = $true)]$Receipt,
        [Parameter(Mandatory = $true)][string]$Scenario,
        [Parameter(Mandatory = $true)][string]$CleanupToken,
        [Parameter(Mandatory = $true)][string]$ProbeDirectory,
        [Parameter(Mandatory = $true)][string]$MarkerBuildCommit
    )

    foreach ($name in @('scenario', 'buildCommit', 'probeDirectory', 'cleanupToken')) {
        Assert-ReceiptProperty -Receipt $Receipt -Name $name -Scenario $Scenario
    }
    if ($Receipt.scenario -isnot [string] -or $Receipt.scenario -cne $Scenario -or $Receipt.buildCommit -isnot [string] -or $Receipt.buildCommit -cne $MarkerBuildCommit -or $Receipt.probeDirectory -isnot [string] -or -not [string]::Equals($Receipt.probeDirectory.TrimEnd('\'), $ProbeDirectory, [StringComparison]::OrdinalIgnoreCase) -or $Receipt.cleanupToken -isnot [string] -or $Receipt.cleanupToken -cne $CleanupToken) {
        throw "$Scenario receipt does not match this probe's known ownership."
    }
}

function Invoke-DesktopStartupScenario {
    param(
        [Parameter(Mandatory = $true)][string]$Scenario,
        [Parameter(Mandatory = $true)][int]$ExpectedExitCode,
        [Parameter(Mandatory = $true)][bool]$ExpectedSetupCompleted,
        [Parameter(Mandatory = $true)][bool]$ExpectedPageLoaded,
        [Parameter(Mandatory = $true)][bool]$ExpectedFrontendReady,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$ExpectedErrorCode
    )

    $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')
    do {
        $cleanupToken = [guid]::NewGuid().ToString('D').ToLowerInvariant()
        $probeDirectory = Join-Path $tempRoot ('PromptDock-native-startup-' + $cleanupToken.Replace('-', ''))
    } while (Test-Path -LiteralPath $probeDirectory)

    $process = New-Object Diagnostics.Process
    $started = $false
    $stdoutTask = $null
    $stderrTask = $null
    $streamsReachedEof = $false
    $cleanupCompleted = $false
    try {
        $startInfo = New-Object Diagnostics.ProcessStartInfo
        $startInfo.FileName = $resolvedExecutable
        $startInfo.Arguments = "--self-test-desktop-startup $Scenario $cleanupToken"
        $startInfo.UseShellExecute = $false
        $startInfo.RedirectStandardOutput = $true
        $startInfo.RedirectStandardError = $true
        $startInfo.CreateNoWindow = $true
        $process.StartInfo = $startInfo
        $started = $process.Start()
        if (-not $started) { throw "$Scenario self-test process did not start." }

        # Begin both readers before waiting so a faulty receipt cannot fill a pipe.
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(40000)) {
            throw "$Scenario self-test exceeded the 40-second deadline."
        }
        if (-not [Threading.Tasks.Task]::WaitAll(@($stdoutTask, $stderrTask), 2000)) {
            throw "$Scenario self-test output streams did not reach EOF."
        }
        $streamsReachedEof = $true

        $stdout = $stdoutTask.Result
        $stderr = $stderrTask.Result
        $maxOutputBytes = 64KB
        if ([Text.Encoding]::UTF8.GetByteCount($stdout) -gt $maxOutputBytes -or [Text.Encoding]::UTF8.GetByteCount($stderr) -gt $maxOutputBytes) {
            throw "$Scenario self-test exceeded the 64 KiB output limit."
        }
        if ($stderr -match '(?im)\bpanic(?:ked)?\b') {
            throw "$Scenario self-test wrote a panic to stderr."
        }
        $lines = @($stdout -split "`r?`n" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
        if ($lines.Count -ne 1) {
            throw "$Scenario self-test must emit exactly one JSON receipt; received $($lines.Count) nonempty stdout lines."
        }
        try {
            $receipt = $lines[0] | ConvertFrom-Json -ErrorAction Stop
        } catch {
            throw "$Scenario self-test receipt is not complete JSON: $($_.Exception.Message)"
        }
        # This runs before the receipt's behavioral assertions so a failed
        # assertion cannot leave the EXE-owned WebView cache behind.
        $markerBuildCommit = Remove-ValidatedProbeDirectory -Scenario $Scenario -CleanupToken $cleanupToken -ProbeDirectory $probeDirectory
        $cleanupCompleted = $true
        Assert-ReceiptProbeOwnership -Receipt $receipt -Scenario $Scenario -CleanupToken $cleanupToken -ProbeDirectory $probeDirectory -MarkerBuildCommit $markerBuildCommit
        foreach ($name in @('scenario', 'buildCommit', 'setupCompleted', 'pageLoaded', 'frontendReady', 'runtimeStopped', 'errorCode', 'exitCode')) {
            Assert-ReceiptProperty -Receipt $receipt -Name $name -Scenario $Scenario
        }
        if ($receipt.scenario -isnot [string] -or $receipt.scenario -cne $Scenario) { throw "$Scenario receipt scenario does not match." }
        if ($receipt.buildCommit -isnot [string] -or $receipt.buildCommit -notmatch '^[0-9a-f]{40}$') { throw "$Scenario receipt has no full build commit." }
        if ($ExpectedClientCommit -and $receipt.buildCommit -cne $ExpectedClientCommit) { throw "$Scenario receipt build commit differs from ExpectedClientCommit." }
        if (-not (Test-JsonInteger $receipt.exitCode) -or $receipt.exitCode -ne $ExpectedExitCode -or $process.ExitCode -ne $ExpectedExitCode) {
            throw "$Scenario self-test exit code mismatch: process=$($process.ExitCode), receipt=$($receipt.exitCode), expected=$ExpectedExitCode."
        }
        foreach ($name in @('setupCompleted', 'pageLoaded', 'frontendReady', 'runtimeStopped')) {
            if ($receipt.$name -isnot [bool]) { throw "$Scenario receipt $name must be a JSON boolean." }
        }
        if ($receipt.setupCompleted -ne $ExpectedSetupCompleted -or $receipt.pageLoaded -ne $ExpectedPageLoaded -or $receipt.frontendReady -ne $ExpectedFrontendReady -or $receipt.runtimeStopped -ne $true) {
            throw "$Scenario receipt lifecycle fields are invalid."
        }
        if ([string]::IsNullOrEmpty($ExpectedErrorCode)) {
            if ($null -ne $receipt.errorCode) { throw "$Scenario receipt must have null errorCode." }
        } elseif ($receipt.errorCode -isnot [string] -or $receipt.errorCode -cne $ExpectedErrorCode) {
            throw "$Scenario receipt errorCode differs from $ExpectedErrorCode."
        }
        Write-Host "$Scenario desktop startup self-test passed." -ForegroundColor Green
    } finally {
        try {
            if ($started -and -not $process.HasExited) {
                $process.Kill()
                if (-not $process.WaitForExit(2000)) { throw "$Scenario self-test could not be reaped." }
            }
        } finally {
            try {
                if ($started -and $process.HasExited -and -not $streamsReachedEof -and $null -ne $stdoutTask -and $null -ne $stderrTask) {
                    $streamsReachedEof = [Threading.Tasks.Task]::WaitAll(@($stdoutTask, $stderrTask), 2000)
                    if (-not $streamsReachedEof) { throw "$Scenario self-test output streams did not reach EOF after process exit." }
                }
                if ($streamsReachedEof -and -not $cleanupCompleted -and (Test-Path -LiteralPath $probeDirectory)) {
                    $null = Remove-ValidatedProbeDirectory -Scenario $Scenario -CleanupToken $cleanupToken -ProbeDirectory $probeDirectory
                    $cleanupCompleted = $true
                }
            } finally {
                $process.Dispose()
            }
        }
    }
}

$resolvedExecutable = [IO.Path]::GetFullPath($Executable)
foreach ($case in @(
    @{ Scenario = 'fresh'; ExpectedExitCode = 0; ExpectedSetupCompleted = $true; ExpectedPageLoaded = $true; ExpectedFrontendReady = $true; ExpectedErrorCode = '' },
    @{ Scenario = 'legacy-policy'; ExpectedExitCode = 1; ExpectedSetupCompleted = $false; ExpectedPageLoaded = $false; ExpectedFrontendReady = $false; ExpectedErrorCode = 'CAPTURE_POLICY_UNAVAILABLE' },
    @{ Scenario = 'legacy-database'; ExpectedExitCode = 1; ExpectedSetupCompleted = $false; ExpectedPageLoaded = $false; ExpectedFrontendReady = $false; ExpectedErrorCode = 'UNSUPPORTED_SCHEMA' },
    @{ Scenario = 'reset-pending'; ExpectedExitCode = 1; ExpectedSetupCompleted = $false; ExpectedPageLoaded = $false; ExpectedFrontendReady = $false; ExpectedErrorCode = 'RUNTIME_RESET_INCOMPLETE' }
)) {
    Invoke-DesktopStartupScenario @case
}

Write-Host 'Portable desktop startup self-tests passed; real desktop Codex and phone acceptance NOT RUN.' -ForegroundColor Green
