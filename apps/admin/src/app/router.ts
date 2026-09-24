import { createRouter, createWebHistory, type RouteRecordRaw } from 'vue-router'

const OverviewPage = () => import('@/features/overview/OverviewPage.vue')
const DevicesPage = () => import('@/features/devices/DevicesPage.vue')
const WechatPage = () => import('@/features/wechat/WechatPage.vue')
const QueuePage = () => import('@/features/queue/QueuePage.vue')
const ResultsPage = () => import('@/features/results/ResultsPage.vue')
const SystemPage = () => import('@/features/system/SystemPage.vue')
const NotFoundPage = () => import('@/app/NotFoundPage.vue')

const routes: RouteRecordRaw[] = [
  {
    path: '/',
    redirect: '/overview',
  },
  {
    path: '/overview',
    name: 'overview',
    component: OverviewPage,
    meta: { title: '总览' },
  },
  {
    path: '/devices',
    name: 'devices',
    component: DevicesPage,
    meta: { title: '设备' },
  },
  {
    path: '/wechat',
    name: 'wechat',
    component: WechatPage,
    meta: { title: '微信通道' },
  },
  {
    path: '/queue',
    name: 'queue',
    component: QueuePage,
    meta: { title: '队列' },
  },
  {
    path: '/results',
    name: 'results',
    component: ResultsPage,
    meta: { title: '结果页' },
  },
  {
    path: '/system',
    name: 'system',
    component: SystemPage,
    meta: { title: '系统' },
  },
  {
    path: '/:pathMatch(.*)*',
    name: 'not-found',
    component: NotFoundPage,
    meta: { title: '未找到' },
  },
]

export const router = createRouter({
  history: createWebHistory(),
  routes,
  scrollBehavior: () => ({ left: 0, top: 0 }),
})

router.afterEach((to) => {
  const titleMeta = to.meta['title']
  const title = typeof titleMeta === 'string' ? titleMeta : '总览'
  document.title = `${title} · PromptDock Relay Admin`
})
