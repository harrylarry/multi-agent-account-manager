import { invoke } from "@tauri-apps/api/core";
import type { AccountRecord, ProviderId } from "./types";

export interface IphoneSession {
  id: string;
  title: string;
  updatedAtMs: number | null;
  pinned: boolean;
  archived: boolean;
}

interface SessionList {
  items: IphoneSession[];
  fetchedAt: number;
}

const storageKey = "maam.iphone.sessions.v2";
const records = new Map<string, SessionList>();
const requests = new Map<string, Promise<void>>();
const failures = new Set<string>();
let eligible = new Set<string>();
let loaded = false;

function persist(): void {
  try { localStorage.setItem(storageKey, JSON.stringify([...records])); } catch { /* The in-memory list still works. */ }
}

const sessionAccountKey = (account: { providerId: ProviderId; uuid: string }) => account.providerId + ":" + account.uuid;

export function reconcileIphoneSessions(accounts: AccountRecord[]): void {
  if (!loaded) {
    loaded = true;
    try {
      localStorage.removeItem("maam.iphone.muse-sessions.v1");
      for (const [uuid, entry] of JSON.parse(localStorage.getItem(storageKey) ?? "[]") as [string, SessionList][]) {
        if (typeof uuid === "string" && entry && Number.isFinite(entry.fetchedAt) && Array.isArray(entry.items)
          && /^(muse|hark):/.test(uuid) && entry.items.every((item) => item && typeof item.id === "string"
            && typeof item.title === "string" && (item.updatedAtMs === null || Number.isFinite(item.updatedAtMs)))) records.set(uuid, entry);
      }
    } catch { records.clear(); }
  }
  eligible = new Set(accounts.filter((account) => account.authState === "authenticated").map(sessionAccountKey));
  let changed = false;
  for (const uuid of records.keys()) {
    if (!eligible.has(uuid)) changed = records.delete(uuid) || changed;
  }
  // Discard the old request identity so a response spanning sign-out cannot refill the cache.
  for (const uuid of requests.keys()) if (!eligible.has(uuid)) requests.delete(uuid);
  for (const uuid of failures) if (!eligible.has(uuid)) failures.delete(uuid);
  if (changed) persist();
}

export function iphoneSessionsFor(account: AccountRecord) {
  const key = sessionAccountKey(account);
  return { items: records.get(key)?.items ?? [], cached: records.has(key), pending: requests.has(key), failed: failures.has(key) };
}

export function updateIphoneSessions(account: AccountRecord, notify: () => void): Promise<void> | undefined {
  const uuid = account.uuid;
  const key = sessionAccountKey(account);
  if (!eligible.has(key)) return;
  const pending = requests.get(key);
  if (pending) return pending;
  const work = invoke<IphoneSession[]>(account.providerId === "muse" ? "list_muse_sessions" : "list_hark_sessions", { uuid }).then((items) => {
    if (requests.get(key) !== work || !eligible.has(key)) return;
    records.set(key, { items: [...new Map(items.map((item) => [item.id, item])).values()], fetchedAt: Date.now() });
    failures.delete(key);
    persist();
  }).catch(() => {
    if (requests.get(key) === work) failures.add(key);
  }).finally(() => {
    if (requests.get(key) === work) requests.delete(key);
    notify();
  });
  requests.set(key, work);
  return work;
}
