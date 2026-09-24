/**
 * 设计走查专用 Tauri mock（仅 vite.mock.config.ts 引用，不进入产品构建）。
 *
 * 用固定的、贴近真实的数据填充全部视图，供纯浏览器渲染与截图走查。
 * 数据边界遵循 docs/ui-design-decision.md：不虚构后端不存在的能力。
 */

const now = Date.now()
const minute = 60_000

const status = {
  dataDirectory: 'C:\\Users\\example\\.promptdock-desktop',
  policy: {
    observe_turns: true,
    notify_started: true,
    notify_ended: true,
    completion_quiet_ms: 2000,
    result_content_mode: 'status_only',
    notify_attention: false,
    include_task_input: false,
  },
  policyRevision: 1,
  policyApplyStatus: 'saved',
  hookHome: 'C:\\Users\\example\\.codex',
  inboxPresent: true,
}

const launcher = {
  config: {
    proxy: { enabled: true, host: '127.0.0.1', port: 7890, noProxy: [] },
    desktop: {
      selectedExecutable: 'C:\\Users\\example\\AppData\\Local\\Programs\\ChatGPT\\ChatGPT.exe',
      refuseIfRunning: true,
    },
  },
  candidates: [
    {
      executable: 'C:\\Users\\example\\AppData\\Local\\Programs\\ChatGPT\\ChatGPT.exe',
      productLabel: 'ChatGPT 桌面端',
      packageVersion: '1.2026.87',
    },
  ],
  selectedRunning: false,
  discoveryIssue: null,
  selectionIssue: null,
}

const relay = {
  runtimeEpoch: 'design-preview',
  revision: 1,
  state: 'ready',
  canSubmit: true,
  canReadOwn: true,
  baseUrl: 'https://relay.example.internal',
  configured: true,
  reachable: true,
  authenticated: true,
  lastErrorCode: null,
}

const deliveries = [
  {
    id: 'notification-000042',
    status: 'delivered',
    remoteStatus: 'provider_accepted',
    createdAt: Math.floor((now - 3 * minute) / 1000),
    payload: { title: '重构会话已结束：桌面与通知链路整改' },
    result: {
      resultId: 'notification-000042',
      sourceHash: 'a'.repeat(64),
      pageState: 'available',
      pageExpiresAt: now + 7 * 86400000,
      notificationId: 'notice-42',
      notificationStatus: 'provider_accepted',
    },
  },
  {
    id: 'notification-000041',
    status: 'delivered',
    remoteStatus: 'pending_channel',
    createdAt: Math.floor((now - 18 * minute) / 1000),
    payload: { title: '等待微信接口确认的测试通知' },
  },
  {
    id: 'notification-000040',
    status: 'delivered',
    remoteStatus: 'dead_letter',
    createdAt: Math.floor((now - 42 * minute) / 1000),
    payload: { title: '发送失败：设备长时间未连接微信，需要人工处理' },
  },
  {
    id: 'notification-000039',
    status: 'delivered',
    remoteStatus: 'delivered',
    createdAt: Math.floor((now - 65 * minute) / 1000),
    payload: { title: '上午的会话轮次开始' },
  },
  {
    id: 'notification-000038',
    status: 'delivered',
    remoteStatus: 'provider_accepted',
    createdAt: Math.floor((now - 2 * 60 * minute) / 1000),
    payload: { title: '架构评审会话结束' },
  },
]

const activityItems = [
  {
    runKey: 'run-preview-attention',
    workspaceLabel: '当前工作区',
    displayTitle: '需要处理的权限请求',
    activityRevision: 4,
    phase: 'settling',
    startedAt: now - 5 * minute,
    lastObservedAt: now - 2 * minute,
    attention: {
      revision: 4,
      acknowledgedRevision: 3,
      label: '等待你的处理',
      historical: false,
      observationExpiresAt: now + 28 * minute,
    },
    result: null,
    delivery: null,
  },
  {
    runKey: 'run-preview-result',
    workspaceLabel: '当前工作区',
    displayTitle: '会话结果已准备',
    activityRevision: 7,
    phase: 'ended_observed',
    startedAt: now - 18 * minute,
    lastObservedAt: now - 8 * minute,
    attention: null,
    result: {
      outboxId: 'notification-000042',
      resultRevision: 7,
      pageState: 'available',
      expiresAt: now + 7 * 86400000,
      seenResultRevision: 6,
    },
    delivery: { state: 'delivered', lastErrorCode: null, heldUntil: null },
  },
]
const activityCounts = { attention: 1, started: 0, results: 1, deliveryIssues: 0, recent: 2 }

const autostart = true
let holdStatus = {
  revision: 1,
  startedAt: null as number | null,
  until: null as number | null,
  requestedDuration: null as number | null,
  state: 'inactive' as 'active' | 'inactive',
}

const hookPlan = {
  registrationId: 'b0b607e3-6f31-4e71-b866-6e3d1bd49c72',
  operationId: '9ca17b70-c4f2-4038-a700-220fc662072c',
  sourceFingerprint: 'sha256:' + 'a'.repeat(64),
  plannedFingerprint: 'sha256:' + 'a'.repeat(64),
  expectedPolicy: status.policy,
  changedEvents: [],
  definitionChanged: false,
  registrationChanged: false,
  outcome: 'no_change',
  reviewRequired: false,
  externalReviewRequired: false,
}

const handlers: Record<string, (args?: Record<string, unknown>) => unknown> = {
  desktop_hook_health: () => ({
    runtimeEpoch: 'design-preview',
    revision: 1,
    observedAt: now,
    fresh: true,
    observationEnabled: true,
    hookSourcePath: status.hookHome + '\\hooks.json',
    userStateSourcePath: status.hookHome + '\\config.toml',
    sourceResolution: 'candidate',
    compatibility: 'unverified',
    hostBuild: null,
    installation: 'current',
    registrationId: 'preview-registration',
    definitionFingerprint: 'sha256:' + 'a'.repeat(64),
    diagnosticCode: null,
    handlers: ['UserPromptSubmit', 'Stop', 'PermissionRequest'].map((event) => ({
      event,
      required: event !== 'PermissionRequest',
      configured: event !== 'PermissionRequest',
      trust: event === 'PermissionRequest' ? 'not_applicable' : 'no_matching_record',
      evidenceSource: 'persisted_user_config',
      lastErrorCode: null,
      lastObservedAt: null,
      observedRegistrationId: null,
      observationCurrent: false,
    })),
    verification: {
      state: 'not_started',
      verificationId: null,
      validatedAt: null,
      validatedDefinitionFingerprint: null,
      hostAttribution: 'unknown',
      instruction: null,
    },
  }),
  desktop_delivery_metadata: () => deliveries[0],
  desktop_hook_plan: () => hookPlan,
  desktop_install_hook: () => hookPlan,
  desktop_status: () => status,
  desktop_launcher_status: () => launcher,
  desktop_launcher_save: (args) => args?.config,
  desktop_save_policy: (args) => ({
    status: 'saved',
    state: {
      policy: args?.policy,
      captureGeneration: 1,
      revision: status.policyRevision + 1,
    },
  }),
  desktop_relay_status: () => relay,
  desktop_deliveries: () => ({ items: deliveries, nextCursor: null }),
  desktop_activity_page: (args) => {
    const filter = args?.filter
    const items =
      filter === 'attention'
        ? activityItems.filter((item) => item.attention)
        : filter === 'results'
          ? activityItems.filter((item) => item.result)
          : activityItems
    return { items, nextCursor: null, counts: activityCounts }
  },
  desktop_activity_detail: (args) => {
    const item =
      activityItems.find((candidate) => candidate.runKey === args?.runKey) ?? activityItems[0]
    return {
      item,
      events: [
        {
          id: `${item.runKey}-observed`,
          kind: item.phase,
          occurredAt: item.lastObservedAt,
          observedAt: item.lastObservedAt,
        },
      ],
      deliveries:
        item.result === null
          ? []
          : [
              {
                id: 'notification-000042',
                kind: 'result_page',
                state: 'delivered',
                pageState: 'available',
                resultRevision: item.result.resultRevision,
              },
            ],
    }
  },
  desktop_attention_ack: () => ({ acknowledgedRevision: 4, currentRevision: 4 }),
  desktop_result_mark_seen: () => ({ seenResultRevision: 7, currentRevision: 7 }),
  desktop_delivery_detail: () => ({
    id: 'notification-000042',
    title: '最终回答（设计预览）',
    body: '完整最终回答\r\n\r\n```rust\r\n  let message = "原文保留";\r\n```',
    contentMode: 'full_final',
    contentBytes: 80,
    sourceHash: null,
    unavailableReason: null,
  }),
  desktop_autostart_status: () => autostart,
  desktop_notification_hold_status: () => holdStatus,
  desktop_notification_hold_set: (args) => {
    holdStatus = {
      revision: holdStatus.revision + 1,
      startedAt: now,
      until: now + Number(args?.minutes ?? 15) * 60_000,
      requestedDuration: Number(args?.minutes ?? 15) * 60_000,
      state: 'active',
    }
    return { status: 'applied', state: holdStatus }
  },
  desktop_notification_hold_resume: () => {
    holdStatus = {
      revision: holdStatus.revision + 1,
      startedAt: null,
      until: null,
      requestedDuration: null,
      state: 'inactive',
    }
    return { status: 'applied', state: holdStatus }
  },
  desktop_health_check: () => ({
    steps: [
      {
        id: 'hook',
        label: 'Hook',
        status: 'not_run',
        code: null,
        detail: '尚未执行实际桌面验证。',
        nextAction: 'open_integration',
      },
    ],
    report: { build: 'design-preview', diagnostics: 'safe' },
    test: null,
  }),
  desktop_health_export: () => ({
    path: 'C:\\Users\\example\\.promptdock-desktop\\health-report.json',
  }),
  desktop_health_test_start: () => ({
    probeId: 'preview-probe',
    outboxId: 'notification-000042',
    status: 'submitted',
    remoteStatus: null,
    lastErrorCode: null,
  }),
  desktop_health_test_status: () => null,
  desktop_diagnostics: () => ({
    schemaVersion: 1,
    hookConfigured: true,
    relayConfigured: relay.configured,
    relayReady: true,
    pendingNotifications: 0,
    sendingNotifications: 0,
    retryNotifications: 0,
    blockedNotifications: 0,
    deadLetterNotifications: 0,
    lastDeliveryAt: deliveries[0]?.createdAt ?? null,
    lastErrorCode: null,
    retention: {
      eventCount: 24,
      outputCount: 8,
      outboxCount: 5,
      contentBytes: 42_128,
      maxRecords: 50_000,
      maxContentBytes: 67_108_864,
    },
    suppressedUnknownEvents: 1,
    verificationEvents: 2,
    lastUnknownObservedAt: Math.floor((now - 9 * minute) / 1000),
    lastVerificationObservedAt: Math.floor((now - 4 * minute) / 1000),
  }),
}

export async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  const handler = handlers[command]
  if (handler) return handler(args) as T
  // 动作类命令（保存 / 安装 / 卸载 / 启动 / 测试通知）一律视为成功
  return null as T
}
