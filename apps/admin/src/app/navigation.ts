import type { Component } from 'vue'
import {
  LayoutDashboard,
  Smartphone,
  MessageSquare,
  ListOrdered,
  FileOutput,
  ServerCog,
} from 'lucide-vue-next'

export interface NavigationItem {
  path: string
  label: string
  description: string
  icon: Component
}

export const navigationItems: NavigationItem[] = [
  {
    path: '/overview',
    label: '总览',
    description: 'Relay 当前运行状态与最近问题',
    icon: LayoutDashboard,
  },
  {
    path: '/devices',
    label: '设备',
    description: '已接入设备、凭证与 Gateway 连接',
    icon: Smartphone,
  },
  {
    path: '/wechat',
    label: '微信通道',
    description: '微信通道状态、扫码登录与测试',
    icon: MessageSquare,
  },
  {
    path: '/queue',
    label: '队列',
    description: '通知投递、交互回复与入站命令',
    icon: ListOrdered,
  },
  {
    path: '/results',
    label: '结果页',
    description: '完整结果的安全状态与撤销操作',
    icon: FileOutput,
  },
  {
    path: '/system',
    label: '系统',
    description: '构建、数据库、Workers 与运维摘要',
    icon: ServerCog,
  },
]
