$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0

. (Join-Path $PSScriptRoot 'clear-local-data.ps1')

function Assert-True {
    param([Parameter(Mandatory = $true)][bool]$Condition, [Parameter(Mandatory = $true)][string]$Message)
    if (-not $Condition) { throw $Message }
}

function Assert-Throws {
    param([Parameter(Mandatory = $true)][scriptblock]$Action, [Parameter(Mandatory = $true)][string]$Pattern)
    try {
        & $Action | Out-Null
    } catch {
        if ($_.Exception.Message -notlike $Pattern) { throw "Expected $Pattern, got $($_.Exception.Message)" }
        return
    }
    throw "Expected failure $Pattern"
}

function Assert-ConfirmationRefusalPreservesData {
    param([Parameter(Mandatory = $true)][string]$Root)

    $process = New-Object Diagnostics.Process
    try {
        $startInfo = New-Object Diagnostics.ProcessStartInfo
        $startInfo.FileName = 'powershell.exe'
        $startInfo.Arguments = '-NoProfile -ExecutionPolicy Bypass -Command ". $env:PD_RESET_SCRIPT; Invoke-ClearLocalRuntimeData -Root $env:PD_RESET_ROOT -Confirm"'
        $startInfo.UseShellExecute = $false
        $startInfo.RedirectStandardInput = $true
        $startInfo.RedirectStandardOutput = $true
        $startInfo.RedirectStandardError = $true
        $startInfo.CreateNoWindow = $true
        $startInfo.EnvironmentVariables['PD_RESET_SCRIPT'] = Join-Path $PSScriptRoot 'clear-local-data.ps1'
        $startInfo.EnvironmentVariables['PD_RESET_ROOT'] = $Root
        $process.StartInfo = $startInfo
        if (-not $process.Start()) { throw 'Confirmation refusal process did not start.' }
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        $process.StandardInput.WriteLine('N')
        $process.StandardInput.Close()
        if (-not $process.WaitForExit(8000)) { throw 'Confirmation refusal process timed out.' }
        [Threading.Tasks.Task]::WaitAll(@($stdoutTask, $stderrTask), 2000) | Out-Null
        Assert-True ($process.ExitCode -eq 0) "Confirmation refusal process failed: $($stderrTask.Result)"
    } finally {
        if (-not $process.HasExited) { $process.Kill(); $process.WaitForExit(2000) | Out-Null }
        $process.Dispose()
    }
}

function Assert-CmdEntryUsesExpectedScript {
    param(
        [Parameter(Mandatory = $true)][string]$Directory,
        [Parameter(Mandatory = $true)][string]$Label,
        [Parameter(Mandatory = $true)][bool]$UseSameDirectoryScript
    )

    New-Item -ItemType Directory -Path $Directory -Force | Out-Null
    $repositoryRoot = Split-Path -Parent $PSScriptRoot
    $commandName = 'clear-local-data.cmd'
    $commandPath = Join-Path $Directory $commandName
    Copy-Item -LiteralPath (Join-Path $repositoryRoot $commandName) -Destination $commandPath
    $scriptDirectory = if ($UseSameDirectoryScript) { $Directory } else { Join-Path $Directory 'scripts' }
    New-Item -ItemType Directory -Path $scriptDirectory -Force | Out-Null
    $evidence = Join-Path $Directory 'entry-evidence.txt'
    $stub = @'
param([switch]$Confirm, [switch]$WhatIf)
[IO.File]::WriteAllText($env:PD_CMD_EVIDENCE, "$env:PD_CMD_LABEL|$($Confirm.IsPresent)|$($WhatIf.IsPresent)", [Text.UTF8Encoding]::new($false))
exit 23
'@
    [IO.File]::WriteAllText((Join-Path $scriptDirectory 'clear-local-data.ps1'), $stub, [Text.UTF8Encoding]::new($true))

    $process = New-Object Diagnostics.Process
    $started = $false
    try {
        $startInfo = New-Object Diagnostics.ProcessStartInfo
        $startInfo.FileName = "$env:ComSpec"
        $startInfo.Arguments = "/d /c `"`"$commandPath`" -WhatIf`""
        $startInfo.WorkingDirectory = $Directory
        $startInfo.UseShellExecute = $false
        $startInfo.RedirectStandardOutput = $true
        $startInfo.RedirectStandardError = $true
        $startInfo.CreateNoWindow = $true
        $startInfo.EnvironmentVariables['PD_CMD_EVIDENCE'] = $evidence
        $startInfo.EnvironmentVariables['PD_CMD_LABEL'] = $Label
        $process.StartInfo = $startInfo
        $started = $process.Start()
        Assert-True $started 'CMD entry process did not start.'
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        Assert-True $process.WaitForExit(8000) 'CMD entry process timed out.'
        Assert-True ([Threading.Tasks.Task]::WaitAll(@($stdoutTask, $stderrTask), 2000)) 'CMD entry output readers did not finish.'
        Assert-True ($process.ExitCode -eq 23) "CMD entry did not preserve stub exit code: $($process.ExitCode); $($stderrTask.Result)"
        Assert-True (Test-Path -LiteralPath $evidence -PathType Leaf) 'CMD entry did not select the stub script.'
        Assert-True (([IO.File]::ReadAllText($evidence) -ceq "$Label|True|True")) 'CMD entry did not forward Confirm and WhatIf to the selected script.'
    } finally {
        try {
            if ($started -and -not $process.HasExited) { $process.Kill(); $process.WaitForExit(2000) | Out-Null }
        } finally {
            $process.Dispose()
        }
    }
}

$tempBase = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
$testRoot = Join-Path $tempBase "PromptDock reset 中文 空格 & $([guid]::NewGuid())"
try {
    New-Item -ItemType Directory -Path $testRoot | Out-Null
    $dataRoot = Join-Path $testRoot '.promptdock-desktop'
    New-Item -ItemType Directory -Path $dataRoot | Out-Null
    $missingRoot = Join-Path $testRoot 'missing-profile\.promptdock-desktop'
    Invoke-ClearLocalRuntimeData -Root $missingRoot -AssumeConfirmed | Out-Null
    Assert-True (-not (Test-Path -LiteralPath $missingRoot)) 'Missing directory reset created a directory.'
    [IO.File]::WriteAllText((Join-Path $dataRoot 'relay-connection.dpapi'), 'keep')
    [IO.File]::WriteAllText((Join-Path $dataRoot 'launcher.json'), 'keep')
    [IO.File]::WriteAllText((Join-Path $dataRoot 'capture-policy.json'), 'keep')
    [IO.File]::WriteAllText((Join-Path $dataRoot 'promptdock.log'), 'keep')
    [IO.File]::WriteAllText((Join-Path $dataRoot 'promptdock.db'), 'remove')
    [IO.File]::WriteAllText((Join-Path $dataRoot 'agent-events.jsonl'), 'remove')
    [IO.File]::WriteAllText((Join-Path $dataRoot 'hook-verification.json'), 'remove')
    [IO.File]::WriteAllText((Join-Path $dataRoot 'unknown.bin'), 'keep')
    $exports = Join-Path $dataRoot 'exports'
    New-Item -ItemType Directory -Path $exports | Out-Null
    [IO.File]::WriteAllText((Join-Path $exports 'user-export.txt'), 'keep')

    Assert-ConfirmationRefusalPreservesData -Root $dataRoot
    Assert-True (Test-Path -LiteralPath (Join-Path $dataRoot 'promptdock.db')) 'Confirmation refusal deleted a database.'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $dataRoot '.runtime-access.guard'))) 'Confirmation refusal created a guard.'

    Invoke-ClearLocalRuntimeData -Root $dataRoot -AssumeConfirmed -WhatIf | Out-Null
    Assert-True (Test-Path -LiteralPath (Join-Path $dataRoot 'promptdock.db')) 'WhatIf deleted a database.'
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $dataRoot '.runtime-access.guard'))) 'WhatIf created a guard.'

    $job = Start-Job -ScriptBlock {
        param([string]$GuardPath)
        $guard = [IO.File]::Open($GuardPath, [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::ReadWrite)
        try {
            Write-Output 'ready'
            Start-Sleep -Seconds 10
        } finally {
            $guard.Dispose()
        }
    } -ArgumentList (Join-Path $dataRoot '.runtime-access.guard')
    try {
        $ready = $false
        for ($attempt = 0; $attempt -lt 40; $attempt++) {
            if ((Receive-Job -Job $job -Keep -ErrorAction Stop) -contains 'ready') { $ready = $true; break }
            Start-Sleep -Milliseconds 50
        }
        Assert-True $ready 'Cross-process runtime guard did not become ready.'
        Assert-Throws -Action { Invoke-ClearLocalRuntimeData -Root $dataRoot -AssumeConfirmed } -Pattern '*'
        Assert-True (Test-Path -LiteralPath (Join-Path $dataRoot 'promptdock.db')) 'Guard contention deleted a database.'
    } finally {
        Stop-Job -Job $job -ErrorAction SilentlyContinue | Out-Null
        Remove-Job -Job $job -Force -ErrorAction SilentlyContinue
    }

    $result = Invoke-ClearLocalRuntimeData -Root $dataRoot -AssumeConfirmed
    Assert-True ($result.Remaining.Count -eq 0) 'Normal reset reported remaining files.'
    foreach ($name in @('promptdock.db', 'agent-events.jsonl', 'hook-verification.json')) {
        Assert-True (-not (Test-Path -LiteralPath (Join-Path $dataRoot $name))) "Reset retained $name."
    }
    foreach ($name in @('relay-connection.dpapi', 'launcher.json', 'capture-policy.json', 'promptdock.log')) {
        Assert-True (Test-Path -LiteralPath (Join-Path $dataRoot $name)) "Reset removed preserved file $name."
    }
    Assert-True (Test-Path -LiteralPath (Join-Path $dataRoot 'unknown.bin')) 'Reset removed an unknown file.'
    Assert-True (Test-Path -LiteralPath (Join-Path $exports 'user-export.txt')) 'Reset removed a user export.'
    $repeat = Invoke-ClearLocalRuntimeData -Root $dataRoot -AssumeConfirmed
    Assert-True ($repeat.Remaining.Count -eq 0) 'Repeated reset reported a failure.'

    [IO.File]::WriteAllText((Join-Path $dataRoot 'promptdock.db'), 'remove')
    [IO.File]::WriteAllText((Join-Path $dataRoot 'agent-events.jsonl'), 'remove')
    $job = Start-Job -ScriptBlock {
        param([string]$InboxPath)
        $inbox = [IO.File]::Open($InboxPath, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        try {
            Write-Output 'ready'
            Start-Sleep -Seconds 10
        } finally {
            $inbox.Dispose()
        }
    } -ArgumentList (Join-Path $dataRoot 'agent-events.jsonl')
    try {
        $ready = $false
        for ($attempt = 0; $attempt -lt 40; $attempt++) {
            if ((Receive-Job -Job $job -Keep -ErrorAction Stop) -contains 'ready') { $ready = $true; break }
            Start-Sleep -Milliseconds 50
        }
        Assert-True $ready 'Cross-process inbox lock did not become ready.'
        Assert-Throws -Action { Invoke-ClearLocalRuntimeData -Root $dataRoot -AssumeConfirmed } -Pattern '*部分本地运行数据未清空*'
        Assert-True (-not (Test-Path -LiteralPath (Join-Path $dataRoot 'promptdock.db'))) 'Partial failure did not delete the database first.'
        Assert-True (Test-Path -LiteralPath (Join-Path $dataRoot 'agent-events.jsonl')) 'Partial failure removed the locked inbox.'
        Assert-True (Test-Path -LiteralPath (Join-Path $dataRoot '.reset-pending')) 'Partial failure did not retain reset-pending.'
    } finally {
        Stop-Job -Job $job -ErrorAction SilentlyContinue | Out-Null
        Remove-Job -Job $job -Force -ErrorAction SilentlyContinue
    }
    Invoke-ClearLocalRuntimeData -Root $dataRoot -AssumeConfirmed | Out-Null
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $dataRoot '.reset-pending'))) 'Successful retry retained reset-pending.'

    $badRoot = Join-Path $testRoot 'directory-target\.promptdock-desktop'
    New-Item -ItemType Directory -Path $badRoot | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $badRoot 'promptdock.db') | Out-Null
    [IO.File]::WriteAllText((Join-Path $badRoot 'agent-events.jsonl'), 'must-not-delete')
    Assert-Throws -Action { Invoke-ClearLocalRuntimeData -Root $badRoot -AssumeConfirmed } -Pattern '*拒绝删除非普通文件*'
    Assert-True (Test-Path -LiteralPath (Join-Path $badRoot 'agent-events.jsonl')) 'Directory target preflight deleted another target.'

    Assert-CmdEntryUsesExpectedScript -Directory (Join-Path $testRoot 'CMD 同目录 空格 & 路径') -Label 'same-directory' -UseSameDirectoryScript $true
    Assert-CmdEntryUsesExpectedScript -Directory (Join-Path $testRoot 'CMD scripts 回退 空格 & 路径') -Label 'scripts-fallback' -UseSameDirectoryScript $false

    Write-Host 'clear-local-data isolated reset tests passed.' -ForegroundColor Green
} finally {
    $resolved = [IO.Path]::GetFullPath($testRoot)
    if (-not $resolved.StartsWith($tempBase, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Refusing to remove a reset test directory outside the system temporary directory.'
    }
    if (Test-Path -LiteralPath $resolved) {
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
