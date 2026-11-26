import { invoke } from "@tauri-apps/api/core";
import type { AccountRecord } from "./types";

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

const storageKey = "maam.iphone.muse-sessions.v1";
const records = new Map<string, SessionList>();
const requests = new Map<string, Promise<void>>();
const failures = new Set<string>();
let eligible = new Set<string>();
let loaded = false;

function persist(): void {
  try { localStorage.setItem(storageKey, JSON.stringify([...records])); } catch { /* The in-memory list still works. */ }
}

export function reconcileIphoneSessions(accounts: AccountRecord[]): void {
  if (!loaded) {
    loaded = true;
    try {
      for (const [uuid, entry] of JSON.parse(localStorage.getItem(storageKey) ?? "[]") as [string, SessionList][]) {
        if (typeof uuid === "string" && entry && Number.isFinite(entry.fetchedAt) && Array.isArray(entry.items)
          && entry.items.every((item) => item && typeof item.id === "string" && typeof item.title === "string")) records.set(uuid, entry);
      }
    } catch { records.clear(); }
  }
  eligible = new Set(accounts.filter((account) => account.providerId === "muse" && account.authState === "authenticated").map((account) => account.uuid));
  let changed = false;
  for (const uuid of records.keys()) {
    if (!eligible.has(uuid)) changed = records.delete(uuid) || changed;
  }
  // Discard the old request identity so a response spanning sign-out cannot refill the cache.
  for (const uuid of requests.keys()) if (!eligible.has(uuid)) requests.delete(uuid);
  for (const uuid of failures) if (!eligible.has(uuid)) failures.delete(uuid);
  if (changed) persist();
}

export function iphoneSessionsFor(uuid: string) {
  return { items: records.get(uuid)?.items ?? [], pending: requests.has(uuid), failed: failures.has(uuid) };
}

export function updateIphoneSessions(account: AccountRecord, force: boolean, notify: () => void): Promise<void> | undefined {
  const uuid = account.uuid;
  if (account.providerId !== "muse" || !eligible.has(uuid)) return;
  const pending = requests.get(uuid);
  if (pending) return pending;
  const cached = records.get(uuid);
  if (!force && cached && !failures.has(uuid) && Date.now() - cached.fetchedAt < 30_000) return;
  const work = invoke<IphoneSession[]>("list_muse_sessions", { uuid }).then((items) => {
    if (requests.get(uuid) !== work || !eligible.has(uuid)) return;
    records.set(uuid, { items: [...new Map(items.map((item) => [item.id, item])).values()], fetchedAt: Date.now() });
    failures.delete(uuid);
    persist();
  }).catch(() => {
    if (requests.get(uuid) === work) failures.add(uuid);
  }).finally(() => {
    if (requests.get(uuid) === work) requests.delete(uuid);
    notify();
  });
  requests.set(uuid, work);
  return work;
}
