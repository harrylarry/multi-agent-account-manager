#!/usr/bin/env node
// Verify (or regenerate) the JavaScript embedded in the account-session
// plugin.
//
// Why this exists: `cargo check`/Xcode compile Swift and Kotlin, but they
// never parse the strings inside. A dropped line in Scripts.swift produced
// a brace imbalance and the whole quota hook silently stopped parsing —
// the app built green and the hook simply never ran. These checks catch
// that class of failure on Linux, without a Mac.
//
// Checks:
//   1. Every embedded JS block compiles (`node --check` on the extracted
//      text, written to a temp file).
//   2. iOS and Android inject byte-identical quota / identity scripts.
//   3. Scripts.swift is exactly what the generator produces from Kotlin
//      (Kotlin is the source of truth; see the file header).
//
// Usage:
//   node scripts/sync-embedded-js.mjs            # check (CI)
//   node scripts/sync-embedded-js.mjs --check    # same
//   node scripts/sync-embedded-js.mjs --write    # regenerate Scripts.swift

import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import {
  SWIFT_SCRIPTS_PATH,
  loadEmbeddedScripts,
  normalizeForCompare,
  renderSwiftFile,
} from "./embedded-js.mjs";

const write = process.argv.includes("--write");
const problems = [];

function fail(message) {
  problems.push(message);
}

function checkSyntax(name, code, dir) {
  const file = join(dir, `${name}.js`);
  writeFileSync(file, code);
  const result = spawnSync(process.execPath, ["--check", file], {
    encoding: "utf8",
  });
  if (result.status !== 0) {
    const detail = (result.stderr || result.stdout || "").trim();
    fail(`${name}: node --check failed\n${detail}`);
  }
}

let { ios, android, extras } = loadEmbeddedScripts();

// Regenerate first when asked, then reload so the checks below run against
// what is now on disk (a Kotlin syntax error must still fail the run).
if (write) {
  const current = readFileSync(SWIFT_SCRIPTS_PATH, "utf8");
  const expected = renderSwiftFile(android);
  if (current !== expected) {
    writeFileSync(SWIFT_SCRIPTS_PATH, expected);
    console.log(`regenerated ${SWIFT_SCRIPTS_PATH}`);
  }
  ({ ios, android, extras } = loadEmbeddedScripts());
}

// 1. Syntax of everything that is injected.
const dir = mkdtempSync(join(tmpdir(), "maam-embedded-js-"));
try {
  checkSyntax("ios-quota-hook", ios.quotaHook, dir);
  checkSyntax("ios-identity-probe", ios.identityProbe, dir);
  checkSyntax("android-quota-hook", android.quotaHook, dir);
  checkSyntax("android-identity-probe", android.identityProbe, dir);
  for (const [name, code] of extras) {
    checkSyntax(`swift-${name}`, code, dir);
  }
} finally {
  rmSync(dir, { recursive: true, force: true });
}

// 2. Parity between the two platforms.
for (const [what, a, b] of [
  ["quotaHook", ios.quotaHook, android.quotaHook],
  ["identityProbe", ios.identityProbe, android.identityProbe],
]) {
  if (normalizeForCompare(a) !== normalizeForCompare(b)) {
    fail(
      `${what}: iOS and Android embedded scripts differ.\n` +
        "  Kotlin (AccountWebActivity.kt) is the source of truth; run\n" +
        "  `node scripts/sync-embedded-js.mjs --write` and review the diff.",
    );
  }
}

// 3. Generated file is up to date (the write path already replaced it).
if (!write) {
  const expected = renderSwiftFile(android);
  const current = readFileSync(SWIFT_SCRIPTS_PATH, "utf8");
  if (current !== expected) {
    fail(
      "Scripts.swift is out of date with the Kotlin source.\n" +
        "  Run `node scripts/sync-embedded-js.mjs --write` and commit the result.",
    );
  }
}

if (problems.length) {
  console.error("embedded-js check FAILED:\n");
  for (const problem of problems) console.error(`- ${problem}\n`);
  process.exit(1);
}

console.log(
  `embedded-js OK: ${extras.size + 2} script(s) compile, iOS/Android parity, Scripts.swift in sync`,
);
