// Embedded JavaScript in the account-session plugin.
//
// The iOS plugin injects the same quota/identity scripts as the Android
// plugin. The documented convention (see the header of Scripts.swift) is
// that the Kotlin constants are the single source of truth and the Swift
// literals are extracted from them, so the two platforms cannot drift.
//
// This module does the extraction for both directions:
//   - read Kotlin  -> the authoritative script text
//   - read Swift   -> what is actually shipped on iOS
// and is used by scripts/sync-embedded-js.mjs (regenerate / verify) and by
// tests/embedded-js.test.mjs (execute the scripts in a stubbed page).
//
// It also collects every other raw-string JavaScript block in the plugin
// (e.g. TaskWatcher.taskStateScript) so `node --check` covers them too.

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

export const REPO_ROOT = dirname(dirname(fileURLToPath(import.meta.url)));

export const SWIFT_SCRIPTS_PATH = join(
  REPO_ROOT,
  "src-tauri/plugins/account-session/ios/Sources/Scripts.swift",
);
export const SWIFT_TASK_WATCHER_PATH = join(
  REPO_ROOT,
  "src-tauri/plugins/account-session/ios/Sources/TaskWatcher.swift",
);
export const KOTLIN_ACTIVITY_PATH = join(
  REPO_ROOT,
  "src-tauri/plugins/account-session/android/src/main/java/com/ldjx/accountmanager/AccountWebActivity.kt",
);

const SWIFT_BLOCK = /static let (\w+) = (?:(\w+) \+ )?#"""\r?\n([\s\S]*?)"""#/g;
const KOTLIN_BLOCK = /const val (\w+) = (?:(\w+) \+ )?"""\r?\n([\s\S]*?)"""/g;

/**
 * Remove the common leading indentation from a raw-string literal body.
 *
 * Both Kotlin and Swift keep the source indentation inside the literal;
 * comparing (and compiling) the dedented form makes the check independent
 * of how each file is indented. Trailing spaces are stripped per line so a
 * stray space never fails the parity check.
 */
export function dedent(raw) {
  const lines = raw.replace(/^\r?\n/, "").split(/\r?\n/);
  // Trailing whitespace-only lines are source indentation, not content.
  while (lines.length && !lines[lines.length - 1].trim()) lines.pop();
  let min = Infinity;
  for (const line of lines) {
    if (!line.trim()) continue;
    min = Math.min(min, line.match(/^[ \t]*/)[0].length);
  }
  if (!Number.isFinite(min)) min = 0;
  return (
    lines
      .map((line) => {
        const lead = line.match(/^[ \t]*/)[0].length;
        return line.slice(Math.min(min, lead)).replace(/[ \t]+$/, "");
      })
      .join("\n") + "\n"
  );
}

function readBlocks(source, pattern) {
  const blocks = new Map();
  for (const match of source.matchAll(pattern)) {
    blocks.set(match[1], { base: match[2] ?? null, body: match[3] });
  }
  return blocks;
}

/**
 * Compose a constant that is written as `base + """..."""` in the source.
 * `base` is the raw text of the referenced constant, not its name.
 */
function compose(blocks, name, resolver) {
  const block = blocks.get(name);
  if (!block) throw new Error(`embedded JS constant not found: ${name}`);
  const own = dedent(block.body);
  if (!block.base) return own;
  const base = resolver(block.base);
  // The concatenation in both languages keeps the two literals adjacent;
  // a single newline reproduces the injected text (the raw string already
  // starts and ends with a newline).
  return `${base}\n${own}`;
}

export function parseSwiftScripts(source) {
  const blocks = readBlocks(source, SWIFT_BLOCK);
  const quota = compose(blocks, "quotaHookInitScript", () => {
    throw new Error("quotaHookInitScript must be a standalone literal");
  });
  const identity = compose(blocks, "identityProbeScript", () => quota);
  return { quotaHook: quota, identityProbe: identity };
}

export function parseKotlinScripts(source) {
  const blocks = readBlocks(source, KOTLIN_BLOCK);
  const quota = compose(blocks, "QUOTA_HOOK_INIT_SCRIPT", () => {
    throw new Error("QUOTA_HOOK_INIT_SCRIPT must be a standalone literal");
  });
  const identity = compose(blocks, "IDENTITY_PROBE_SCRIPT", () => quota);
  return { quotaHook: quota, identityProbe: identity };
}

/** Any other raw-string block in the plugin sources that is JavaScript. */
export function parseExtraScripts(paths) {
  const out = new Map();
  for (const path of paths) {
    const source = readFileSync(path, "utf8");
    const pattern = path.endsWith(".kt")
      ? /const val (\w+) = """\r?\n([\s\S]*?)"""/g
      : /static let (\w+) = #"""\r?\n([\s\S]*?)"""#/g;
    for (const match of source.matchAll(pattern)) {
      const [, name, body] = match;
      const text = dedent(body);
      // Only keep blocks that look like the injected code, not SQL or
      // regex literals that happen to use the same quoting.
      if (/\bwindow\b|\bdocument\b|=>/.test(text)) out.set(name, text);
    }
  }
  return out;
}

export function loadEmbeddedScripts() {
  const ios = parseSwiftScripts(readFileSync(SWIFT_SCRIPTS_PATH, "utf8"));
  const android = parseKotlinScripts(readFileSync(KOTLIN_ACTIVITY_PATH, "utf8"));
  const extras = parseExtraScripts([SWIFT_TASK_WATCHER_PATH]);
  return { ios, android, extras };
}

function indent(text, spaces) {
  const pad = " ".repeat(spaces);
  return text
    .replace(/\n$/, "")
    .split("\n")
    .map((line) => (line ? pad + line : line))
    .join("\n");
}

/**
 * Render Scripts.swift from the Kotlin source. The output is deterministic:
 * running it twice produces the same bytes, and `--check` compares the
 * result against the file on disk.
 */
export function renderSwiftFile(android) {
  // The Kotlin constant is `QUOTA_HOOK_INIT_SCRIPT + """..."""`, so the
  // probe body is everything after the quota hook plus the separating
  // newline. Leading newlines are stripped because the Swift raw string
  // already contributes one.
  const probeBody = android.identityProbe
    .slice(android.quotaHook.length)
    .replace(/^\n+/, "");
  return `// Scripts.swift — JavaScript injected into account web views.
//
// GENERATED FILE — do not edit by hand.
//
// Source of truth: AccountWebActivity.kt (QUOTA_HOOK_INIT_SCRIPT and
// IDENTITY_PROBE_SCRIPT). The two platforms must inject byte-identical
// JavaScript; hand-editing this file is what previously broke it (a
// dropped \`const origFetch = window.fetch\` left an unbalanced brace, so
// the whole script failed to parse and the hook never installed).
//
// Regenerate:  node scripts/sync-embedded-js.mjs --write
// Verify:      node scripts/sync-embedded-js.mjs --check   (also in CI)

enum Scripts {
  /// fetch/WebSocket hooks that capture quota-related responses and stash
  /// them on window.__maamQuota. Injected once per web view at document
  /// start via WKUserScript.
  static let quotaHookInitScript = #"""
${indent(android.quotaHook, 12)}
"""#

  /// Identity probe: runs the quota hook first, then inspects the DOM /
  /// cookies to detect the signed-in identity and logout state.
  /// Evaluated on demand (probe timer) via WKWebView.evaluateJavaScript.
  /// Returns a JSON string: { status, identity, avatarUrl, quotaText,
  /// quotaPercent, quotaDetail, providerId? } — same shape as Android.
  static let identityProbeScript = quotaHookInitScript + #"""
${indent(probeBody, 12)}
"""#
}
`;
}

export function normalizeForCompare(text) {
  return text
    .split("\n")
    .map((line) => line.replace(/[ \t]+$/, ""))
    .join("\n")
    .replace(/^\n+/, "")
    .replace(/\n+$/, "");
}
