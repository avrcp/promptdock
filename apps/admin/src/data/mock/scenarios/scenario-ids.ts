export const SCENARIO_IDS = [
  'healthy',
  'wechat-disconnected',
  'wechat-login',
  'wechat-needs-activation',
  'gateway-partial-offline',
  'queue-backlog',
  'dead-letter',
  'database-degraded',
  'relay-unavailable',
  'empty-first-run',
  'slow-network',
  'partial-failure',
] as const

export type ScenarioId = (typeof SCENARIO_IDS)[number]

export interface ScenarioDescriptor {
  id: ScenarioId
  label: string
  description: string
}

export const SCENARIO_DESCRIPTORS: readonly ScenarioDescriptor[] = [
  {
    id: 'healthy',
    label: '正常运行',
    description: 'Relay 正常、微信就绪、队列无堆积',
  },
  {
    id: 'wechat-disconnected',
    label: '微信未连接',
    description: 'WeChat 通道未登录，通知会保留在队列中',
  },
  {
    id: 'wechat-login',
    label: '微信等待登录授权',
    description: '展示通道断开时受控登录入口的前置状态',
  },
  {
    id: 'wechat-needs-activation',
    label: '微信需要重新连接',
    description: '凭据需要通过受控登录流程重新连接',
  },
  {
    id: 'gateway-partial-offline',
    label: 'Gateway 部分离线',
    description: '部分设备 Gateway 连接断开',
  },
  {
    id: 'queue-backlog',
    label: '队列积压',
    description: '通知投递出现明显积压',
  },
  {
    id: 'dead-letter',
    label: '死信条目',
    description: '存在 dead_letter 状态的入站命令',
  },
  {
    id: 'database-degraded',
    label: '数据库降级',
    description: '数据库健康度下降但仍可读',
  },
  {
    id: 'relay-unavailable',
    label: 'Relay 不可用',
    description: 'Overview 顶层获取失败（section-level fallback）',
  },
  {
    id: 'empty-first-run',
    label: '空状态首次运行',
    description: '无设备、无队列、无问题',
  },
  {
    id: 'slow-network',
    label: '慢网络',
    description: '所有读取走 1.5–2.5s 慢延迟',
  },
  {
    id: 'partial-failure',
    label: '部分失败',
    description: 'Overview 多个 section 同时出现 section-level 错误',
  },
]

export function isScenarioId(value: string): value is ScenarioId {
  return (SCENARIO_IDS as readonly string[]).includes(value)
}
