[CmdletBinding(SupportsShouldProcess = $true, ConfirmImpact = 'High')]
param()

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

$script:ResetFiles = @(
    'promptdock.db',
    'promptdock.db-wal',
    'promptdock.db-shm',
    'agent-events.jsonl',
    'agent-events.jsonl.lock',
    'agent-events.jsonl.maintenance.json',
    'hook-verification.json',
    'hook-verification.lock',
    'hook-observed-submit.json',
    'hook-observed-stop.json',
    'hook-observed-permission.json'
)
$script:InboxName = 'agent-events.jsonl'
$script:GuardName = '.runtime-access.guard'
$script:ResetPendingName = '.reset-pending'

function Test-RegularResetDirectory {
    param([Parameter(Mandatory = $true)][string]$Path)

    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if (-not ($item -is [IO.DirectoryInfo]) -or (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
        throw '本地运行数据目录必须是普通目录，不能是链接或重解析点。'
    }
    return [IO.Path]::GetFullPath($item.FullName).TrimEnd([char[]]@([char]92, [char]47))
}

function Get-DefaultResetRoot {
    $profile = [Environment]::GetFolderPath([Environment+SpecialFolder]::UserProfile)
    if ([string]::IsNullOrWhiteSpace($profile)) {
        throw '无法定位当前 Windows 用户 Profile。'
    }
    return [IO.Path]::Combine([IO.Path]::GetFullPath($profile), '.promptdock-desktop')
}

function Get-OwnedResetTargets {
    param([Parameter(Mandatory = $true)][string]$Root)

    $targets = New-Object Collections.Generic.List[string]
    foreach ($name in $script:ResetFiles) {
        $targets.Add((Join-Path $Root $name))
    }
    $prefix = "$script:InboxName.purge-"
    foreach ($item in @(Get-ChildItem -LiteralPath $Root -Force -ErrorAction Stop)) {
        if ($item.Name.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
            $id = $item.Name.Substring($prefix.Length)
            $parsed = [guid]::Empty
            if ([guid]::TryParse($id, [ref]$parsed)) {
                $targets.Add($item.FullName)
            }
        }
    }
    return @($targets | Select-Object -Unique)
}

function Test-ResetControlFile {
    param([Parameter(Mandatory = $true)][string]$Root, [Parameter(Mandatory = $true)][string]$Name)

    $path = [IO.Path]::GetFullPath((Join-Path $Root $Name))
    if ([IO.Path]::GetDirectoryName($path) -cne $Root) { throw '维护控制文件越出本地运行数据目录。' }
    if (-not (Test-Path -LiteralPath $path)) { return $path }
    $item = Get-Item -LiteralPath $path -Force -ErrorAction Stop
    if (-not ($item -is [IO.FileInfo]) -or (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
        throw "拒绝使用非普通维护文件：$Name"
    }
    return $path
}

function Test-ResetTarget {
    param(
        [Parameter(Mandatory = $true)][string]$Root,
        [Parameter(Mandatory = $true)][string]$Target
    )

    $resolved = [IO.Path]::GetFullPath($Target)
    if ([IO.Path]::GetDirectoryName($resolved) -cne $Root) {
        throw '清空目标越出本地运行数据目录。'
    }
    if (-not (Test-Path -LiteralPath $resolved)) { return $false }
    $item = Get-Item -LiteralPath $resolved -Force -ErrorAction Stop
    if (-not ($item -is [IO.FileInfo]) -or (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
        throw "拒绝删除非普通文件：$($item.Name)"
    }
    return $true
}

function Invoke-ClearLocalRuntimeData {
    [CmdletBinding(SupportsShouldProcess = $true, ConfirmImpact = 'High')]
    param(
        [Parameter(Mandatory = $true)][string]$Root,
        [switch]$AssumeConfirmed
    )

    if (-not (Test-Path -LiteralPath $Root -PathType Container)) {
        Write-Host "本地运行数据目录不存在，无需清空：$Root"
        return [pscustomobject]@{ Deleted = @(); Remaining = @(); Unknown = @() }
    }
    $rootPath = Test-RegularResetDirectory -Path $Root
    $targets = @(Get-OwnedResetTargets -Root $rootPath)
    $unknown = @(
        Get-ChildItem -LiteralPath $rootPath -Force -ErrorAction Stop |
            Where-Object { $targets -notcontains $_.FullName -and $_.Name -cne $script:GuardName } |
            ForEach-Object { $_.Name }
    )

    if (-not $WhatIfPreference) {
        Write-Host '将清空本机运行记录、待发送通知、轮次收件箱/Attention 已知晓状态、临时暂缓、体检测试记录、接收缓存和 Hook 验证记录。'
        Write-Host '待发送通知及本地未确认的重发/撤销意图会丢失。保留服务器连接、桌面启动设置、内容选项、已安装的 Hook、日志和用户导出。'
        Write-Host '不会删除服务器结果页或微信消息；这不是安全擦除；维护期间收到的新 Hook 不会被正常处理。'
        Write-Host "数据目录：$rootPath"
    }
    if ($WhatIfPreference) {
        $PSCmdlet.ShouldProcess($rootPath, '清空 PromptDock 本地运行数据') | Out-Null
        return [pscustomobject]@{ Deleted = @(); Remaining = @(); Unknown = $unknown }
    }
    # Test-only callers dot-source this file and provide the root explicitly;
    # the packaged entrypoint never supplies this bypass.
    if (-not $AssumeConfirmed -and -not $PSCmdlet.ShouldProcess($rootPath, '清空 PromptDock 本地运行数据')) {
        return [pscustomobject]@{ Deleted = @(); Remaining = @(); Unknown = $unknown }
    }

    # Confirmed only: FileShare.None blocks newly starting GUI/helper owners and
    # fails without deleting anything when a compatible runtime handle exists.
    $guardPath = Join-Path $rootPath $script:GuardName
    $guard = $null
    try {
        Test-ResetControlFile -Root $rootPath -Name $script:GuardName | Out-Null
        Test-ResetControlFile -Root $rootPath -Name $script:ResetPendingName | Out-Null
        try {
            $guard = [IO.File]::Open($guardPath, [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
        } catch [IO.IOException] {
            throw 'PromptDock 正在运行或维护中；请从托盘退出后重试。未删除任何运行文件。'
        }
        $rootPath = Test-RegularResetDirectory -Path $rootPath
        $targets = @(Get-OwnedResetTargets -Root $rootPath)
        foreach ($target in $targets) {
            if (Test-Path -LiteralPath $target) {
                Test-ResetTarget -Root $rootPath -Target $target | Out-Null
            }
        }
        $pendingPath = Test-ResetControlFile -Root $rootPath -Name $script:ResetPendingName
        [IO.File]::WriteAllText($pendingPath, "reset pending $(Get-Date -Format o)", [Text.UTF8Encoding]::new($false))
        $deleted = New-Object Collections.Generic.List[string]
        $remaining = New-Object Collections.Generic.List[string]
        foreach ($target in $targets) {
            try {
                if (-not (Test-ResetTarget -Root $rootPath -Target $target)) { continue }
                Remove-Item -LiteralPath $target -Force -ErrorAction Stop
                $deleted.Add([IO.Path]::GetFileName($target))
            } catch {
                $remaining.Add([IO.Path]::GetFileName($target))
            }
        }
        if ($remaining.Count -gt 0) {
            throw "部分本地运行数据未清空：$($remaining -join ', ')；已删除：$($deleted -join ', ')"
        }
        [IO.File]::Delete($pendingPath)
        Write-Host '本地运行数据已清空。重新启动 PromptDock 后，策略、连接、设置和 Hook 安装仍会保留；收件箱、暂缓和体检测试状态将从新的本机事件恢复。'
        return [pscustomobject]@{ Deleted = @($deleted); Remaining = @(); Unknown = $unknown }
    } finally {
        if ($null -ne $guard) { $guard.Dispose() }
    }
}

if ($MyInvocation.InvocationName -ne '.') {
    Invoke-ClearLocalRuntimeData -Root (Get-DefaultResetRoot) -WhatIf:$WhatIfPreference
}
