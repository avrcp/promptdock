import { createHash } from "node:crypto";
import { readFile, readdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { join } from "node:path";

const contractRoot = fileURLToPath(
  new URL("../../contracts/admin-api/v2/", import.meta.url),
);
const fixtureRoot = join(contractRoot, "fixtures");
const inventoryRoot = join(contractRoot, "inventories");

const manifest = await readJson(
  join(contractRoot, "manifest.json"),
  "manifest",
);
assertExactKeys(
  manifest,
  ["contract", "fixtures", "manifestVersion"],
  "manifest",
);
assert(manifest.manifestVersion === 1, "manifestVersion must be 1");
assert(
  manifest.contract === "promptdock-relay-admin-api-v2",
  "contract identity drifted",
);
assert(
  Array.isArray(manifest.fixtures) && manifest.fixtures.length > 0,
  "fixture inventory is empty",
);

const manifestPaths = [];
let previousPath = "";
for (const entry of manifest.fixtures) {
  assertExactKeys(
    entry,
    ["mediaType", "path", "schemaVersion", "sha256"],
    "manifest fixture",
  );
  assert(
    typeof entry.path === "string" &&
      /^[a-z0-9][a-z0-9-]*\.json$/.test(entry.path),
    "invalid fixture path",
  );
  assert(
    entry.path > previousPath,
    `fixture inventory must be sorted: ${entry.path}`,
  );
  assert(
    !manifestPaths.includes(entry.path),
    `duplicate fixture: ${entry.path}`,
  );
  assert(
    entry.mediaType === "application/json",
    `invalid media type: ${entry.path}`,
  );
  assert(entry.schemaVersion === 2, `invalid schema version: ${entry.path}`);
  assert(
    typeof entry.sha256 === "string" && /^[0-9a-f]{64}$/.test(entry.sha256),
    `invalid SHA-256: ${entry.path}`,
  );
  const bytes = await readFile(join(fixtureRoot, entry.path));
  assert(sha256(bytes) === entry.sha256, `fixture hash drift: ${entry.path}`);
  JSON.parse(bytes.toString("utf8"));
  manifestPaths.push(entry.path);
  previousPath = entry.path;
}

const actualPaths = (await readdir(fixtureRoot, { withFileTypes: true }))
  .filter((entry) => entry.isFile())
  .map((entry) => entry.name)
  .sort();
assertEqual(
  actualPaths,
  manifestPaths,
  "fixture directory differs from manifest",
);

const routes = await readInventory("routes-v2.json", [
  "basePath",
  "routes",
  "schemaVersion",
]);
assert(routes.basePath === "/admin/api/v2", "Admin API base path drifted");
assert(
  Array.isArray(routes.routes) && routes.routes.length > 0,
  "route inventory is empty",
);
const routeKeys = new Set();
for (const route of routes.routes) {
  assertExactKeys(route, ["methods", "path"], "route");
  assert(
    Array.isArray(route.methods) && route.methods.length > 0,
    `route methods missing: ${route.path}`,
  );
  assert(
    typeof route.path === "string" && route.path.startsWith("/"),
    "invalid route path",
  );
  for (const method of route.methods)
    assert(
      ["DELETE", "GET", "HEAD", "POST"].includes(method),
      `invalid route method: ${method}`,
    );
  const key = `${route.methods.join(",")} ${route.path}`;
  assert(!routeKeys.has(key), `duplicate route: ${key}`);
  routeKeys.add(key);
}

const capabilities = await readInventory("capabilities-v2.json", [
  "capabilities",
  "schemaVersion",
]);
assertStringInventory(capabilities.capabilities, "capabilities");
assert(
  capabilities.capabilities.includes("admin_read_v2"),
  "admin_read_v2 capability is required",
);

const actions = await readInventory("actions-v2.json", [
  "deviceActions",
  "deviceCredentialActions",
  "inboundCommands",
  "maintenanceActions",
  "schemaVersion",
  "wechatActions",
]);
for (const key of [
  "deviceActions",
  "deviceCredentialActions",
  "inboundCommands",
  "maintenanceActions",
  "wechatActions",
]) {
  assertStringInventory(actions[key], key);
}
assert(
  actions.inboundCommands.includes("list_jobs"),
  "list_jobs must remain in Admin API v2",
);
assert(
  !actions.inboundCommands.includes("list_runs"),
  "legacy list_runs must remain rejected",
);

const inboundActions = await readJson(
  join(fixtureRoot, "inbound-command-actions-v2.json"),
  "inbound command actions fixture",
);
assertExactKeys(
  inboundActions,
  ["actions", "schemaVersion"],
  "inbound command actions fixture",
);
assertEqual(
  inboundActions.actions,
  actions.inboundCommands,
  "inbound command action inventory drifted",
);

const states = await readInventory("states-v2.json", [
  "adminDeviceStates",
  "adminGatewayStates",
  "adminModes",
  "alertComponents",
  "alertSeverities",
  "channelEventKinds",
  "componentHealthStates",
  "databaseIntegrityStates",
  "databaseSizeBuckets",
  "deliveryKinds",
  "deliveryOrigins",
  "deliveryStates",
  "inboundStates",
  "loginStates",
  "publicBindClasses",
  "relayHealthStates",
  "retentionResults",
  "safeWorkerStates",
  "schemaVersion",
  "walStates",
  "wechatStates",
  "wechatTestStates",
]);
for (const [key, value] of Object.entries(states)) {
  if (key !== "schemaVersion") assertStringInventory(value, key);
}

const meta = await readJson(join(fixtureRoot, "meta-v2.json"), "meta fixture");
for (const capability of meta.capabilities) {
  assert(
    capabilities.capabilities.includes(capability),
    `meta fixture has unknown capability: ${capability}`,
  );
}

console.log(
  `Admin API contract check passed: ${manifest.fixtures.length} fixtures, ${routes.routes.length} routes, closed inventories verified.`,
);

async function readInventory(filename, keys) {
  const value = await readJson(join(inventoryRoot, filename), filename);
  assertExactKeys(value, keys, filename);
  assert(value.schemaVersion === 2, `${filename} schemaVersion must be 2`);
  return value;
}

async function readJson(path, label) {
  try {
    return JSON.parse(await readFile(path, "utf8"));
  } catch (error) {
    throw new Error(
      `Admin API contract check failed: cannot read ${label}: ${safeMessage(error)}`,
    );
  }
}

function assertStringInventory(value, label) {
  assert(
    Array.isArray(value) && value.length > 0,
    `${label} must be a non-empty array`,
  );
  assert(
    value.every((item) => typeof item === "string" && item.length > 0),
    `${label} must contain strings`,
  );
  assert(new Set(value).size === value.length, `${label} contains duplicates`);
}

function assertExactKeys(value, expected, label) {
  assert(
    value && typeof value === "object" && !Array.isArray(value),
    `${label} must be an object`,
  );
  assertEqual(
    Object.keys(value).sort(),
    [...expected].sort(),
    `${label} has unknown or missing fields`,
  );
}

function assertEqual(actual, expected, message) {
  assert(JSON.stringify(actual) === JSON.stringify(expected), message);
}

function assert(condition, message) {
  if (!condition)
    throw new Error(`Admin API contract check failed: ${message}`);
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function safeMessage(error) {
  return error instanceof Error ? error.message : String(error);
}
