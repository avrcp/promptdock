// This file is generated from contracts/admin-api/v2/openapi.json.
export const ADMIN_API_MAJOR = 2 as const
export const ADMIN_API_BASE_PATH = '/admin/api/v2' as const
export const ADMIN_API_ROUTES = Object.freeze({
  "deliveries": "/deliveries",
  "devices": "/devices",
  "deviceDetail": "/devices/{device_id}",
  "deviceDisable": "/devices/{device_id}/disable",
  "deviceEnable": "/devices/{device_id}/enable",
  "deviceRevoke": "/devices/{device_id}/revoke",
  "deviceRotate": "/devices/{device_id}/rotate",
  "inboundCommands": "/inbound-commands",
  "interactiveReplies": "/interactive-replies",
  "retention": "/maintenance/retention",
  "meta": "/meta",
  "overview": "/overview",
  "results": "/results",
  "resultDetail": "/results/{result_row_id}",
  "resultRevoke": "/results/{result_row_id}/revoke",
  "system": "/system",
  "wechatDisconnect": "/wechat/disconnect",
  "wechatEvents": "/wechat/events",
  "wechatLogin": "/wechat/login",
  "wechatLoginSession": "/wechat/login/{login_id}",
  "wechatLoginVerify": "/wechat/login/{login_id}/verify",
  "wechatStatus": "/wechat/status",
  "wechatTest": "/wechat/test"
})
export const ADMIN_API_ROUTE_INVENTORY = Object.freeze([
  {
    "methods": [
      "GET",
      "HEAD"
    ],
    "path": "/meta"
  },
  {
    "methods": [
      "GET",
      "HEAD"
    ],
    "path": "/overview"
  },
  {
    "methods": [
      "GET",
      "HEAD"
    ],
    "path": "/devices"
  },
  {
    "methods": [
      "POST"
    ],
    "path": "/devices"
  },
  {
    "methods": [
      "GET",
      "HEAD"
    ],
    "path": "/devices/{device_id}"
  },
  {
    "methods": [
      "POST"
    ],
    "path": "/devices/{device_id}/rotate"
  },
  {
    "methods": [
      "POST"
    ],
    "path": "/devices/{device_id}/enable"
  },
  {
    "methods": [
      "POST"
    ],
    "path": "/devices/{device_id}/disable"
  },
  {
    "methods": [
      "POST"
    ],
    "path": "/devices/{device_id}/revoke"
  },
  {
    "methods": [
      "GET",
      "HEAD"
    ],
    "path": "/wechat/status"
  },
  {
    "methods": [
      "GET",
      "HEAD"
    ],
    "path": "/wechat/events"
  },
  {
    "methods": [
      "POST"
    ],
    "path": "/wechat/test"
  },
  {
    "methods": [
      "POST"
    ],
    "path": "/wechat/disconnect"
  },
  {
    "methods": [
      "POST"
    ],
    "path": "/wechat/login"
  },
  {
    "methods": [
      "GET",
      "HEAD"
    ],
    "path": "/wechat/login/{login_id}"
  },
  {
    "methods": [
      "DELETE"
    ],
    "path": "/wechat/login/{login_id}"
  },
  {
    "methods": [
      "POST"
    ],
    "path": "/wechat/login/{login_id}/verify"
  },
  {
    "methods": [
      "GET",
      "HEAD"
    ],
    "path": "/deliveries"
  },
  {
    "methods": [
      "GET",
      "HEAD"
    ],
    "path": "/interactive-replies"
  },
  {
    "methods": [
      "GET",
      "HEAD"
    ],
    "path": "/inbound-commands"
  },
  {
    "methods": [
      "GET",
      "HEAD"
    ],
    "path": "/results"
  },
  {
    "methods": [
      "GET",
      "HEAD"
    ],
    "path": "/results/{result_row_id}"
  },
  {
    "methods": [
      "POST"
    ],
    "path": "/results/{result_row_id}/revoke"
  },
  {
    "methods": [
      "GET",
      "HEAD"
    ],
    "path": "/system"
  },
  {
    "methods": [
      "POST"
    ],
    "path": "/maintenance/retention"
  }
])
