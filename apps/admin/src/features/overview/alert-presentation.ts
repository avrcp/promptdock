import type { CurrentAlert } from '@/contracts/common'

/**
 * Map a current alert to a presentation that drives the Overview's
 * "now-what" layer.  Centralising the mapping keeps OverviewPage free of
 * switch-on-code noise and lets us add new severities / actions in one
 * place.
 */
export interface AlertPresentation {
  priority: number
  title: string
  targetRoute: string | null
  actionLabel: string | null
}

const ROUTE_BY_COMPONENT: Record<string, string> = {
  wechat: '/wechat',
  queue: '/queue',
  devices: '/devices',
  system: '/system',
  relay: '/overview',
}

export function presentAlert(alert: CurrentAlert): AlertPresentation {
  const targetRoute = ROUTE_BY_COMPONENT[alert.component] ?? '/overview'
  // Prioritise by severity so the most actionable item bubbles to the top
  // without forcing the operator to scan the whole list.
  const priority = alert.severity === 'error' ? 0 : alert.severity === 'warning' ? 1 : 2
  const actionLabel =
    alert.severity === 'error' ? '立即处理' : alert.severity === 'warning' ? '查看' : '了解'
  return {
    priority,
    title: alert.message,
    targetRoute,
    actionLabel,
  }
}

/**
 * Pick the top N alerts by priority.  The full list is still shown in the
 * "当前问题" panel below; this function only feeds the "now-what" header.
 */
export function topAlerts(alerts: readonly CurrentAlert[], limit: number): CurrentAlert[] {
  return [...alerts]
    .map((alert, index) => ({ alert, originalIndex: index, presentation: presentAlert(alert) }))
    .sort((a, b) => {
      if (a.presentation.priority !== b.presentation.priority) {
        return a.presentation.priority - b.presentation.priority
      }
      // Stable order: keep original position so the same alert doesn't
      // bounce around between refreshes.
      return a.originalIndex - b.originalIndex
    })
    .slice(0, limit)
    .map((entry) => entry.alert)
}
