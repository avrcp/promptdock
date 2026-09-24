export type ViewKey = 'overview' | 'integration' | 'notifications' | 'deliveries' | 'diagnostics'

export const viewMeta: Readonly<Record<ViewKey, { title: string; description: string }>> = {
  overview: { title: '总览', description: 'ChatGPT 桌面应用中的 Codex 通知' },
  integration: { title: '桌面与 Hook', description: '选择实际桌面宿主，并连接 Codex 会话事件' },
  notifications: { title: 'Relay 与通知', description: '连接服务器并控制发送到微信的内容范围' },
  deliveries: { title: '投递记录', description: '查看本机提交及服务器接管状态' },
  diagnostics: { title: '后台与诊断', description: '管理开机自启并查看脱敏运行信息' },
}
