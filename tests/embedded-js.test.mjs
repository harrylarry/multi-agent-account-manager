// Runtime tests for the JavaScript embedded in the iOS/Android plugins.
//
// These run the *actual* script text extracted from the sources in a
// stubbed page (node:vm), because a green Swift/Kotlin build says nothing
// about whether the injected JS parses or works:
//   - 2d39149 shipped a SyntaxError in Scripts.swift (unbalanced brace
//     after the fetch hook lost `const origFetch = window.fetch`), so the
//     hook silently never installed on iOS.
//
// Covered here: injection, a normal fetch, no recursion, no double-wrap on
// re-injection, quota capture from a response, rejection propagation, and
// the identity probe's return value.

import assert from "node:assert/strict";
import test from "node:test";
import vm from "node:vm";
import { loadEmbeddedScripts } from "../scripts/embedded-js.mjs";

function storage() {
  const map = new Map();
  return {
    get length() {
      return map.size;
    },
    key: (i) => [...map.keys()][i] ?? null,
    getItem: (k) => (map.has(k) ? map.get(k) : null),
    setItem: (k, v) => map.set(String(k), String(v)),
    removeItem: (k) => map.delete(k),
    clear: () => map.clear(),
  };
}

function jsonResponse(body, { ok = true, contentType = "application/json" } = {}) {
  const text = typeof body === "string" ? body : JSON.stringify(body);
  return {
    ok,
    status: ok ? 200 : 500,
    headers: {
      get: (name) => (String(name).toLowerCase() === "content-type" ? contentType : null),
    },
    clone() {
      return { text: async () => text, json: async () => JSON.parse(text) };
    },
    text: async () => text,
    json: async () => JSON.parse(text),
  };
}

function createPage({ hostname = "muse.ai", pathname = "/", fetchImpl } = {}) {
  const sandbox = {
    console,
    location: {
      protocol: "https:",
      hostname,
      pathname,
      href: `https://${hostname}${pathname}`,
    },
    localStorage: storage(),
    sessionStorage: storage(),
    document: {
      readyState: "complete",
      title: "Muse",
      body: { innerText: "" },
      addEventListener() {},
      querySelectorAll: () => [],
      querySelector: () => null,
    },
    setTimeout,
    clearTimeout,
    setInterval,
    clearInterval,
    XMLHttpRequest: class XMLHttpRequest {},
    WebSocket: class WebSocket {},
  };
  if (fetchImpl !== undefined) sandbox.fetch = fetchImpl;
  sandbox.window = sandbox;
  sandbox.self = sandbox;
  sandbox.top = sandbox;
  const context = vm.createContext(sandbox);
  return { context, run: (code) => vm.runInContext(code, context) };
}

const scripts = loadEmbeddedScripts();
const quotaHook = scripts.ios.quotaHook;
const identityProbe = scripts.ios.identityProbe;

const subscriptionPayload = {
  subscription: {
    subscriptionType: "FREE",
    usage: { state: "METERED", percentUsed: 5 },
  },
};

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

test("every embedded script compiles", () => {
  const named = [
    ["ios quotaHook", scripts.ios.quotaHook],
    ["ios identityProbe", scripts.ios.identityProbe],
    ["android quotaHook", scripts.android.quotaHook],
    ["android identityProbe", scripts.android.identityProbe],
  ];
  for (const [name, code] of named) {
    assert.doesNotThrow(() => new vm.Script(code), `${name} must parse`);
  }
  for (const [name, code] of scripts.extras) {
    assert.doesNotThrow(() => new vm.Script(code), `${name} must parse`);
  }
});

test("iOS and Android inject identical scripts", () => {
  assert.equal(scripts.ios.quotaHook, scripts.android.quotaHook);
  assert.equal(scripts.ios.identityProbe, scripts.android.identityProbe);
});

test("the quota hook defines fetch before using it", () => {
  // Regression guard for 2d39149: `origFetch.apply` without a declaration.
  const declare = quotaHook.indexOf("const origFetch = window.fetch");
  const use = quotaHook.indexOf("origFetch.apply");
  assert.ok(declare >= 0, "must declare `const origFetch = window.fetch`");
  assert.ok(use > declare, "must declare origFetch before the wrapper uses it");
  assert.ok(
    quotaHook.includes("window.__maamOrigFetch = origFetch"),
    "must expose the original fetch for the native replay helpers",
  );
});

test("injection installs the hook and keeps fetch working", async () => {
  const calls = [];
  const original = async (url) => {
    calls.push(url);
    return jsonResponse(subscriptionPayload);
  };
  const page = createPage({ fetchImpl: original });

  page.run(quotaHook);

  assert.equal(page.context.__maamNetHooked, true);
  assert.equal(page.context.__maamOrigFetch, original);
  assert.notEqual(page.context.fetch, original, "fetch must be wrapped");

  const res = await page.context.fetch("/api/bootstrap");
  assert.equal(calls.length, 1, "the original fetch must run exactly once");
  assert.equal(res.ok, true);
});

test("the wrapper does not recurse", async () => {
  let calls = 0;
  const page = createPage({
    fetchImpl: async () => {
      calls += 1;
      if (calls > 3) throw new Error("recursion detected");
      return jsonResponse({ ok: true });
    },
  });
  page.run(quotaHook);
  await page.context.fetch("/api/one");
  await page.context.fetch("/api/two");
  assert.equal(calls, 2);
});

test("a JSON quota response is captured", async () => {
  const page = createPage({ fetchImpl: async () => jsonResponse(subscriptionPayload) });
  page.run(quotaHook);

  await page.context.fetch("/api/subscription");
  await tick();

  const detail = page.context.__maamQuotaDetail;
  assert.ok(detail, "quota detail must be captured");
  assert.equal(detail.planLabel, "免费版");
  assert.equal(detail.planUsedPercent, 5);
  assert.equal(page.context.__maamQuota.text, "已使用 5%");
  assert.equal(page.context.__maamQuota.pct, 95);
  assert.ok(
    page.context.localStorage.getItem("__maam_cached_quota_detail_v2__"),
    "detail must be cached for the next page load",
  );
});

test("a non-JSON response does not produce quota data", async () => {
  const page = createPage({
    fetchImpl: async () => jsonResponse("<html></html>", { contentType: "text/html" }),
  });
  page.run(quotaHook);
  await page.context.fetch("/page");
  await tick();
  assert.equal(page.context.__maamQuotaDetail, undefined);
});

test("re-injection is a no-op (no double wrap)", async () => {
  let calls = 0;
  const page = createPage({
    fetchImpl: async () => {
      calls += 1;
      return jsonResponse({ ok: true });
    },
  });
  page.run(quotaHook);
  const wrapped = page.context.fetch;

  page.run(quotaHook);

  assert.equal(page.context.fetch, wrapped, "second injection must not rewrap");
  await page.context.fetch("/api/again");
  assert.equal(calls, 1, "only the original fetch runs once");
});

test("a page without fetch is left untouched", () => {
  const page = createPage({ fetchImpl: undefined });
  assert.doesNotThrow(() => page.run(quotaHook));
  assert.equal(page.context.__maamNetHooked, true);
  assert.equal(page.context.fetch, undefined);
});

test("fetch rejection still reaches the caller", async () => {
  const page = createPage({
    fetchImpl: async () => {
      throw new Error("network down");
    },
  });
  page.run(quotaHook);
  await assert.rejects(() => page.context.fetch("/api/x"), /network down/);
});

test("the identity probe returns a JSON payload", () => {
  const page = createPage({ fetchImpl: async () => jsonResponse(subscriptionPayload) });
  page.run(quotaHook);

  const raw = page.run(identityProbe);
  assert.equal(typeof raw, "string");
  const parsed = JSON.parse(raw);
  assert.equal(parsed.auth, true);
  assert.equal(parsed.loggedOut, false);
  assert.equal(parsed.email, "");
  assert.equal(typeof parsed.quotaText, "string");
});

test("the identity probe runs the quota hook first", () => {
  const page = createPage({ fetchImpl: async () => jsonResponse({ ok: true }) });
  // No separate injection: the probe script must install the hook itself.
  page.run(identityProbe);
  assert.equal(page.context.__maamNetHooked, true);
  assert.equal(typeof page.context.__maamOrigFetch, "function");
});
