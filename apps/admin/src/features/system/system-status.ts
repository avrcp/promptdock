import type { ComponentHealth, HealthTone } from '@/contracts/common'
import type { DatabaseSizeBucket, RetentionResult, WorkerState } from '@/contracts/system'

export type SystemStatusTone = HealthTone

interface SystemStatusVisual {
  tone: SystemStatusTone
  label: string
}

const WORKER_STATE_VISUALS: Record<WorkerState, SystemStatusVisual> = {
  running: { tone: 'success', label: '运行中' },
  idle: { tone: 'muted', label: '空闲' },
  degraded: { tone: 'warning', label: '降级' },
  failed: { tone: 'danger', label: '失败' },
  disabled: { tone: 'muted', label: '已禁用' },
  stopping: { tone: 'warning', label: '停止中' },
}

export function workerStateVisual(state: WorkerState): SystemStatusVisual {
  return WORKER_STATE_VISUALS[state]
}

const DATABASE_INTEGRITY_VISUALS: Record<'ok' | 'degraded' | 'failed', SystemStatusVisual> = {
  ok: { tone: 'success', label: '正常' },
  degraded: { tone: 'warning', label: '降级' },
  failed: { tone: 'danger', label: '失败' },
}

export function databaseIntegrityVisual(status: 'ok' | 'degraded' | 'failed'): SystemStatusVisual {
  return DATABASE_INTEGRITY_VISUALS[status]
}

const SIZE_BUCKET_VISUALS: Record<DatabaseSizeBucket, string> = {
  lt_10mb: '< 10 MB',
  '10_to_100mb': '10–100 MB',
  '100_to_500mb': '100–500 MB',
  gt_500mb: '> 500 MB',
}

export function databaseSizeBucketVisual(bucket: DatabaseSizeBucket): string {
  return SIZE_BUCKET_VISUALS[bucket]
}

const RETENTION_RESULT_VISUALS: Record<RetentionResult, SystemStatusVisual> = {
  success: { tone: 'success', label: '成功' },
  partial: { tone: 'warning', label: '部分完成' },
  failed: { tone: 'danger', label: '失败' },
  not_observed: { tone: 'muted', label: '尚未观测' },
}

export function retentionResultVisual(result: RetentionResult): SystemStatusVisual {
  return RETENTION_RESULT_VISUALS[result]
}

const BIND_CLASS_VISUALS: Record<'loopback' | 'container', string> = {
  loopback: '回路',
  container: '容器网络',
}

export function bindClassVisual(bindClass: 'loopback' | 'container'): string {
  return BIND_CLASS_VISUALS[bindClass]
}

export function poolHealthVisual(health: ComponentHealth): SystemStatusVisual {
  switch (health) {
    case 'healthy':
      return { tone: 'success', label: '正常' }
    case 'degraded':
      return { tone: 'warning', label: '降级' }
    case 'failed':
      return { tone: 'danger', label: '失败' }
    case 'disabled':
      return { tone: 'muted', label: '已关闭' }
    case 'unknown':
      return { tone: 'muted', label: '未配置' }
  }
}
