/* global process */
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const directory = new URL("./", import.meta.url);
const generated = execFileSync(process.execPath, [
  fileURLToPath(new URL("generate.mjs", directory))
]);
const expected = readFileSync(new URL("expected.json", directory));

if (!generated.equals(expected)) {
  process.stderr.write("OpenAI Codex Hook trust fixture mismatch\n");
  process.exitCode = 1;
} else {
  process.stdout.write("OpenAI Codex Hook trust fixture: PASS\n");
}
