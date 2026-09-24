import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import {
  adminMetaSchema,
  adminOverviewWireSchema,
  deliveriesPageWireSchema,
  deviceActionWireSchema,
  deviceCredentialWireSchema,
  deviceDetailWireSchema,
  devicesPageWireSchema,
  inboundCommandsPageWireSchema,
  interactiveRepliesPageWireSchema,
  retentionWireSchema,
  resultDetailWireSchema,
  resultReceiptWireSchema,
  resultsPageWireSchema,
  systemWireSchema,
  wechatDisconnectWireSchema,
  wechatEventsPageWireSchema,
  wechatLoginWireSchema,
  wechatStatusWireSchema,
  wechatTestWireSchema,
  wireErrorSchema,
} from "./index";

const fixtureRoot = fileURLToPath(
  new URL("../../../contracts/admin-api/v2/fixtures/", import.meta.url),
);

const fixtureSchemas = {
  "deliveries-page-v2.json": deliveriesPageWireSchema,
  "device-action-receipt-v2.json": deviceActionWireSchema,
  "device-create-receipt-v2.json": deviceCredentialWireSchema,
  "device-detail-v2.json": deviceDetailWireSchema,
  "device-rotate-receipt-v2.json": deviceCredentialWireSchema,
  "devices-page-v2.json": devicesPageWireSchema,
  "error-v2.json": wireErrorSchema,
  "inbound-commands-page-v2.json": inboundCommandsPageWireSchema,
  "interactive-replies-page-v2.json": interactiveRepliesPageWireSchema,
  "meta-v2.json": adminMetaSchema,
  "overview-v2.json": adminOverviewWireSchema,
  "retention-action-receipt-v2.json": retentionWireSchema,
  "result-detail-v2.json": resultDetailWireSchema,
  "result-receipt-v2.json": resultReceiptWireSchema,
  "results-page-v2.json": resultsPageWireSchema,
  "system-v2.json": systemWireSchema,
  "wechat-action-receipt-v2.json": wechatDisconnectWireSchema,
  "wechat-events-page-v2.json": wechatEventsPageWireSchema,
  "wechat-login-cancelled-v2.json": wechatLoginWireSchema,
  "wechat-login-verify-required-v2.json": wechatLoginWireSchema,
  "wechat-login-waiting-scan-v2.json": wechatLoginWireSchema,
  "wechat-status-v2.json": wechatStatusWireSchema,
  "wechat-test-receipt-v2.json": wechatTestWireSchema,
} as const;

describe("canonical Relay Admin API v2 fixtures", () => {
  for (const [filename, schema] of Object.entries(fixtureSchemas)) {
    it(`strictly decodes ${filename}`, () => {
      const value = JSON.parse(
        readFileSync(`${fixtureRoot}/${filename}`, "utf8"),
      );
      expect(schema.parse(value)).toEqual(value);
    });
  }

  it("rejects unknown response fields", () => {
    const value = JSON.parse(
      readFileSync(`${fixtureRoot}/meta-v2.json`, "utf8"),
    );
    expect(
      adminMetaSchema.safeParse({ ...value, unexpected: true }).success,
    ).toBe(false);
  });
});
