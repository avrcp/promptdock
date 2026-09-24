import { createHash } from "node:crypto";
import { readFile, readdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const contractRoot = fileURLToPath(
  new URL("../../contracts/admin-api/v2/", import.meta.url),
);
const fixtureRoot = join(contractRoot, "fixtures");
const manifestPath = join(contractRoot, "manifest.json");

const paths = (await readdir(fixtureRoot, { withFileTypes: true }))
  .filter(
    (entry) => entry.isFile() && /^[a-z0-9][a-z0-9-]*\.json$/.test(entry.name),
  )
  .map((entry) => entry.name)
  .sort();

if (paths.length === 0)
  throw new Error("Refusing to write an empty Admin API manifest.");

const fixtures = [];
for (const path of paths) {
  const bytes = await readFile(join(fixtureRoot, path));
  JSON.parse(bytes.toString("utf8"));
  fixtures.push({
    path,
    sha256: createHash("sha256").update(bytes).digest("hex"),
    mediaType: "application/json",
    schemaVersion: 2,
  });
}

const manifest = {
  manifestVersion: 1,
  contract: "promptdock-relay-admin-api-v2",
  fixtures,
};
await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, "utf8");
console.log(
  `Updated Admin API v2 manifest for ${fixtures.length} generated fixtures.`,
);
