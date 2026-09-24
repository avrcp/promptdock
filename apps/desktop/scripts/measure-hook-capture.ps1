[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Executable,
    [ValidateRange(1, 200)][int]$Iterations = 20
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0

function Assert-RegularFile {
    param([Parameter(Mandatory = $true)][string]$Path, [Parameter(Mandatory = $true)][string]$Label)
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if (-not ($item -is [IO.FileInfo]) -or (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
        throw "$Label must be a regular file."
    }
    return $item.FullName
}

function New-CaptureFixture {
    param([Parameter(Mandatory = $true)][string]$Directory)

    $registration = [guid]::NewGuid().ToString()
    [IO.File]::WriteAllText((Join-Path $Directory 'hook-registration.json'), (ConvertTo-Json $registration -Compress), [Text.UTF8Encoding]::new($false))
    [IO.File]::WriteAllText((Join-Path $Directory 'hook-target.json'), (ConvertTo-Json $Directory -Compress), [Text.UTF8Encoding]::new($false))
    $policy = [ordered]@{
        schemaVersion = 1
        policy = [ordered]@{
            observe_turns = $true
            notify_started = $true
            notify_ended = $true
            completion_quiet_ms = 2000
            result_content_mode = 'full_final'
            notify_attention = $false
            include_task_input = $false
        }
        revision = 1
        captureGeneration = 0
    } | ConvertTo-Json -Depth 4 -Compress
    [IO.File]::WriteAllText((Join-Path $Directory 'capture-policy.json'), $policy, [Text.UTF8Encoding]::new($false))
    return $registration
}

function Invoke-CaptureProcess {
    param(
        [Parameter(Mandatory = $true)][string]$ExecutablePath,
        [Parameter(Mandatory = $true)][string]$Registration,
        [Parameter(Mandatory = $true)][string]$Inbox,
        [Parameter(Mandatory = $true)][ValidateSet('codex-user-prompt', 'codex-stop')][string]$Event,
        [Parameter(Mandatory = $true)][string]$Marker,
        [Parameter(Mandatory = $true)][string]$InputText
    )

    $before = if (Test-Path -LiteralPath $Inbox -PathType Leaf) { (Get-Item -LiteralPath $Inbox).Length } else { 0 }
    $process = New-Object Diagnostics.Process
    $started = $false
    try {
        $startInfo = New-Object Diagnostics.ProcessStartInfo
        $startInfo.FileName = $ExecutablePath
        $startInfo.UseShellExecute = $false
        $startInfo.RedirectStandardInput = $true
        $startInfo.RedirectStandardOutput = $true
        $startInfo.RedirectStandardError = $true
        $startInfo.CreateNoWindow = $true
        $startInfo.Arguments = "--capture-agent-event --hook-registration-id $Registration $Event $Marker --inbox `"$Inbox`""
        $process.StartInfo = $startInfo
        $timer = [Diagnostics.Stopwatch]::StartNew()
        $started = $process.Start()
        if (-not $started) { throw 'Capture helper did not start.' }
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        $bytes = [Text.UTF8Encoding]::new($false).GetBytes($InputText)
        $process.StandardInput.BaseStream.Write($bytes, 0, $bytes.Length)
        $process.StandardInput.BaseStream.Flush()
        $process.StandardInput.Close()
        if (-not $process.WaitForExit(8000)) { throw 'Capture helper exceeded the eight-second measurement deadline.' }
        if (-not [Threading.Tasks.Task]::WaitAll(@($stdoutTask, $stderrTask), 2000)) { throw 'Capture helper output did not reach EOF.' }
        $timer.Stop()
        $expectedStdout = if ($Event -eq 'codex-stop') { '{}' } else { '' }
        $after = if (Test-Path -LiteralPath $Inbox -PathType Leaf) { (Get-Item -LiteralPath $Inbox).Length } else { 0 }
        if ($process.ExitCode -ne 0 -or $stdoutTask.Result -cne $expectedStdout -or $stderrTask.Result.Length -ne 0) {
            throw "Capture helper failed neutral contract (exit=$($process.ExitCode))."
        }
        if ($after -le $before) { throw 'Capture helper exited successfully without appending to the isolated inbox.' }
        return [pscustomobject]@{
            elapsed_ms = $timer.Elapsed.TotalMilliseconds
            input_bytes = $bytes.Length
            appended_bytes = $after - $before
        }
    } finally {
        try {
            if ($started -and -not $process.HasExited) {
                $process.Kill()
                $process.WaitForExit(2000) | Out-Null
            }
        } finally {
            $process.Dispose()
        }
    }
}

function Get-Percentile {
    param([Parameter(Mandatory = $true)][double[]]$Values, [Parameter(Mandatory = $true)][double]$Percentile)
    $ordered = @($Values | Sort-Object)
    return $ordered[[Math]::Ceiling($ordered.Count * $Percentile) - 1]
}

$executablePath = Assert-RegularFile -Path $Executable -Label 'Executable'
$tempBase = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd([char[]]@([char]92, [char]47)) + '\'
$root = Join-Path $tempBase "PromptDock capture measure 中文 空格 & $([guid]::NewGuid())"
try {
    New-Item -ItemType Directory -Path $root | Out-Null
    $inbox = Join-Path $root 'agent-events.jsonl'
    $registration = New-CaptureFixture -Directory $root
    $smallPrompt = 'measure small UserPromptSubmit'
    $maximumFinal = 'F' * (256 * 1024)
    $samples = [ordered]@{ UserPromptSubmit = New-Object Collections.Generic.List[object]; FullFinalStop = New-Object Collections.Generic.List[object] }
    $failures = New-Object Collections.Generic.List[string]

    for ($index = 1; $index -le $Iterations; $index++) {
        $session = "measure-session-$index"
        $turn = "measure-turn-$index"
        $prompt = @{ hook_event_name = 'UserPromptSubmit'; session_id = $session; turn_id = $turn; cwd = $root; model = 'measure-model'; prompt = $smallPrompt } | ConvertTo-Json -Compress
        try {
            $samples.UserPromptSubmit.Add((Invoke-CaptureProcess -ExecutablePath $executablePath -Registration $registration -Inbox $inbox -Event 'codex-user-prompt' -Marker '--promptdock-desktop-agent-event-user-prompt' -InputText $prompt))
        } catch { $failures.Add("UserPromptSubmit#${index}: $($_.Exception.Message)") }
        $stop = @{ hook_event_name = 'Stop'; session_id = $session; turn_id = $turn; cwd = $root; model = 'measure-model'; last_assistant_message = $maximumFinal } | ConvertTo-Json -Compress
        try {
            $samples.FullFinalStop.Add((Invoke-CaptureProcess -ExecutablePath $executablePath -Registration $registration -Inbox $inbox -Event 'codex-stop' -Marker '--promptdock-desktop-agent-event-stop' -InputText $stop))
        } catch { $failures.Add("FullFinalStop#${index}: $($_.Exception.Message)") }
    }

    $report = foreach ($name in $samples.Keys) {
        $measurements = $samples[$name].ToArray()
        $values = @($measurements | ForEach-Object { $_.elapsed_ms })
        [pscustomobject]@{
            scenario = $name
            iterations = $Iterations
            successes = $values.Count
            failures = $Iterations - $values.Count
            first_ms = if ($values.Count) { [Math]::Round($values[0], 2) } else { $null }
            p50_ms = if ($values.Count) { [Math]::Round((Get-Percentile -Values $values -Percentile 0.50), 2) } else { $null }
            p95_ms = if ($values.Count) { [Math]::Round((Get-Percentile -Values $values -Percentile 0.95), 2) } else { $null }
            max_ms = if ($values.Count) { [Math]::Round(($values | Measure-Object -Maximum).Maximum, 2) } else { $null }
            input_bytes_min = if ($measurements.Count) { ($measurements.input_bytes | Measure-Object -Minimum).Minimum } else { $null }
            input_bytes_max = if ($measurements.Count) { ($measurements.input_bytes | Measure-Object -Maximum).Maximum } else { $null }
            inbox_growth_bytes = if ($measurements.Count) { ($measurements.appended_bytes | Measure-Object -Sum).Sum } else { 0 }
        }
    }
    [pscustomobject]@{
        executable_sha256 = (Get-FileHash -LiteralPath $executablePath -Algorithm SHA256).Hash.ToLowerInvariant()
        final_body_bytes = [Text.Encoding]::UTF8.GetByteCount($maximumFinal)
        startup_scope = 'Fresh helper process per sample; elapsed includes Process.Start, stdin, durable append and output EOF. OS cold cache not controlled.'
        scenarios = @($report)
    } | ConvertTo-Json -Depth 4
    if ($failures.Count) { throw "Capture measurement failures:`n$($failures -join "`n")" }
} finally {
    if (Test-Path -LiteralPath $root) {
        $resolved = [IO.Path]::GetFullPath($root)
        if (-not $resolved.StartsWith($tempBase, [StringComparison]::OrdinalIgnoreCase)) { throw 'Refusing to remove a measurement directory outside the system temporary directory.' }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
