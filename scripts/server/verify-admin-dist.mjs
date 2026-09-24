import { createHash } from "node:crypto";
import { lstat, readFile, readdir, writeFile } from "node:fs/promises";
import { readFileSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { execFileSync } from "node:child_process";

const workspaceRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const defaultDist = join(workspaceRoot, "apps", "admin", "dist");
const artifactManifestName = "admin-artifact-manifest.json";
const forbiddenMarkers = [
  "dev-scenario-select",
  "promptdock-relay-admin:dev-scenario",
  "mock-pdv2.not-a-real-secret",
  "sourceMappingURL=",
  "__ADMIN_MOCK_BUILD__",
];

function fail(message) {
  throw new Error(message);
}

async function filesUnder(directory) {
  const files = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) files.push(...(await filesUnder(path)));
    else if (entry.isFile()) files.push(path);
    else fail(`Admin artifact contains an unsupported entry: ${path}`);
  }
  return files.sort();
}

function inside(parent, candidate) {
  const path = relative(parent, candidate);
  return path === "" || (path !== ".." && !path.startsWith(`..${sep}`));
}

function sha256(content) {
  return createHash("sha256").update(content).digest("hex");
}

function workspaceVersion() {
  const cargo = readFileSync(join(workspaceRoot, "Cargo.toml"), "utf8");
  const match = cargo.match(/^version\s*=\s*"([^"]+)"\s*$/m);
  if (!match) fail("Cargo workspace version could not be resolved.");
  return process.env.VITE_BUILD_VERSION ?? match[1];
}

function sourceCommit() {
  return (
    process.env.VITE_BUILD_COMMIT ??
    execFileSync("git", ["rev-parse", "HEAD"], {
      cwd: workspaceRoot,
      encoding: "utf8",
    }).trim()
  );
}

export async function inspectAdminDist(dist = defaultDist) {
  const absoluteDist = resolve(dist);
  const rootEntry = await lstat(absoluteDist).catch(() => null);
  if (!rootEntry?.isDirectory() || rootEntry.isSymbolicLink()) {
    fail(`Admin dist must be a real directory: ${absoluteDist}`);
  }
  const indexPath = join(absoluteDist, "index.html");
  const indexEntry = await lstat(indexPath).catch(() => null);
  if (!indexEntry?.isFile() || indexEntry.isSymbolicLink())
    fail("Admin dist/index.html is required.");
  const index = await readFile(indexPath, "utf8");
  if (!/type="module"/.test(index) || !/assets\//.test(index)) {
    fail("Admin dist/index.html is not a Vite production entrypoint.");
  }

  const files = [];
  for (const path of await filesUnder(absoluteDist)) {
    if (!inside(absoluteDist, path))
      fail(`Admin artifact escapes dist: ${path}`);
    const name = relative(absoluteDist, path).replaceAll("\\", "/");
    if (name === artifactManifestName) continue;
    if (name.endsWith(".map")) fail(`Source maps are forbidden: ${name}`);
    const content = await readFile(path);
    const text = content.toString("utf8");
    for (const marker of forbiddenMarkers) {
      if (text.includes(marker))
        fail(`Forbidden marker ${JSON.stringify(marker)} in ${name}`);
    }
    if (/(?:fetch|XMLHttpRequest|WebSocket)\s*\([^)]*https?:\/\//i.test(text)) {
      fail(`Unexpected external request in ${name}`);
    }
    files.push({
      path: name,
      sha256: sha256(content),
      bytes: content.byteLength,
    });
  }
  return files;
}

async function assertEmbeddedBuildIdentity(dist, version, commit) {
  const javascript = (await filesUnder(dist)).filter((path) =>
    path.endsWith(".js"),
  );
  const bundle = (
    await Promise.all(
      javascript.map(async (path) => (await readFile(path)).toString("utf8")),
    )
  ).join("\n");
  if (!bundle.includes(version))
    fail("Admin bundle does not embed the unified release version.");
  if (!bundle.includes(commit))
    fail("Admin bundle does not embed the unified source commit.");
}

export async function writeAndVerifyArtifactManifest(dist = defaultDist) {
  const absoluteDist = resolve(dist);
  const version = workspaceVersion();
  const commit = sourceCommit();
  if (!/^[0-9a-f]{40}$/.test(commit) || /^0{40}$/.test(commit))
    fail("Admin source commit must be a non-zero 40-character Git SHA.");
  await assertEmbeddedBuildIdentity(absoluteDist, version, commit);
  const manifest = {
    schemaVersion: 1,
    buildVersion: version,
    sourceCommit: commit,
    adminApiMajor: Number(process.env.VITE_ADMIN_API_MAJOR ?? "2"),
    files: await inspectAdminDist(absoluteDist),
  };
  const path = join(absoluteDist, artifactManifestName);
  await writeFile(path, `${JSON.stringify(manifest, null, 2)}\n`, "utf8");
  const actual = JSON.parse(await readFile(path, "utf8"));
  const rescanned = await inspectAdminDist(absoluteDist);
  if (
    JSON.stringify(actual) !== JSON.stringify({ ...manifest, files: rescanned })
  ) {
    fail("Admin artifact manifest does not match the production dist.");
  }
  return manifest;
}

async function main() {
  const dist = process.argv[2] ? resolve(process.argv[2]) : defaultDist;
  const manifest = await writeAndVerifyArtifactManifest(dist);
  console.log(
    `Verified Admin production dist ${manifest.buildVersion} at ${manifest.sourceCommit.slice(0, 12)} (${manifest.files.length} files).`,
  );
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
) {
  main().catch((error) => {
    console.error(
      `Admin artifact verification failed: ${error instanceof Error ? error.message : error}`,
    );
    process.exitCode = 1;
  });
}
