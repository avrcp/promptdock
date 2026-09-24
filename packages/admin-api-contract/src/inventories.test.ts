import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import {
  ADMIN_API_BASE_PATH,
  ADMIN_API_ROUTE_INVENTORY,
  ADMIN_CAPABILITY_VALUES,
  ADMIN_DEVICE_STATE_VALUES,
  ADMIN_GATEWAY_STATE_VALUES,
  ADMIN_MODE_VALUES,
  ALERT_COMPONENT_VALUES,
  ALERT_SEVERITY_VALUES,
  CHANNEL_EVENT_KIND_VALUES,
  COMPONENT_HEALTH_STATE_VALUES,
  DATABASE_INTEGRITY_STATE_VALUES,
  DATABASE_SIZE_BUCKET_VALUES,
  DELIVERY_KIND_VALUES,
  DELIVERY_ORIGIN_VALUES,
  DELIVERY_STATE_VALUES,
  DEVICE_ACTION_VALUES,
  DEVICE_CREDENTIAL_ACTION_VALUES,
  INBOUND_COMMAND_VALUES,
  INBOUND_STATE_VALUES,
  LOGIN_STATE_VALUES,
  MAINTENANCE_ACTION_VALUES,
  PUBLIC_BIND_CLASS_VALUES,
  RELAY_HEALTH_STATE_VALUES,
  RETENTION_RESULT_VALUES,
  SAFE_WORKER_STATE_VALUES,
  WAL_STATE_VALUES,
  WECHAT_ACTION_VALUES,
  WECHAT_STATE_VALUES,
  WECHAT_TEST_STATE_VALUES,
} from "./index";

const inventoryRoot = fileURLToPath(
  new URL("../../../contracts/admin-api/v2/inventories/", import.meta.url),
);

function inventory(filename: string): Record<string, unknown> {
  return JSON.parse(
    readFileSync(`${inventoryRoot}/${filename}`, "utf8"),
  ) as Record<string, unknown>;
}

describe("closed Admin API v2 inventories", () => {
  it("matches routes and capabilities exactly", () => {
    const routes = inventory("routes-v2.json");
    expect(routes["basePath"]).toBe(ADMIN_API_BASE_PATH);
    expect(routes["routes"]).toEqual(ADMIN_API_ROUTE_INVENTORY);

    const capabilities = inventory("capabilities-v2.json");
    expect(capabilities["capabilities"]).toEqual(ADMIN_CAPABILITY_VALUES);
  });

  it("matches actions exactly and rejects the legacy list_runs action", () => {
    const actions = inventory("actions-v2.json");
    expect(actions).toMatchObject({
      deviceCredentialActions: DEVICE_CREDENTIAL_ACTION_VALUES,
      deviceActions: DEVICE_ACTION_VALUES,
      inboundCommands: INBOUND_COMMAND_VALUES,
      maintenanceActions: MAINTENANCE_ACTION_VALUES,
      wechatActions: WECHAT_ACTION_VALUES,
    });
    expect(INBOUND_COMMAND_VALUES).toContain("list_jobs");
    expect(INBOUND_COMMAND_VALUES).not.toContain("list_runs");
  });

  it("matches every closed state inventory exactly", () => {
    expect(inventory("states-v2.json")).toMatchObject({
      adminDeviceStates: ADMIN_DEVICE_STATE_VALUES,
      adminGatewayStates: ADMIN_GATEWAY_STATE_VALUES,
      adminModes: ADMIN_MODE_VALUES,
      alertComponents: ALERT_COMPONENT_VALUES,
      alertSeverities: ALERT_SEVERITY_VALUES,
      channelEventKinds: CHANNEL_EVENT_KIND_VALUES,
      componentHealthStates: COMPONENT_HEALTH_STATE_VALUES,
      databaseIntegrityStates: DATABASE_INTEGRITY_STATE_VALUES,
      databaseSizeBuckets: DATABASE_SIZE_BUCKET_VALUES,
      deliveryKinds: DELIVERY_KIND_VALUES,
      deliveryOrigins: DELIVERY_ORIGIN_VALUES,
      deliveryStates: DELIVERY_STATE_VALUES,
      inboundStates: INBOUND_STATE_VALUES,
      loginStates: LOGIN_STATE_VALUES,
      publicBindClasses: PUBLIC_BIND_CLASS_VALUES,
      relayHealthStates: RELAY_HEALTH_STATE_VALUES,
      retentionResults: RETENTION_RESULT_VALUES,
      safeWorkerStates: SAFE_WORKER_STATE_VALUES,
      walStates: WAL_STATE_VALUES,
      wechatStates: WECHAT_STATE_VALUES,
      wechatTestStates: WECHAT_TEST_STATE_VALUES,
    });
  });
});
