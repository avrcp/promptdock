import type { ActionReceipt, MaintenanceReceipt } from '@/contracts/common'
import type { RequestOptions } from '@/contracts/common'
import type {
  CreateDeviceInput,
  DeviceCredentialReceipt,
  RevokeDeviceInput,
  RotateDeviceInput,
  SetDeviceEnabledInput,
} from '@/contracts/device'
import type {
  DisconnectWechatInput,
  StartWechatLoginInput,
  VerifyWechatLoginInput,
  WechatLoginSession,
  WechatTestReceipt,
} from '@/contracts/wechat'
import type { ResultReceipt, ResultRevokeInput } from '@/contracts/result'

export interface AdminCommandRepository {
  createDevice(input: CreateDeviceInput, options?: RequestOptions): Promise<DeviceCredentialReceipt>
  rotateDevice(input: RotateDeviceInput, options?: RequestOptions): Promise<DeviceCredentialReceipt>
  setDeviceEnabled(input: SetDeviceEnabledInput, options?: RequestOptions): Promise<ActionReceipt>
  revokeDevice(input: RevokeDeviceInput, options?: RequestOptions): Promise<ActionReceipt>
  revokeResult(input: ResultRevokeInput, options?: RequestOptions): Promise<ResultReceipt>

  startWechatLogin(
    input: StartWechatLoginInput,
    options?: RequestOptions,
  ): Promise<WechatLoginSession>
  getWechatLogin(loginId: string, options?: RequestOptions): Promise<WechatLoginSession>
  verifyWechatLogin(
    input: VerifyWechatLoginInput,
    options?: RequestOptions,
  ): Promise<WechatLoginSession>
  cancelWechatLogin(loginId: string, options?: RequestOptions): Promise<WechatLoginSession>
  disconnectWechat(input: DisconnectWechatInput, options?: RequestOptions): Promise<ActionReceipt>
  sendWechatTest(options?: RequestOptions): Promise<WechatTestReceipt>

  runRetention(options?: RequestOptions): Promise<MaintenanceReceipt>
}
