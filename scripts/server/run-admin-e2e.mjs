import { spawn } from "node:child_process";
import { createServer } from "node:net";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const workspaceRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const port = await freePort();
const integration = process.argv.includes("--integration");
const passthroughArgs = process.argv
  .slice(2)
  .filter((value) => value !== "--integration");
const script = integration ? "test:e2e:integration" : "test:e2e";
const executable =
  process.platform === "win32"
    ? (process.env.ComSpec ?? "cmd.exe")
    : "corepack";
const args =
  process.platform === "win32"
    ? [
        "/d",
        "/s",
        "/c",
        [
          `corepack pnpm@11.10.0 --filter @promptdock/relay-admin ${script}`,
          ...passthroughArgs.map(quoteForCmd),
        ].join(" "),
      ]
    : [
        "pnpm@11.10.0",
        "--filter",
        "@promptdock/relay-admin",
        script,
        ...passthroughArgs,
      ];
const child = spawn(executable, args, {
  cwd: workspaceRoot,
  env: {
    ...process.env,
    [integration ? "ADMIN_INTEGRATION_PORT" : "ADMIN_E2E_PORT"]: String(port),
  },
  stdio: "inherit",
  windowsHide: true,
});

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.once(signal, () => child.kill(signal));
}

child.once("error", (error) => {
  console.error(error);
  process.exitCode = 1;
});
child.once("exit", (code) => {
  process.exitCode = code ?? 1;
});

function freePort() {
  return new Promise((resolvePort, reject) => {
    const server = createServer();
    server.unref();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (!address || typeof address === "string") {
        server.close();
        reject(new Error("Could not allocate an E2E port."));
        return;
      }
      server.close((error) =>
        error ? reject(error) : resolvePort(address.port),
      );
    });
  });
}

function quoteForCmd(value) {
  if (!/^[A-Za-z0-9_./:=*-]+$/.test(value)) {
    throw new Error(`Unsupported Playwright argument: ${value}`);
  }
  return value;
}
