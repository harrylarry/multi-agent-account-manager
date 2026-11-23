import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./styles.css";
import type {
  AccountKey,
  AccountRecord,
  InstallUpdateOutcome,
  ManagerSnapshot,
  ProviderId,
  ProviderInfo,
  QuotaDetail,
  UpdateCheckResult,
  UpdateProgressEvent,
} from "./types";

type ModalMode =
  | { kind: "none" }
  | { kind: "add"; providerId: ProviderId; draft: string }
  | { kind: "rename"; account: AccountRecord; draft: string }
  | { kind: "remove"; account: AccountRecord }
  | { kind: "update" };

const PROVIDER_LOGOS: Record<ProviderId, { src: string; fullBleed: boolean }> = {
  muse: { src: "/logos/muse.png", fullBleed: true },
  hark: { src: "/logos/hark.svg", fullBleed: false },
};

const ICON_EXTERNAL = `<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/><polyline points="15 3 21 3 21 9"/><line x1="10" y1="14" x2="21" y2="3"/></svg>`;
const ICON_REFRESH = `<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M21.5 2v6h-6M21.34 15.57a10 10 0 1 1-.57-8.38l5.67-5.67"/></svg>`;
const ICON_EDIT = `<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h9"/><path d="M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4L16.5 3.5z"/></svg>`;
const ICON_TRASH = `<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/></svg>`;
const ICON_USER_PLUS = `<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><line x1="19" y1="8" x2="19" y2="14"/><line x1="22" y1="11" x2="16" y2="11"/></svg>`;
const ICON_UPDATE = `<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg>`;
const ICON_DRAG = `<svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor"><circle cx="9" cy="6" r="1.6"/><circle cx="15" cy="6" r="1.6"/><circle cx="9" cy="12" r="1.6"/><circle cx="15" cy="12" r="1.6"/><circle cx="9" cy="18" r="1.6"/><circle cx="15" cy="18" r="1.6"/></svg>`;

let snapshot: ManagerSnapshot | null = null;
let modal: ModalMode = { kind: "none" };
let optimisticSwitching: AccountKey | null = null;
// Spike 原生诊断（0.1.22）：Muse 页底部显示诊断区。
// 诊断结果不进 structuralSignature，改动后只同步诊断区 DOM，不重建列表。
// 注：诊断区无条件显示（日常版也有，方便后续配合调试——用户 2026-11-23 定）。
// Spike 徽章则按 bundle id 门控（is_spike_build 命令），只在 Spike 版显示。
interface SpikeDiagnoseResult {
  ok: boolean;
  phase: string;
  handshakeCompleted: boolean;
  pingStatus: number | null;
  cvmDetected: boolean;
  error: string | null;
}
let spikeDiagnose: SpikeDiagnoseResult | null = null;
let spikeDiagnoseRunning = false;
interface SpikeWatchSummary {
  events: number;
  turnsStarted: number;
  messagesStarted: number;
  textDeltas: number;
  messagesDone: number;
  busy: number;
  idle: number;
  duplicates: number;
  ignored: number;
  eventsByName?: Record<string, number>;
}
interface SpikeWatchResult {
  ok: boolean;
  phase: string;
  seconds: number;
  status: number | null;
  summary: SpikeWatchSummary;
  lines?: number;
  nonEvent?: number;
  parseErrors?: number;
  typeCounts?: Record<string, number>;
  error: string | null;
}
let spikeWatch: SpikeWatchResult | null = null;
let spikeWatchRunning = false;
let spikeWatchError: string | null = null;
let spikeWatchAccount: string | null = null;
interface SpikeApprovalItem {
  id: string; kind: string; displayName: string;
  requestedAtMs: number; expiresAtMs: number; status: string;
}
interface SpikeApprovalsResult {
  ok: boolean; phase: string; status: number | null;
  count: number; first: SpikeApprovalItem | null; error: string | null;
}
let spikeApprovals: SpikeApprovalsResult | null = null;
let spikeApprovalsRunning = false;
let spikeApprovalsError: string | null = null;
interface SpikeQuota {
  tier: string; percentUsed: number; state: string; resetsAtMs: number; source: string;
}
let spikeQuota: SpikeQuota | null = null;
let spikeQuotaRunning = false;
let spikeQuotaError: string | null = null;
interface SpikeSendResult {
  ok: boolean; status: string; sendId: string;
  ack: unknown; sessionId: string | null; error: string | null;
}
let spikeSend: SpikeSendResult | null = null;
let spikeSendRunning = false;
let spikeSendError: string | null = null;
let spikeDiagnoseError: string | null = null;
// 是否 Spike 版（bundle id 门控）：只决定顶栏徽章显不显示；诊断区不门控。
let isSpikeBuild = false;
let busy = false;
// Batch-1: a switch requested while another command is in flight is
// queued (latest wins) instead of silently dropped.
let pendingSwitch: {
  cmd: string;
  args: Record<string, unknown>;
  optimisticTarget: AccountKey | null;
} | null = null;
// Batch-1 reorder mode: long-press a row to reorder accounts by drag.
let reorderMode = false;
let reorderProviderId: ProviderId | null = null;
// True while a reorder drag is in progress. render() skips while set: a
// snapshot arriving mid-drag must not rebuild the list under the finger
// (the snapshot data is already stored; the next render picks it up).
let dragActive = false;
// 0.1.9 incremental rendering: structural signature of the last full
// render + per-row rendered state, so field-only snapshots (the 4.5s
// quota probes) patch rows in place instead of rebuilding #app.
let lastStructuralSig: string | null = null;
let patchState: Map<
  string,
  { avatarSig: string; subVariant: RowSubVariant; detailHtml: string }
> = new Map();
// 0.1.9 event delegation: long-press / drag state used to live in the
// per-render bindEvents closures; listeners are now bound once at
// bootstrap, so the state lives here.
let longPressTimer: ReturnType<typeof setTimeout> | null = null;
let longPressStart: { x: number; y: number } | null = null;
let dragRow: HTMLElement | null = null;
let dragTouchId: number | null = null;
let lastRenderedUpdateStage: string | null = null;
// 0.1.21: identity key of the last modal HTML written into #modal-root,
// and the mount element it was written into — renderModalRoot() skips
// the write when neither changed, so a re-render never disturbs modal
// input focus. The key is the modal's identity (kind / account), NOT
// the full HTML: typing in the rename/add input only changes
// modal.draft, which must not trigger a rewrite — the live input node
// already holds the typed value, and rewriting it would replace the
// node and drop focus. The update modal is the exception: its content
// is dynamic (check / install progress), so it falls back to comparing
// the full HTML. The mount changes identity on every full #app
// rebuild, which is how the cache knows the new (empty) container
// still needs filling.
let lastModalKey: string | null = null;
let lastModalMount: HTMLElement | null = null;

const updateState: {
  checking: boolean;
  installing: boolean;
  result: UpdateCheckResult | null;
  progress: UpdateProgressEvent | null;
  error: string | null;
  outcomeStatus: string | null;
  outcomeMessage: string | null;
} = {
  checking: false,
  installing: false,
  result: null,
  progress: null,
  error: null,
  outcomeStatus: null,
  outcomeMessage: null,
};

const scrollOffsets: Record<ProviderId, number> = {
  muse: 0,
  hark: 0,
};

function isSameKey(a: AccountKey | null, b: AccountRecord): boolean {
  return Boolean(a && a.providerId === b.providerId && a.uuid === b.uuid);
}

function effectiveSwitchingKey(state: ManagerSnapshot): AccountKey | null {
  return state.switchingAccount ?? optimisticSwitching;
}

function isMobilePlatform(platform: string): boolean {
  return platform === "android" || platform === "ios";
}

function escapeHtml(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

function renderProviderLogo(id: ProviderId, extraClass = ""): string {
  const cfg = PROVIDER_LOGOS[id];
  const cls = `${extraClass} ${cfg.fullBleed ? "full-bleed" : ""}`.trim();
  return `<img src="${cfg.src}" alt="${escapeHtml(id)}" class="${cls}" draggable="false" />`;
}

function computeAvatarText(acc: AccountRecord): string {
  if (acc.autoAlias) {
    return "@";
  }
  const clean = acc.alias.trim();
  const first = Array.from(clean)[0];
  return first ? first.toUpperCase() : "#";
}

function splitQuotaValue(raw: string): { main: string; note: string | null } {
  const m = raw.trim().match(/^(.*?)\s*[（(]([^）)]+)[）)]\s*$/);
  if (m && m[1].trim()) {
    return { main: m[1].trim(), note: m[2].trim() };
  }
  return { main: raw.trim(), note: null };
}

type QuotaStatus = "normal" | "extra" | "low" | "critical";

function resolveQuotaState(acc: AccountRecord): {
  status: QuotaStatus;
  label: string;
  extraDepleted: boolean;
} {
  if (acc.quotaDetail) {
    const d = acc.quotaDetail;
    const planUsed = d.planUsedPercent;
    const planVal = d.planValue || "";
    const planDepleted =
      (planUsed !== null && planUsed >= 100) ||
      planVal.includes("用尽") ||
      planVal.includes("DEPLETED");
    const planLow =
      !planDepleted &&
      ((planUsed !== null && planUsed >= 85) ||
        planVal.includes("偏低") ||
        planVal.includes("LOW"));
    const hasExtra = Boolean(
      d.extraLabel || d.extraValue || d.extraUsedPercent !== null
    );
    const extraUsed = d.extraUsedPercent;
    const extraVal = d.extraValue || "";
    const extraDepleted =
      hasExtra &&
      ((extraUsed !== null && extraUsed >= 100) ||
        extraVal.includes("用尽") ||
        extraVal.includes("耗尽") ||
        extraVal.includes("DEPLETED") ||
        /剩余\s*0(\s|个|口|$)/.test(extraVal));
    const extraLow =
      hasExtra &&
      !extraDepleted &&
      ((extraUsed !== null && extraUsed >= 85) ||
        extraVal.includes("偏低") ||
        extraVal.includes("LOW"));
    const extraIsCap = Boolean(d.extraLabel && /月|monthly/i.test(d.extraLabel));

    if (extraIsCap) {
      if (planDepleted) {
        return { status: "critical", label: "日满", extraDepleted };
      }
      if (extraDepleted) {
        return { status: "critical", label: "月满", extraDepleted };
      }
      if (planLow || extraLow) {
        return { status: "low", label: "偏低", extraDepleted };
      }
      return { status: "normal", label: "", extraDepleted };
    }

    if (planDepleted) {
      if (!hasExtra || extraDepleted) {
        return { status: "critical", label: "用尽", extraDepleted };
      }
      if (extraLow) {
        return { status: "low", label: "偏低", extraDepleted };
      }
      return { status: "extra", label: "用额外", extraDepleted };
    }
    if (planLow) {
      if (!hasExtra || extraDepleted) {
        return { status: "low", label: "偏低", extraDepleted };
      }
      return { status: "normal", label: "", extraDepleted };
    }
    return { status: "normal", label: "", extraDepleted };
  }

  if (acc.quotaPercent === 0 || Boolean(acc.quotaText?.includes("用尽"))) {
    return { status: "critical", label: "用尽", extraDepleted: false };
  }
  if (
    (typeof acc.quotaPercent === "number" && acc.quotaPercent <= 15) ||
    Boolean(acc.quotaText?.includes("偏低"))
  ) {
    return { status: "low", label: "偏低", extraDepleted: false };
  }
  return { status: "normal", label: "", extraDepleted: false };
}

// 0.1.9: per-row derived state, shared by the full renderer and the
// incremental patcher so the two can never drift apart.
type RowSubVariant = "loading" | "logged-out" | "quota" | "none";

interface DerivedRowState {
  switching: boolean;
  active: boolean;
  isLoggedOut: boolean;
  hasDetail: boolean;
  // 0.1.15: task watcher saw a new reply while this account was not
  // active (never shown on the active row — the user is looking at it).
  hasUnreadReply: boolean;
  alertStatus: QuotaStatus;
  alertLabel: string;
  extraDepleted: boolean;
  avatarText: string;
  subVariant: RowSubVariant;
  quotaLabel: string;
  isEmpty: boolean;
  isLow: boolean;
}

function deriveRowState(
  acc: AccountRecord,
  state: ManagerSnapshot,
  switchingKey: AccountKey | null
): DerivedRowState {
  const switching = isSameKey(switchingKey, acc);
  const active = !switching && isSameKey(state.activeAccount, acc);
  const hasUnreadReply = !active && Boolean(acc.hasUnreadReply);
  const avatarText = computeAvatarText(acc);
  const isLoggedOut = acc.authState === "logged_out";
  const hasDetail = !isLoggedOut && Boolean(acc.quotaDetail);
  const quotaState = isLoggedOut
    ? { status: "normal" as QuotaStatus, label: "", extraDepleted: false }
    : resolveQuotaState(acc);
  const subVariant: RowSubVariant =
    switching && !hasDetail
      ? "loading"
      : isLoggedOut
        ? "logged-out"
        : !hasDetail
          ? "quota"
          : "none";
  const quotaLabel =
    acc.quotaText ?? (acc.autoAlias ? "登录后自动同步额度" : "正在同步额度明细…");
  const isEmpty =
    acc.quotaPercent === 0 || Boolean(acc.quotaText?.includes("用尽"));
  const isLow =
    !isEmpty &&
    ((typeof acc.quotaPercent === "number" && acc.quotaPercent <= 20) ||
      Boolean(acc.quotaText?.includes("偏低")));
  return {
    switching,
    active,
    isLoggedOut,
    hasDetail,
    hasUnreadReply,
    alertStatus: quotaState.status,
    alertLabel: quotaState.label,
    extraDepleted: quotaState.extraDepleted,
    avatarText,
    subVariant,
    quotaLabel,
    isEmpty,
    isLow,
  };
}

function rowClassFor(d: DerivedRowState): string {
  return [
    "account-row",
    d.hasDetail ? "has-detail" : "",
    d.active ? "active" : "",
    d.switching ? "switching" : "",
    d.isLoggedOut ? "logged-out" : "",
    d.alertStatus !== "normal" ? `alert-${d.alertStatus}` : "",
  ]
    .filter(Boolean)
    .join(" ");
}

function avatarWrapHtml(acc: AccountRecord, d: DerivedRowState): string {
  const avatarClass = [
    "avatar",
    acc.autoAlias ? "pending-identity" : acc.providerId,
  ].join(" ");
  const avatarNodeHtml = acc.avatarUrl
    ? `<div class="avatar has-photo"><img src="${escapeHtml(acc.avatarUrl)}" class="avatar-photo" alt="" draggable="false" /></div>`
    : `<div class="${avatarClass}">${escapeHtml(d.avatarText)}</div>`;
  return `${avatarNodeHtml}
${d.switching ? `<span class="avatar-spinner"></span>` : ""}
${d.active ? `<span class="status-dot ${d.isLoggedOut ? "offline" : ""}"></span>` : ""}
${d.hasUnreadReply ? `<span class="unread-dot"></span>` : ""}`;
}

function avatarSigFor(acc: AccountRecord, d: DerivedRowState): string {
  return [
    acc.avatarUrl ?? "",
    d.avatarText,
    acc.autoAlias ? "1" : "0",
    d.switching ? "1" : "0",
    d.active ? "1" : "0",
    d.isLoggedOut ? "1" : "0",
    d.hasUnreadReply ? "1" : "0",
  ].join("|");
}

function alertBadgeHtml(d: DerivedRowState): string {
  if (d.isLoggedOut) return `<span class="auth-status-badge">未登录</span>`;
  if (d.alertStatus !== "normal") {
    return `<span class="quota-warning-badge ${d.alertStatus}">${d.alertLabel}</span>`;
  }
  return "";
}

function loginHintFor(
  acc: AccountRecord,
  d: DerivedRowState,
  isMobile: boolean
): string {
  if (d.active) {
    return isMobile ? "登录已失效，点击进入重新登录" : "登录已失效，请在右侧重新登录";
  }
  return acc.autoAlias ? "尚未登录，点击前往登录" : "登录已失效，点击重新登录";
}

function subHtmlFor(
  acc: AccountRecord,
  d: DerivedRowState,
  isMobile: boolean
): string {
  if (d.subVariant === "loading") {
    return `<div class="account-sub loading-text"><span class="quota-label-text">正在加载…</span></div>`;
  }
  if (d.subVariant === "logged-out") {
    return `<div class="account-sub auth-logged-out"><span class="auth-dot-inline"></span><span class="quota-label-text">${loginHintFor(acc, d, isMobile)}</span></div>`;
  }
  if (d.subVariant === "quota") {
    const subClass = d.isEmpty
      ? "account-sub quota-empty"
      : d.isLow
        ? "account-sub quota-low"
        : "account-sub";
    const barHtml =
      typeof acc.quotaPercent === "number"
        ? `<span class="mini-quota-track"><span class="mini-quota-fill ${d.isEmpty ? "empty" : d.isLow ? "low" : ""}" style="width:${Math.max(0, Math.min(100, acc.quotaPercent))}%"></span></span>`
        : "";
    return `<div class="${subClass}">${barHtml}<span class="quota-label-text">${escapeHtml(d.quotaLabel)}</span></div>`;
  }
  return "";
}

function formatShortResetText(raw: string | null): string {
  const clean = (raw ?? "").trim();
  if (!clean) return "";
  const m = clean.match(/^(?:每周限额将在|周限额重置时间[:：]?|将在)\s*(.+?)\s*$/);
  if (m && m[1]) {
    const inner = m[1].trim();
    return inner.endsWith("重置") ? inner : `${inner}重置`;
  }
  return clean;
}

function formatShortExtraText(
  detail: QuotaDetail,
  extraDepleted: boolean
): string | null {
  if (
    !detail.extraLabel &&
    !detail.extraValue &&
    detail.extraUsedPercent === null
  ) {
    return null;
  }
  const prefix = detail.extraLabel && /月|monthly/i.test(detail.extraLabel) ? "月" : "额外";
  if (extraDepleted) {
    return `${prefix} 100% · 已耗尽`;
  }
  const parsed = splitQuotaValue(detail.extraValue ?? "");
  const pctPart =
    typeof detail.extraUsedPercent === "number"
      ? `${detail.extraUsedPercent}%`
      : parsed.main.replace(/^已使用\s*/, "").trim();
  const notePart = parsed.note
    ? parsed.note
        .replace(/^剩余\s*/, "余 ")
        .replace(/个?口令$/, "")
        .replace(/\s*tokens?$/i, "")
        .trim()
    : "";
  if (pctPart && notePart) {
    return `${prefix} ${pctPart} · ${notePart}`;
  }
  if (notePart) {
    return `${prefix} · ${notePart}`;
  }
  if (pctPart) {
    return `${prefix} ${pctPart}`;
  }
  return detail.extraLabel ?? "额外额度";
}

function renderQuotaDetailPanel(
  detail: QuotaDetail,
  status: QuotaStatus,
  extraDepleted: boolean
): string {
  const shortVal =
    typeof detail.planUsedPercent === "number"
      ? `${detail.planUsedPercent}%`
      : splitQuotaValue(detail.planValue).main;
  const clamped =
    typeof detail.planUsedPercent === "number"
      ? Math.max(0, Math.min(100, detail.planUsedPercent))
      : null;
  const barHtml =
    clamped !== null
      ? `<div class="quota-bar-track ${status}"><div class="quota-bar-fill ${status} ${clamped > 0 ? "has-fill" : ""}" style="width:${clamped}%"></div></div>`
      : `<div class="quota-compact-spacer"></div>`;

  const rawReset = detail.planSubtext ?? "";
  const shortReset = formatShortResetText(rawReset);
  const shortExtra = formatShortExtraText(detail, extraDepleted);
  const fullExtraTitle = [
    detail.extraLabel,
    detail.extraValue,
    detail.extraSubtext,
  ]
    .filter(Boolean)
    .join(" · ");

  const metaHtml =
    shortReset || shortExtra
      ? `
        <div class="quota-compact-meta">
          <span class="quota-sub-text" title="${escapeHtml(rawReset)}">${escapeHtml(shortReset)}</span>
          ${shortExtra ? `<span class="quota-extra-pill ${status}" title="${escapeHtml(fullExtraTitle)}">${escapeHtml(shortExtra)}</span>` : ""}
        </div>
      `
      : "";

  return `
    <div class="quota-compact-main">
      <span class="quota-plan-label ${status}">${escapeHtml(detail.planLabel)}</span>
      ${barHtml}
      <span class="quota-val ${status}">${escapeHtml(shortVal)}</span>
    </div>
    ${metaHtml}
  `;
}

function saveScrollPosition(): void {
  if (!snapshot) return;
  const listEl = document.querySelector<HTMLElement>(".account-list");
  if (listEl) {
    scrollOffsets[snapshot.selectedProviderId] = listEl.scrollTop;
  }
}

function restoreScrollPosition(): void {
  if (!snapshot) return;
  const listEl = document.querySelector<HTMLElement>(".account-list");
  if (listEl) {
    listEl.scrollTop = scrollOffsets[snapshot.selectedProviderId] ?? 0;
  }
}

function rerenderPreservingScroll(): void {
  saveScrollPosition();
  render();
  restoreScrollPosition();
}

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  let idx = 0;
  let val = bytes;
  while (val >= 1024 && idx < units.length - 1) {
    val /= 1024;
    idx += 1;
  }
  return idx === 0 ? `${Math.round(val)} B` : `${val.toFixed(1)} ${units[idx]}`;
}

function formatProgressSummary(evt: UpdateProgressEvent): string {
  if (evt.stage === "verifying") return "校验中";
  if (evt.stage === "installing") return "正在替换";
  if (evt.stage === "ready") return "完成";
  if (evt.stage === "needs_permission") return "待授权";
  const pct = typeof evt.percent === "number" ? `${evt.percent}%` : "";
  const transferred = evt.total
    ? `${formatBytes(evt.downloaded)} / ${formatBytes(evt.total)}`
    : evt.downloaded > 0
      ? formatBytes(evt.downloaded)
      : "";
  if (pct && transferred) return `${pct} · ${transferred}`;
  return pct || transferred;
}

async function runCheckForUpdates(silent: boolean): Promise<void> {
  if (updateState.checking || updateState.installing) return;
  updateState.checking = true;
  if (!silent) {
    updateState.error = null;
    updateState.outcomeStatus = null;
    updateState.outcomeMessage = null;
    if (modal.kind === "update") {
      rerenderPreservingScroll();
    }
  }
  try {
    const res = await invoke<UpdateCheckResult>("check_for_updates");
    updateState.result = res;
    updateState.error = null;
  } catch (err) {
    if (!silent) {
      updateState.error = String(err);
    }
  } finally {
    updateState.checking = false;
    if (!silent || updateState.result?.hasUpdate || modal.kind === "update") {
      rerenderPreservingScroll();
    }
  }
}

async function runDownloadAndInstall(): Promise<void> {
  const res = updateState.result;
  if (!res?.downloadUrl || updateState.checking || updateState.installing) return;
  updateState.installing = true;
  updateState.error = null;
  updateState.outcomeStatus = null;
  updateState.outcomeMessage = null;
  lastRenderedUpdateStage = "downloading";
  updateState.progress = {
    stage: "downloading",
    downloaded: 0,
    total: res.size,
    percent: 0,
    message: "正在准备下载...",
  };
  rerenderPreservingScroll();
  try {
    const outcome = await invoke<InstallUpdateOutcome>("download_and_install_update", {
      request: {
        version: res.latestVersion,
        downloadUrl: res.downloadUrl,
        sha256: res.sha256,
        filename: res.filename,
      },
    });
    updateState.outcomeStatus = outcome.status;
    updateState.outcomeMessage = outcome.message;
  } catch (err) {
    updateState.error = String(err);
  } finally {
    updateState.installing = false;
    rerenderPreservingScroll();
  }
}

async function setModal(next: ModalMode): Promise<void> {
  const wasActive = modal.kind !== "none";
  const isActive = next.kind !== "none";
  modal = next;
  if (wasActive !== isActive) {
    try {
      snapshot = await invoke<ManagerSnapshot>("set_modal_active", {
        active: isActive,
      });
    } catch (err) {
      if (snapshot) {
        snapshot = { ...snapshot, notice: String(err) };
      }
    }
  }
  render();
  if (next.kind === "add" || next.kind === "rename") {
    const input = document.querySelector<HTMLInputElement>("#modal-alias-input");
    if (input) {
      input.focus();
      input.select();
    }
  } else if (next.kind === "remove") {
    document.querySelector<HTMLButtonElement>("#modal-confirm-btn")?.focus();
  }
}

async function callCommand(
  cmd: string,
  args: Record<string, unknown> = {},
  optimisticTarget: AccountKey | null = null
): Promise<void> {
  if (busy) {
    // Batch-1: don't silently drop taps. A switch requested while
    // another command is in flight is queued (only the latest one
    // matters) and runs as soon as the current command finishes;
    // other commands keep the old drop behavior.
    if (cmd === "switch_account") {
      pendingSwitch = { cmd, args, optimisticTarget };
      if (optimisticTarget) {
        optimisticSwitching = optimisticTarget;
        if (snapshot) {
          snapshot = { ...snapshot, activeAccount: null };
        }
        render();
        restoreScrollPosition();
      }
    }
    return;
  }
  busy = true;
  saveScrollPosition();
  if (optimisticTarget) {
    optimisticSwitching = optimisticTarget;
    if (snapshot) {
      snapshot = { ...snapshot, activeAccount: null };
    }
    render();
    restoreScrollPosition();
  }
  try {
    snapshot = await invoke<ManagerSnapshot>(cmd, args);
  } catch (err) {
    if (snapshot) {
      snapshot = { ...snapshot, notice: String(err) };
    }
  } finally {
    optimisticSwitching = null;
    busy = false;
    render();
    restoreScrollPosition();
    if (pendingSwitch) {
      const next = pendingSwitch;
      pendingSwitch = null;
      await callCommand(next.cmd, next.args, next.optimisticTarget);
    }
  }
}

// Batch-1: silent sync triggers (iOS). Fire-and-forget: the Rust side
// throttles to one sync per 30s and skips the currently open account
// (the foreground probe already covers it).
function triggerAutoSync(): void {
  void invoke("trigger_quota_sync", { force: false }).catch(() => {});
}

// Spike 原生诊断（0.1.22）：诊断区只在 Spike 版的 Muse 页显示。
// 结果只同步诊断区 DOM（syncSpikeDiag），不走 render()，不重建列表。
function spikeDiagInnerHtml(): string {
  const btnLabel = spikeDiagnoseRunning ? "诊断中…" : "运行诊断";
  let resultHtml = "";
  if (spikeDiagnose) {
    const r = spikeDiagnose;
    resultHtml = `
      <div class="spike-diag-result ${r.ok ? "ok" : "fail"}">
        <div>${r.ok ? "✓ 诊断通过" : "✗ 诊断失败"}（阶段：${escapeHtml(r.phase)}）</div>
        <div>握手：${r.handshakeCompleted ? "✓ 完成" : "✗ 未完成"}</div>
        <div>ping：${r.pingStatus !== null ? `HTTP ${r.pingStatus}` : "—"}</div>
        <div>CVM：${r.cvmDetected ? "已检测到" : "未检测到"}</div>
        ${r.error ? `<div>错误：${escapeHtml(r.error)}</div>` : ""}
      </div>`;
  } else if (spikeDiagnoseError) {
    resultHtml = `<div class="spike-diag-result fail">错误：${escapeHtml(spikeDiagnoseError)}</div>`;
  }
  let sendHtml = "";
  if (spikeSend) {
    const r = spikeSend;
    const ackId =
      r.ack && typeof r.ack === "object" && "message_id" in (r.ack as Record<string, unknown>)
        ? String((r.ack as Record<string, unknown>).message_id)
        : "—";
    sendHtml = `
      <div class="spike-diag-result ${r.ok ? "ok" : "fail"}">
        <div>${r.ok ? "✓ 已发送并收到 ack" : "✗ 发送未确认"}（${escapeHtml(r.status)}）</div>
        <div>sendId：${escapeHtml(r.sendId)}　message_id：${escapeHtml(ackId)}</div>
        ${r.sessionId ? `<div>会话：${escapeHtml(r.sessionId)}</div>` : ""}
        ${r.error ? `<div>说明：${escapeHtml(r.error)}</div>` : ""}
      </div>`;
  } else if (spikeSendError) {
    sendHtml = `<div class="spike-diag-result fail">错误：${escapeHtml(spikeSendError)}</div>`;
  }
  let quotaHtml = "";
  if (spikeQuota) {
    const q = spikeQuota;
    quotaHtml = `
      <div class="spike-diag-result ok">
        <div>✓ 额度：${escapeHtml(q.tier || "—")}　已用 ${q.percentUsed}%　状态 ${escapeHtml(q.state || "—")}</div>
        <div>重置：${q.resetsAtMs ? new Date(q.resetsAtMs).toLocaleString() : "—"}　来源：${escapeHtml(q.source)}</div>
      </div>`;
  } else if (spikeQuotaError) {
    quotaHtml = `<div class="spike-diag-result fail">错误：${escapeHtml(spikeQuotaError)}</div>`;
  }
  let approvalsHtml = "";
  if (spikeApprovals) {
    const a = spikeApprovals;
    const f = a.first;
    approvalsHtml = `
      <div class="spike-diag-result ${a.ok ? "ok" : "fail"}">
        <div>${a.ok ? "✓ 审批只读查询成功" : "✗ 审批查询失败"}（HTTP ${a.status ?? "—"}，待审批 ${a.count} 条）</div>
        ${f ? `<div>最新：${escapeHtml(f.displayName || "—")} / ${escapeHtml(f.kind || "—")} / ${escapeHtml(f.status)}${f.expiresAtMs ? ` / 到期 ${new Date(f.expiresAtMs).toLocaleTimeString()}` : ""}</div>` : ""}
        ${a.error ? `<div>错误：${escapeHtml(a.error)}</div>` : ""}
      </div>`;
  } else if (spikeApprovalsError) {
    approvalsHtml = `<div class="spike-diag-result fail">错误：${escapeHtml(spikeApprovalsError)}</div>`;
  }
  let watchHtml = "";
  if (spikeWatch) {
    const s = spikeWatch.summary;
    watchHtml = `
      <div class="spike-diag-result ${spikeWatch.ok ? "ok" : "fail"}">
        <div>${spikeWatch.ok ? "✓ 订阅正常" : "✗ 订阅异常"}（${spikeWatch.seconds}s，HTTP ${spikeWatch.status ?? "—"}）</div>
        <div>账号：${escapeHtml(spikeWatchAccount ?? "—")}（优先当前活跃账号）</div>
        <div>事件 ${s.events}：开始 ${s.turnsStarted}，消息 ${s.messagesStarted}，增量 ${s.textDeltas}，完成 ${s.messagesDone}</div>
        <div>忙碌 ${s.busy}，空闲 ${s.idle}，重复 ${s.duplicates}，忽略 ${s.ignored}</div>
        <div>原始：${spikeWatch.lines ?? 0} 行（非 event ${spikeWatch.nonEvent ?? 0}，坏行 ${spikeWatch.parseErrors ?? 0}）　类型：${escapeHtml(
          Object.entries(spikeWatch.typeCounts ?? {})
            .map(([k, v]) => `${k}×${v}`)
            .join("，") || "—"
        )}</div>
        <div>事件名：${escapeHtml(
          Object.entries(s.eventsByName ?? {})
            .sort((a, b) => b[1] - a[1])
            .slice(0, 5)
            .map(([k, v]) => `${k}×${v}`)
            .join("，") || "—"
        )}</div>
        ${spikeWatch.error ? `<div>错误：${escapeHtml(spikeWatch.error)}</div>` : ""}
      </div>`;
  } else if (spikeWatchError) {
    watchHtml = `<div class="spike-diag-result fail">错误：${escapeHtml(spikeWatchError)}</div>`;
  }
  return `
    <div class="spike-diag-header">
      <span class="spike-diag-title">原生诊断（Spike）</span>
      <button id="spike-diagnose-btn" class="mini-btn" type="button" ${spikeDiagnoseRunning ? "disabled" : ""}>${btnLabel}</button>
      <button id="spike-watch-btn" class="mini-btn" type="button" ${spikeWatchRunning ? "disabled" : ""}>${spikeWatchRunning ? "订阅中…" : "订阅探针 15s"}</button>
      <button id="spike-approvals-btn" class="mini-btn" type="button" ${spikeApprovalsRunning ? "disabled" : ""}>${spikeApprovalsRunning ? "查询中…" : "审批探针"}</button>
      <button id="spike-quota-btn" class="mini-btn" type="button" ${spikeQuotaRunning ? "disabled" : ""}>${spikeQuotaRunning ? "查询中…" : "额度探针"}</button>
      <button id="spike-send-btn" class="mini-btn" type="button" ${spikeSendRunning ? "disabled" : ""}>${spikeSendRunning ? "发送中…" : "发送探针"}</button>
      <button id="spike-copy-btn" class="mini-btn" type="button">复制结果</button>
    </div>
    ${resultHtml}
    ${watchHtml}
    ${approvalsHtml}
    ${quotaHtml}
    ${sendHtml}`;
}

function syncSpikeDiag(): void {
  const el = document.querySelector<HTMLElement>(".spike-diag");
  if (!el) return;
  el.innerHTML = spikeDiagInnerHtml();
}

function pickWatchAccount(): AccountRecord | undefined {
  if (!snapshot) return undefined;
  const active = snapshot.activeAccount;
  if (active) {
    const a = snapshot.accounts.find((x) => isSameKey(active, x));
    if (a && a.providerId === "muse" && a.authState === "authenticated") return a;
  }
  return snapshot.accounts.find(
    (a) => a.providerId === "muse" && a.authState === "authenticated"
  );
}

async function runSpikeSend(): Promise<void> {
  if (!snapshot || spikeSendRunning) return;
  const acc = pickWatchAccount();
  if (!acc) {
    spikeSend = null;
    spikeSendError = "没有已登录的 Muse 账号。";
    syncSpikeDiag();
    return;
  }
  spikeSendRunning = true;
  spikeSendError = null;
  syncSpikeDiag();
  try {
    // 隔离测试会话：内容是无语义整数，不含隐私，也不发往用户主聊天。
    const probeText = `[spike b4] ${Math.floor(Math.random() * 1e6)}`;
    spikeSend = await Promise.race([
      invoke<SpikeSendResult>("muse_native_send_probe", {
        providerId: "muse",
        uuid: acc.uuid,
        text: probeText,
        nodeId: "",
        sessionId: null,
      }),
      new Promise<SpikeSendResult>((_, reject) =>
        setTimeout(() => reject(new Error("发送探针超时（60 秒）")), 60_000)
      ),
    ]);
  } catch (e) {
    spikeSend = null;
    spikeSendError = String(e);
  }
  spikeSendRunning = false;
  syncSpikeDiag();
}

async function runSpikeQuota(): Promise<void> {
  if (!snapshot || spikeQuotaRunning) return;
  const acc = pickWatchAccount();
  if (!acc) {
    spikeQuota = null;
    spikeQuotaError = "没有已登录的 Muse 账号。";
    syncSpikeDiag();
    return;
  }
  spikeQuotaRunning = true;
  spikeQuotaError = null;
  syncSpikeDiag();
  try {
    spikeQuota = await Promise.race([
      invoke<SpikeQuota>("muse_native_quota_probe", { providerId: "muse", uuid: acc.uuid }),
      new Promise<SpikeQuota>((_, reject) =>
        setTimeout(() => reject(new Error("额度探针超时（60 秒）")), 60_000)
      ),
    ]);
  } catch (e) {
    spikeQuota = null;
    spikeQuotaError = String(e);
  }
  spikeQuotaRunning = false;
  syncSpikeDiag();
}

async function runSpikeApprovals(): Promise<void> {
  if (!snapshot || spikeApprovalsRunning) return;
  const acc = pickWatchAccount();
  if (!acc) {
    spikeApprovals = null;
    spikeApprovalsError = "没有已登录的 Muse 账号。";
    syncSpikeDiag();
    return;
  }
  spikeApprovalsRunning = true;
  spikeApprovalsError = null;
  syncSpikeDiag();
  try {
    spikeApprovals = await Promise.race([
      invoke<SpikeApprovalsResult>("muse_native_approvals_probe", {
        providerId: "muse",
        uuid: acc.uuid,
      }),
      new Promise<SpikeApprovalsResult>((_, reject) =>
        setTimeout(() => reject(new Error("审批探针超时（45 秒）")), 45_000)
      ),
    ]);
  } catch (e) {
    spikeApprovals = null;
    spikeApprovalsError = String(e);
  }
  spikeApprovalsRunning = false;
  syncSpikeDiag();
}

async function runSpikeWatch(): Promise<void> {
  if (!snapshot || spikeWatchRunning) return;
  const acc = pickWatchAccount();
  spikeWatchAccount = acc?.alias ?? null;
  if (!acc) {
    spikeWatch = null;
    spikeWatchError = "没有已登录的 Muse 账号，请先登录一个测试账号。";
    syncSpikeDiag();
    return;
  }
  spikeWatchRunning = true;
  spikeWatchError = null;
  syncSpikeDiag();
  try {
    spikeWatch = await Promise.race([
      invoke<SpikeWatchResult>("muse_native_watch_probe", {
        providerId: "muse",
        uuid: acc.uuid,
        seconds: 15,
      }),
      new Promise<SpikeWatchResult>((_, reject) =>
        setTimeout(() => reject(new Error("订阅探针超时（75 秒）")), 75_000)
      ),
    ]);
  } catch (e) {
    spikeWatch = null;
    spikeWatchError = String(e);
  }
  spikeWatchRunning = false;
  syncSpikeDiag();
}

async function runSpikeDiagnose(): Promise<void> {
  if (!snapshot || spikeDiagnoseRunning) return;
  const acc = snapshot.accounts.find(
    (a) => a.providerId === "muse" && a.authState === "authenticated"
  );
  if (!acc) {
    spikeDiagnose = null;
    spikeDiagnoseError = "没有已登录的 Muse 账号，请先登录一个测试账号。";
    syncSpikeDiag();
    return;
  }
  spikeDiagnoseRunning = true;
  spikeDiagnoseError = null;
  syncSpikeDiag();
  try {
    // 前端超时 70 秒：Rust 侧已有 60 秒整体超时，但如果 Rust 命令本身没返回
    // （如 Tauri 桥挂住），前端也必须给用户一个明确结果，不能一直转圈。
    // 70 秒 > Rust 60 秒，让 Rust 的超时结果优先返回。
    spikeDiagnose = await Promise.race([
      invoke<SpikeDiagnoseResult>("muse_native_diagnose", {
        providerId: "muse",
        uuid: acc.uuid,
      }),
      new Promise<never>((_, reject) =>
        setTimeout(
          () =>
            reject(
              new Error(
                "前端超时（70 秒）：诊断命令没有返回。Rust 侧的 60 秒超时也没触发，可能是 Tauri 桥或运行时卡住了，请重启 App 再试。"
              )
            ),
          70_000
        )
      ),
    ]);
  } catch (e) {
    spikeDiagnose = null;
    spikeDiagnoseError = String(e);
  }
  spikeDiagnoseRunning = false;
  syncSpikeDiag();
}

function currentProvider(state: ManagerSnapshot): ProviderInfo {
  return (
    state.providers.find((p) => p.id === state.selectedProviderId) ??
    state.providers[0]
  );
}

function renderUpdateModal(): string {
  const res = updateState.result;
  const curVer = res ? res.currentVersion : "...";
  const hasUpdate = Boolean(res?.hasUpdate);
  const title = hasUpdate
    ? `发现新版本 v${escapeHtml(res!.latestVersion)}`
    : "检查更新";

  const versionRow = res
    ? `
      <div class="update-version-row">
        <span class="update-version-pill">当前 v${escapeHtml(curVer)}</span>
        ${
          hasUpdate
            ? `<span>→</span><span class="update-version-pill latest">最新 v${escapeHtml(res.latestVersion)}</span>`
            : ""
        }
        ${res.size ? `<span class="update-size-label">${escapeHtml(formatBytes(res.size))}</span>` : ""}
      </div>
    `
    : `<div class="modal-desc">正在连接 GitHub Releases 检查版本信息...</div>`;

  let bodyHtml = "";
  if (updateState.checking && !res) {
    bodyHtml = `
      <div class="update-progress-wrap">
        <div class="update-progress-track"><div class="update-progress-fill indeterminate"></div></div>
      </div>
    `;
  } else if (res) {
    const noteText = res.releaseNotes.trim();
    if (hasUpdate) {
      bodyHtml += `<div class="update-notes">${escapeHtml(noteText || "包含稳定性改进与问题修复。")}</div>`;
      if (!res.downloadUrl) {
        bodyHtml += `<div class="modal-desc">当前发布尚未包含本平台自动安装包，请点击「发布页」手动下载。</div>`;
      }
    } else {
      bodyHtml += `<div class="modal-desc">${escapeHtml(noteText || "当前已是最新版本。")}</div>`;
    }
  }

  const prog = updateState.progress;
  const showProgress =
    prog &&
    prog.stage !== "error" &&
    (updateState.installing ||
      updateState.outcomeStatus === "restarting" ||
      prog.stage === "ready");
  if (showProgress) {
    lastRenderedUpdateStage = prog.stage;
    const isIndeterminate = prog.percent === null;
    const widthStyle = isIndeterminate ? "" : `style="width:${Math.max(0, Math.min(100, prog.percent!))}%"`;
    bodyHtml += `
      <div class="update-progress-wrap">
        <div class="update-progress-meta">
          <span id="update-progress-text">${escapeHtml(prog.message)}</span>
          <span id="update-progress-pct" class="update-progress-pct">${escapeHtml(formatProgressSummary(prog))}</span>
        </div>
        <div class="update-progress-track">
          <div id="update-progress-bar" class="update-progress-fill ${isIndeterminate ? "indeterminate" : ""}" ${widthStyle}></div>
        </div>
      </div>
    `;
  }

  if (updateState.outcomeMessage) {
    bodyHtml += `<div class="update-outcome-box">${escapeHtml(updateState.outcomeMessage)}</div>`;
  }
  if (updateState.error) {
    bodyHtml += `<div class="update-error-box">${escapeHtml(updateState.error)}</div>`;
  }

  const canInstall = hasUpdate && Boolean(res?.downloadUrl);
  const disablePrimary =
    updateState.checking ||
    updateState.installing ||
    updateState.outcomeStatus === "restarting";
  // iOS has no in-app install: the update flow ends at the release page,
  // where the user downloads the new IPA and re-sideloads it. The Rust
  // updater returns outcome status "manual_install" for the same reason.
  const manualInstallFlow =
    snapshot?.platform === "ios" || updateState.outcomeStatus === "manual_install";
  const primaryBtn = canInstall
    ? manualInstallFlow
      ? `<button id="update-manual-install-btn" class="btn primary" type="button" ${disablePrimary ? "disabled" : ""}>去下载 IPA</button>`
      : `<button id="update-install-btn" class="btn primary" type="button" ${disablePrimary ? "disabled" : ""}>${
          updateState.installing
            ? "更新中..."
            : updateState.outcomeStatus === "needs_permission"
              ? "已授权，继续安装"
              : "立即下载并更新"
        }</button>`
    : `<button id="update-recheck-btn" class="btn primary" type="button" ${disablePrimary ? "disabled" : ""}>${
        updateState.checking ? "检查中..." : "重新检查"
      }</button>`;

  const pageBtn = res?.releasePageUrl
    ? `<button id="update-open-page-btn" class="btn" type="button">发布页</button>`
    : "";

  return `
    <div class="inline-modal" role="dialog" aria-modal="true">
      <div class="modal-card">
        <div class="modal-title">${title}</div>
        ${versionRow}
        ${bodyHtml}
        <div class="modal-footer">
          <button id="modal-cancel-btn" class="btn" type="button" ${updateState.installing ? "disabled" : ""}>关闭</button>
          ${pageBtn}
          ${primaryBtn}
        </div>
      </div>
    </div>
  `;
}

function renderModal(provider: ProviderInfo): string {
  if (modal.kind === "none") return "";
  if (modal.kind === "update") {
    return renderUpdateModal();
  }
  if (modal.kind === "add") {
    return `
      <div class="inline-modal" role="dialog" aria-modal="true">
        <div class="modal-card">
          <div class="modal-title">新增 ${escapeHtml(provider.shortName)} 账号</div>
          <div class="modal-desc">每个账号拥有独立隔离的登录态。备注可留空，登录成功后将自动显示邮箱名称。</div>
          <input id="modal-alias-input" class="modal-input" maxlength="60" placeholder="备注名称（留空则自动获取登录邮箱）" value="${escapeHtml(modal.draft)}" />
          <div class="modal-footer">
            <button id="modal-cancel-btn" class="btn" type="button">取消</button>
            <button id="modal-confirm-btn" class="btn primary" type="button">创建并打开</button>
          </div>
        </div>
      </div>
    `;
  }
  if (modal.kind === "rename") {
    return `
      <div class="inline-modal" role="dialog" aria-modal="true">
        <div class="modal-card">
          <div class="modal-title">修改备注名称</div>
          <div class="modal-desc">仅更新本机列表中的显示名称，不影响已登录的官方会话。</div>
          <input id="modal-alias-input" class="modal-input" maxlength="60" placeholder="输入新的备注名称" value="${escapeHtml(modal.draft)}" />
          <div class="modal-footer">
            <button id="modal-cancel-btn" class="btn" type="button">取消</button>
            <button id="modal-confirm-btn" class="btn primary" type="button">保存</button>
          </div>
        </div>
      </div>
    `;
  }
  return `
    <div class="inline-modal" role="dialog" aria-modal="true">
      <div class="modal-card">
        <div class="modal-title">移除「${escapeHtml(modal.account.alias)}」</div>
        <div class="modal-desc">将关闭该视图并彻底清除本机独立浏览资料（Cookie 与存储）。云端账号及历史对话仍由官方保留。</div>
        <div class="modal-footer">
          <button id="modal-cancel-btn" class="btn" type="button">取消</button>
          <button id="modal-confirm-btn" class="btn danger" type="button">确认移除</button>
        </div>
      </div>
    </div>
  `;
}

function renderMiddleContent(state: ManagerSnapshot, provider: ProviderInfo): string {
  const providerAccounts = state.accounts.filter((a) => a.providerId === provider.id);
  const switchingKey = effectiveSwitchingKey(state);
  const isMobile = isMobilePlatform(state.platform);

  const noticeHtml = state.notice
    ? `
      <div class="notice-bar">
        <span>${escapeHtml(state.notice)}</span>
        <button id="dismiss-notice-btn" class="notice-close" type="button" title="关闭提示">✕</button>
      </div>
    `
    : "";

  const conflictsHtml = state.conflicts
    .map(
      (c) => `
      <div class="conflict-box">
        <div>检测到 UUID 归属冲突（${escapeHtml(c.uuid.slice(0, 8))}…），请选择保留归属服务：</div>
        <div class="conflict-actions">
          ${c.providers
            .map(
              (pid) =>
                `<button class="mini-btn" data-resolve-uuid="${escapeHtml(c.uuid)}" data-resolve-provider="${escapeHtml(pid)}" type="button">归属 ${escapeHtml(pid.toUpperCase())}</button>`
            )
            .join("")}
        </div>
      </div>
    `
    )
    .join("");

  const hasUpdate = Boolean(updateState.result?.hasUpdate);
  const mobileUpdateBtn = isMobile
    ? `<button id="check-update-btn" class="ghost-icon-btn update-trigger-btn ${hasUpdate ? "has-update" : ""}" type="button" title="${hasUpdate ? `发现新版本 v${escapeHtml(updateState.result!.latestVersion)}` : "检查更新"}">${ICON_UPDATE}${hasUpdate ? `<span class="update-dot"></span>` : ""}</button>`
    : "";

  // Batch-1 reorder mode: the header shows only a Done button.
  const inReorder = reorderMode && reorderProviderId === provider.id;
  const headerBtnHtml = inReorder
    ? `<button id="reorder-done-btn" class="icon-btn" type="button">完成</button>`
    : provider.enabled
      ? `
      ${mobileUpdateBtn}
      ${!isMobile ? `<button id="sync-all-btn" class="ghost-icon-btn" type="button" title="刷新已就绪账号额度">${ICON_REFRESH}</button>` : ""}
      <button id="open-site-btn" class="ghost-icon-btn" type="button" title="在浏览器中打开 ${escapeHtml(provider.shortName)} 官网">${ICON_EXTERNAL}</button>
      <button id="add-account-btn" class="icon-btn primary" type="button">+ 新增</button>
    `
      : `${mobileUpdateBtn}<button id="open-site-btn" class="icon-btn primary" type="button">打开官网 ${ICON_EXTERNAL}</button>`;

  let listBody = "";
  if (!provider.enabled) {
    listBody = `
      <div class="empty-list">
        <div class="empty-icon-circle">${renderProviderLogo(provider.id)}</div>
        <div><strong>${escapeHtml(provider.name)}</strong> 暂未开放内嵌会话</div>
        <div>待核验官方网页直登路径与嵌入条款后启用，可点击右上角访问官网。</div>
      </div>
    `;
  } else if (providerAccounts.length === 0) {
    listBody = `
      <div class="empty-list">
        <div class="empty-icon-circle">${ICON_USER_PLUS}</div>
        <div>暂无 ${escapeHtml(provider.shortName)} 账号</div>
        <div>点击右上角「+ 新增」创建独立登录环境</div>
      </div>
    `;
  } else {
    listBody = providerAccounts
      .map((acc, idx) => {
        const d = deriveRowState(acc, state, switchingKey);
        const isMac = navigator.userAgent.includes("Mac");
        const shortcutHint =
          !isMobile && idx < 9
            ? `<span class="shortcut-pill" title="快捷切号">${isMac ? "⌘" : "^"}${idx + 1}</span>`
            : "";
        return `
          <div class="${rowClassFor(d)}" data-switch-uuid="${escapeHtml(acc.uuid)}">
            <div class="account-header">
              <span class="drag-handle" title="拖动排序">${ICON_DRAG}</span>
              <div class="avatar-wrap">
                ${avatarWrapHtml(acc, d)}
              </div>
              <div class="account-meta">
                <div class="account-alias-row">
                  <span class="account-alias">${escapeHtml(acc.alias)}</span>
                  ${shortcutHint}
                  ${alertBadgeHtml(d)}
                </div>
                ${subHtmlFor(acc, d, isMobile)}
              </div>
              <div class="row-ops">
                <button class="action-icon-btn" data-rename-uuid="${escapeHtml(acc.uuid)}" title="修改备注" type="button">${ICON_EDIT}</button>
                <button class="action-icon-btn danger" data-remove-uuid="${escapeHtml(acc.uuid)}" title="从本机移除" type="button">${ICON_TRASH}</button>
              </div>
            </div>
            ${!d.isLoggedOut && acc.quotaDetail ? renderQuotaDetailPanel(acc.quotaDetail, d.alertStatus, d.extraDepleted) : ""}
          </div>
        `;
      })
      .join("");
  }

  // Spike 原生诊断区：Muse 页显示（本分支即 Spike 分支）。
  const spikeDiagHtml =
    provider.id === "muse"
      ? `<div class="spike-diag">${spikeDiagInnerHtml()}</div>`
      : "";

  // Spike 顶栏标识（0.1.22）：只在 Spike 版显示（bundle id 门控）。
  const spikeBadgeHtml =
    isSpikeBuild && provider.id === "muse"
      ? `<span class="spike-badge" title="Spike 版">Spike</span>`
      : "";

  return `
    <div class="column-header">
      <div class="provider-title-wrap">
        <span class="provider-title">${escapeHtml(provider.name)}</span>
        ${spikeBadgeHtml}
        ${
          !provider.enabled
            ? `<span class="status-pill">${escapeHtml(provider.statusText)}</span>`
            : providerAccounts.length > 0
              ? `<span class="count-badge">${providerAccounts.length}</span>`
              : ""
        }
      </div>
      <div class="header-actions">${headerBtnHtml}</div>
    </div>
    ${noticeHtml}
    ${conflictsHtml}
    <div class="account-list${inReorder ? " reorder-mode" : ""}">${listBody}</div>
    ${spikeDiagHtml}
    <!-- 0.1.21: the modal lives in its own overlay container and is
         filled by renderModalRoot(), not by this template — opening or
         closing it no longer rebuilds #app (see structuralSignature). -->
    <div id="modal-root"></div>
  `;
}

function renderRightStage(state: ManagerSnapshot, provider: ProviderInfo): string {
  const switchingKey = effectiveSwitchingKey(state);
  const switchingAcc = switchingKey
    ? state.accounts.find(
        (a) => a.providerId === switchingKey.providerId && a.uuid === switchingKey.uuid
      )
    : undefined;

  if (switchingKey) {
    const targetName = switchingAcc?.alias ?? provider.shortName;
    return `
      <main class="right-placeholder">
        <div class="stage-card">
          <div class="stage-logo-orbit">
            <span class="stage-orbit-ring"></span>
            <div class="stage-logo-tile">${renderProviderLogo(provider.id)}</div>
          </div>
          <div class="placeholder-title">正在加载「${escapeHtml(targetName)}」</div>
          <div class="placeholder-desc">首次打开正在渲染 ${escapeHtml(provider.shortName)} 页面，加载完成后再次切换将瞬间直达…</div>
          <div class="progress-track"><div class="progress-bar"></div></div>
        </div>
      </main>
    `;
  }

  if (!provider.enabled) {
    return `
      <main class="right-placeholder">
        <div class="stage-card">
          <div class="stage-logo-tile">${renderProviderLogo(provider.id)}</div>
          <div class="placeholder-title">${escapeHtml(provider.name)} · 待接入</div>
          <div class="placeholder-desc">首版暂未启用应用内嵌会话。访问官网不会覆盖上次已打开的账号记录。</div>
          <button id="stage-open-site-btn" class="btn primary" type="button">打开 ${escapeHtml(provider.shortName)} 官网</button>
        </div>
      </main>
    `;
  }

  return `
    <main class="right-placeholder">
      <div class="stage-card">
        <div class="stage-logo-tile">${renderProviderLogo(provider.id)}</div>
        <div class="placeholder-title">${escapeHtml(provider.name)}</div>
        <div class="placeholder-desc">从左侧选择一个账号进入对话，或新建账号开启独立隔离登录。</div>
        <button id="stage-add-btn" class="btn primary" type="button">+ 新增 ${escapeHtml(provider.shortName)} 账号</button>
      </div>
    </main>
  `;
}

// 0.1.9 incremental rendering.
//
// Every snapshot used to rebuild #app from scratch (innerHTML +
// re-binding every listener). The 4.5s quota probes only change field
// values, so render() now compares a structural signature first:
//   - signature unchanged -> patchRows(): update only the rows whose
//     derived state changed, in place. Row nodes survive, so scroll,
//     focus (typing in the rename modal), and an in-progress drag are
//     never disturbed.
//   - signature changed -> full render as before (add/remove account,
//     provider switch, notice, conflicts, switching target,
//     reorder mode).
// 0.1.21: modal open/close and update-check state are no longer in the
// signature — the modal renders into #modal-root on its own and the
// update button is synced in place (syncUpdateButton), so neither
// rebuilds the list or disturbs scroll / drag / modal input focus.
// Anything that changes DOM structure must be in the signature; anything
// patchable must NOT be (alias, quota text/percent, avatar, auth state,
// active/switching flags — except the switching target itself, which
// drives the desktop stage).
function structuralSignature(state: ManagerSnapshot, provider: ProviderInfo): string {
  const switchingKey = effectiveSwitchingKey(state);
  const providerAccounts = state.accounts.filter(
    (a) => a.providerId === provider.id
  );
  const switchingAcc = switchingKey
    ? state.accounts.find((a) => isSameKey(switchingKey, a))
    : undefined;
  const parts = [
    state.platform,
    state.selectedProviderId,
    state.providers
      .map(
        (p) =>
          `${p.id}:${p.enabled ? 1 : 0}:${p.name}:${p.shortName}:${p.statusText}`
      )
      .join(","),
    providerAccounts.map((a) => a.uuid).join(","),
    // 0.1.21: hasUnreadReply is NOT here — it already lives in
    // avatarSigFor, so patchRows updates the unread dot in place
    // instead of rebuilding the whole list when a reply arrives.
    providerAccounts
      .map(
        (a) =>
          `${a.uuid}:${a.authState === "logged_out" ? 0 : a.quotaDetail ? 1 : 0}`
      )
      .join(","),
    switchingKey
      ? `${switchingKey.providerId}:${switchingKey.uuid}:${switchingAcc?.alias ?? ""}`
      : "",
    // 0.1.21: modal.kind / modal.account are NOT here — the modal is
    // rendered separately into #modal-root (renderModalRoot), so
    // opening or closing it leaves the list untouched.
    state.notice ?? "",
    state.conflicts
      .map((c) => `${c.uuid}:${c.providers.join("+")}`)
      .join(","),
    reorderMode ? `reorder:${reorderProviderId ?? ""}` : "",
    // 0.1.21: updateState is NOT here either — its only #app consumer
    // besides the (now separate) update modal is the check-update
    // button, which syncUpdateButton() patches in place. A silent
    // update check therefore no longer rebuilds the list either.
  ];
  return parts.join("\n");
}

// 0.1.21: render the modal into its own overlay container (#modal-root,
// inside the middle column, so the existing absolute positioning and
// .mobile-shell overrides apply unchanged), independently of #app's
// innerHTML. The HTML is only written when the modal's identity changes,
// so typing in the rename/add input (draft lives in `modal`, and the
// live input node already shows it) and the update-progress in-place
// patching are never disturbed by a rewrite. Called from render() on
// both the patch and the full paths.
function modalIdentityKey(provider: ProviderInfo): string | null {
  // Deliberately excludes modal.draft: a draft change is a keystroke,
  // not a new modal — the input's value is already up to date in the
  // DOM, and rewriting #modal-root here would replace the focused
  // input node (regression vs 0.1.20, where the patch path never
  // touched the modal). The opening draft is still rendered: opening
  // the modal changes kind from "none", so the key changes and the
  // HTML is written then.
  switch (modal.kind) {
    case "none":
      return "none";
    case "add":
      return `add:${modal.providerId}:${provider.shortName}`;
    case "rename":
      return `rename:${modal.account.uuid}`;
    case "remove":
      return `remove:${modal.account.uuid}:${modal.account.alias}`;
    case "update":
      // Dynamic content (update check / install progress): no stable
      // identity — the caller falls back to the full HTML.
      return null;
  }
}

function renderModalRoot(provider: ProviderInfo): void {
  const mount = document.querySelector<HTMLElement>("#modal-root");
  if (!mount) return;
  const html = renderModal(provider);
  const key = modalIdentityKey(provider) ?? html;
  if (mount === lastModalMount && key === lastModalKey) return;
  lastModalMount = mount;
  lastModalKey = key;
  mount.innerHTML = html;
}

// 0.1.21: updateState left the structural signature with the modal, so
// a silent update check must not rebuild the list either. The
// check-update button is the one #app element that reads updateState —
// sync it in place here, on the patch path. (The full path rebuilds it
// from the template as before.)
function syncUpdateButton(): void {
  const btn = document.querySelector<HTMLElement>("#check-update-btn");
  if (!btn) return;
  const hasUpdate = Boolean(updateState.result?.hasUpdate);
  btn.classList.toggle("has-update", hasUpdate);
  btn.title = hasUpdate
    ? `发现新版本 v${updateState.result!.latestVersion}`
    : "检查更新";
  const dot = btn.querySelector<HTMLElement>(".update-dot");
  if (hasUpdate && !dot) {
    btn.insertAdjacentHTML("beforeend", `<span class="update-dot"></span>`);
  } else if (!hasUpdate && dot) {
    dot.remove();
  }
}

function seedPatchState(state: ManagerSnapshot, provider: ProviderInfo): void {
  patchState.clear();
  const switchingKey = effectiveSwitchingKey(state);
  for (const acc of state.accounts.filter(
    (a) => a.providerId === provider.id
  )) {
    const d = deriveRowState(acc, state, switchingKey);
    patchState.set(acc.uuid, {
      avatarSig: avatarSigFor(acc, d),
      subVariant: d.subVariant,
      detailHtml:
        !d.isLoggedOut && acc.quotaDetail
          ? renderQuotaDetailPanel(acc.quotaDetail, d.alertStatus, d.extraDepleted)
          : "",
    });
  }
}

function patchRows(state: ManagerSnapshot, provider: ProviderInfo): void {
  const listEl = document.querySelector<HTMLElement>(".account-list");
  if (!listEl) return;
  const switchingKey = effectiveSwitchingKey(state);
  for (const acc of state.accounts.filter(
    (a) => a.providerId === provider.id
  )) {
    const row = listEl.querySelector<HTMLElement>(
      `[data-switch-uuid="${CSS.escape(acc.uuid)}"]`
    );
    if (!row) continue;
    // The row under the finger keeps its dragged position and .dragging
    // class; the drag-end handler re-renders with the latest snapshot.
    if (row === dragRow) continue;
    patchRow(row, acc, state, switchingKey);
  }
}

function patchRow(
  row: HTMLElement,
  acc: AccountRecord,
  state: ManagerSnapshot,
  switchingKey: AccountKey | null
): void {
  const d = deriveRowState(acc, state, switchingKey);
  const isMobile = isMobilePlatform(state.platform);
  const prev = patchState.get(acc.uuid);

  // Row classes (active / switching / logged-out / alert / has-detail).
  const cls = rowClassFor(d);
  if (row.className !== cls) row.className = cls;

  // Avatar block (photo, initial, spinner, status dot).
  const avatarSig = avatarSigFor(acc, d);
  if (!prev || prev.avatarSig !== avatarSig) {
    const wrap = row.querySelector<HTMLElement>(".avatar-wrap");
    if (wrap) wrap.innerHTML = avatarWrapHtml(acc, d);
  }

  // Alias text.
  const aliasEl = row.querySelector<HTMLElement>(".account-alias");
  if (aliasEl && aliasEl.textContent !== acc.alias) {
    aliasEl.textContent = acc.alias;
  }

  // Alert badge (auth-status / quota-warning), last child of the alias row.
  const aliasRow = row.querySelector<HTMLElement>(".account-alias-row");
  const desiredBadge = alertBadgeHtml(d);
  const currentBadge = aliasRow?.querySelector<HTMLElement>(
    ".quota-warning-badge, .auth-status-badge"
  );
  if (desiredBadge === "") {
    currentBadge?.remove();
  } else if (!currentBadge || currentBadge.outerHTML !== desiredBadge) {
    currentBadge?.remove();
    aliasRow?.insertAdjacentHTML("beforeend", desiredBadge);
  }

  // Quota sub-line. Its existence depends only on hasDetail (in the
  // signature), so within a patch it can only change variant or values.
  const sub = row.querySelector<HTMLElement>(".account-sub");
  if (d.hasDetail) {
    sub?.remove();
  } else if (!prev || prev.subVariant !== d.subVariant) {
    const html = subHtmlFor(acc, d, isMobile);
    if (sub) {
      sub.outerHTML = html;
    } else {
      aliasRow?.insertAdjacentHTML("afterend", html);
    }
  } else if (sub) {
    if (d.subVariant === "quota") {
      const subCls = d.isEmpty
        ? "account-sub quota-empty"
        : d.isLow
          ? "account-sub quota-low"
          : "account-sub";
      if (sub.className !== subCls) sub.className = subCls;
      const fill = sub.querySelector<HTMLElement>(".mini-quota-fill");
      const needsBar = typeof acc.quotaPercent === "number";
      if (needsBar !== Boolean(fill)) {
        // Bar appeared/disappeared: rebuild the sub line.
        sub.outerHTML = subHtmlFor(acc, d, isMobile);
      } else {
        if (fill && needsBar) {
          const w = `${Math.max(0, Math.min(100, acc.quotaPercent!))}%`;
          if (fill.style.width !== w) fill.style.width = w;
          fill.classList.toggle("empty", d.isEmpty);
          fill.classList.toggle("low", d.isLow);
        }
        const labelEl = sub.querySelector<HTMLElement>(".quota-label-text");
        if (labelEl && labelEl.textContent !== d.quotaLabel) {
          labelEl.textContent = d.quotaLabel;
        }
      }
    } else if (d.subVariant === "logged-out") {
      const labelEl = sub.querySelector<HTMLElement>(".quota-label-text");
      const hint = loginHintFor(acc, d, isMobile);
      if (labelEl && labelEl.textContent !== hint) {
        labelEl.textContent = hint;
      }
    }
    // "loading" has static text; nothing to patch.
  }

  // Quota detail panel: no wrapper element (two sibling nodes after the
  // header), so swap the subtree when its rendered HTML changes. No
  // listeners or images live inside it.
  const detailHtml =
    !d.isLoggedOut && acc.quotaDetail
      ? renderQuotaDetailPanel(acc.quotaDetail, d.alertStatus, d.extraDepleted)
      : "";
  if (!prev || prev.detailHtml !== detailHtml) {
    row
      .querySelectorAll(".quota-compact-main, .quota-compact-meta")
      .forEach((el) => el.remove());
    if (detailHtml) {
      row
        .querySelector<HTMLElement>(".account-header")
        ?.insertAdjacentHTML("afterend", detailHtml);
    }
  }

  patchState.set(acc.uuid, {
    avatarSig,
    subVariant: d.subVariant,
    detailHtml,
  });
}

function render(): void {
  const root = document.querySelector<HTMLElement>("#app");
  if (!root || !snapshot) return;

  const provider = currentProvider(snapshot);
  const sig = structuralSignature(snapshot, provider);
  if (sig === lastStructuralSig) {
    // Field-only change: patch in place. Safe mid-drag (patchRow skips
    // the dragging row), so quota updates keep flowing during a drag.
    // 0.1.21: the modal and the update button are synced here too —
    // modal open/close and update checks take this path, so the list
    // is never rebuilt for them.
    patchRows(snapshot, provider);
    renderModalRoot(provider);
    syncUpdateButton();
    return;
  }
  // A structural change arriving mid-drag must not rebuild the list
  // under the finger; the drag-end handler re-renders with the latest
  // snapshot.
  if (dragActive) return;
  lastStructuralSig = sig;

  const isMobile = isMobilePlatform(snapshot.platform);

  if (isMobile) {
    root.innerHTML = `
      <div class="mobile-shell">
        <div class="mobile-tabs">
          ${snapshot.providers
            .map(
              (p) => `
              <button class="mobile-tab ${p.id === provider.id ? "active" : ""}" data-provider="${escapeHtml(p.id)}" type="button">
                ${renderProviderLogo(p.id, "mobile-tab-logo")}
                <span>${escapeHtml(p.shortName)}</span>
              </button>
            `
            )
            .join("")}
        </div>
        <div class="middle-column" style="width:100%;min-width:0;flex:1;border-right:none;">
          ${renderMiddleContent(snapshot, provider)}
        </div>
      </div>
    `;
  } else {
    root.innerHTML = `
      <div class="desktop-shell">
        <aside class="service-rail">
          <div class="rail-brand" title="叠号 · AI 助手账号切换器">
            <img src="/logos/app-logo.svg" alt="叠号" class="rail-brand-img" draggable="false" />
          </div>
          <div class="rail-divider"></div>
          ${snapshot.providers
            .map(
              (p) => `
              <button class="rail-btn ${p.id === provider.id ? "active" : ""} ${!p.enabled ? "disabled" : ""}" data-provider="${escapeHtml(p.id)}" title="${escapeHtml(p.name)}" type="button">
                <div class="rail-logo-box">${renderProviderLogo(p.id, "rail-logo-img")}</div>
                <span class="rail-label">${escapeHtml(p.shortName)}</span>
                ${!p.enabled ? `<span class="rail-pending-tag">待接入</span>` : ""}
              </button>
            `
            )
            .join("")}
          <div class="rail-footer">
            <button
              id="check-update-btn"
              class="rail-util-btn ${updateState.result?.hasUpdate ? "has-update" : ""}"
              title="${updateState.result?.hasUpdate ? `发现新版本 v${escapeHtml(updateState.result.latestVersion)}` : "检查更新"}"
              type="button"
            >
              ${ICON_UPDATE}
              <span class="rail-util-label">更新</span>
              ${updateState.result?.hasUpdate ? `<span class="update-dot"></span>` : ""}
            </button>
          </div>
        </aside>
        <section class="middle-column">
          ${renderMiddleContent(snapshot, provider)}
        </section>
        ${renderRightStage(snapshot, provider)}
      </div>
    `;
  }

  // 0.1.21: refill the modal container after the full rebuild — the
  // container itself is recreated with #app, renderModalRoot() notices
  // the new mount and writes the modal HTML into it.
  renderModalRoot(provider);
  seedPatchState(snapshot, provider);
}

// 0.1.9 event delegation: listeners are bound ONCE on #app at bootstrap
// and dispatch by closest-match, instead of re-binding every listener
// on every render. Row clicks, provider tabs, modal buttons, long-press
// and drag-start all go through here; the drag's window-level move/up
// listeners are still added per drag session (beginDrag/endDrag).

function cancelLongPress(): void {
  if (longPressTimer) {
    clearTimeout(longPressTimer);
    longPressTimer = null;
  }
  longPressStart = null;
}

function moveDrag(clientY: number): void {
  if (!dragRow) return;
  const listEl = document.querySelector<HTMLElement>(".account-list");
  if (!listEl) return;
  const rows = Array.from(
    listEl.querySelectorAll<HTMLElement>("[data-switch-uuid]")
  );
  const after = rows.find((r) => {
    if (r === dragRow) return false;
    const rect = r.getBoundingClientRect();
    return clientY < rect.top + rect.height / 2;
  });
  if (after) listEl.insertBefore(dragRow, after);
  else listEl.appendChild(dragRow);
}

const onDragPointerMove = (mv: PointerEvent) => moveDrag(mv.clientY);
const onDragPointerUp = () => {
  void endDrag();
};
const onDragTouchMove = (ev: TouchEvent) => {
  if (dragTouchId === null) return;
  for (let i = 0; i < ev.changedTouches.length; i++) {
    const t = ev.changedTouches[i];
    if (t.identifier === dragTouchId) {
      ev.preventDefault();
      moveDrag(t.clientY);
      return;
    }
  }
};
const onDragTouchEnd = (ev: TouchEvent) => {
  if (dragTouchId === null) return;
  for (let i = 0; i < ev.changedTouches.length; i++) {
    if (ev.changedTouches[i].identifier === dragTouchId) {
      void endDrag();
      return;
    }
  }
};

async function endDrag(): Promise<void> {
  const row = dragRow;
  if (!row) return;
  dragRow = null;
  dragTouchId = null;
  dragActive = false;
  window.removeEventListener("pointermove", onDragPointerMove);
  window.removeEventListener("pointerup", onDragPointerUp);
  window.removeEventListener("pointercancel", onDragPointerUp);
  window.removeEventListener("touchmove", onDragTouchMove);
  window.removeEventListener("touchend", onDragTouchEnd);
  window.removeEventListener("touchcancel", onDragTouchEnd);
  row.classList.remove("dragging");
  const listEl = document.querySelector<HTMLElement>(".account-list");
  if (!listEl || !snapshot) {
    render();
    restoreScrollPosition();
    return;
  }
  const uuids = Array.from(
    listEl.querySelectorAll<HTMLElement>("[data-switch-uuid]")
  )
    .map((r) => r.dataset.switchUuid)
    .filter((u): u is string => Boolean(u));
  const provider = currentProvider(snapshot);
  const current = snapshot.accounts
    .filter((a) => a.providerId === provider.id)
    .map((a) => a.uuid);
  if (
    uuids.length === current.length &&
    uuids.some((u, i) => u !== current[i])
  ) {
    await callCommand("reorder_accounts", {
      providerId: provider.id,
      uuids,
    });
  } else {
    render();
    restoreScrollPosition();
  }
}

function beginDrag(row: HTMLElement): void {
  if (dragRow) return;
  dragRow = row;
  dragActive = true;
  row.classList.add("dragging");
  window.addEventListener("pointermove", onDragPointerMove);
  window.addEventListener("pointerup", onDragPointerUp);
  window.addEventListener("pointercancel", onDragPointerUp);
  window.addEventListener("touchmove", onDragTouchMove, { passive: false });
  window.addEventListener("touchend", onDragTouchEnd);
  window.addEventListener("touchcancel", onDragTouchEnd);
}

async function openReleasePage(): Promise<void> {
  const pageUrl = updateState.result?.releasePageUrl;
  if (!pageUrl) return;
  try {
    await invoke("open_external_url", { url: pageUrl });
  } catch (err) {
    updateState.error = String(err);
    rerenderPreservingScroll();
  }
}

function bindDelegatedEvents(): void {
  const root = document.querySelector<HTMLElement>("#app");
  if (!root) return;

  root.addEventListener("click", async (ev) => {
    if (!snapshot) return;
    const target = ev.target as HTMLElement;
    const provider = currentProvider(snapshot);

    // Most specific first; each branch returns after handling (this is
    // what stopPropagation used to do for the row-op buttons).
    const renameBtn = target.closest<HTMLElement>("[data-rename-uuid]");
    if (renameBtn) {
      const uuid = renameBtn.dataset.renameUuid;
      const acc = snapshot.accounts.find(
        (a) => a.providerId === provider.id && a.uuid === uuid
      );
      if (acc) {
        await setModal({
          kind: "rename",
          account: acc,
          draft: acc.autoAlias ? "" : acc.alias,
        });
      }
      return;
    }
    const removeBtn = target.closest<HTMLElement>("[data-remove-uuid]");
    if (removeBtn) {
      const uuid = removeBtn.dataset.removeUuid;
      const acc = snapshot.accounts.find(
        (a) => a.providerId === provider.id && a.uuid === uuid
      );
      if (acc) {
        await setModal({ kind: "remove", account: acc });
      }
      return;
    }
    const resolveBtn = target.closest<HTMLElement>("[data-resolve-uuid]");
    if (resolveBtn) {
      const uuid = resolveBtn.dataset.resolveUuid;
      const keepProviderId = resolveBtn.dataset.resolveProvider;
      if (uuid && keepProviderId) {
        await callCommand("resolve_uuid_conflict", { keepProviderId, uuid });
      }
      return;
    }
    // Spike 原生诊断按钮（只在 Spike 版的 Muse 页存在）。
    const diagBtn = target.closest<HTMLElement>("#spike-diagnose-btn");
    if (diagBtn) {
      await runSpikeDiagnose();
      return;
    }
    const watchBtn = target.closest<HTMLElement>("#spike-watch-btn");
    if (watchBtn) {
      await runSpikeWatch();
      return;
    }
    const sendBtn = target.closest<HTMLElement>("#spike-send-btn");
    if (sendBtn) {
      await runSpikeSend();
      return;
    }
    const quotaBtn = target.closest<HTMLElement>("#spike-quota-btn");
    if (quotaBtn) {
      await runSpikeQuota();
      return;
    }
    const approvalsBtn = target.closest<HTMLElement>("#spike-approvals-btn");
    if (approvalsBtn) {
      await runSpikeApprovals();
      return;
    }
    const copyBtn = target.closest<HTMLElement>("#spike-copy-btn");
    if (copyBtn) {
      const el = document.querySelector<HTMLElement>(".spike-diag");
      const text = el?.innerText ?? "";
      try {
        await navigator.clipboard.writeText(text);
      } catch {
        const ta = document.createElement("textarea");
        ta.value = text;
        document.body.appendChild(ta);
        ta.select();
        document.execCommand("copy");
        ta.remove();
      }
      copyBtn.textContent = "已复制";
      setTimeout(() => {
        copyBtn.textContent = "复制结果";
      }, 1200);
      return;
    }
    const providerBtn = target.closest<HTMLElement>("[data-provider]");
    if (providerBtn) {
      const nextId = providerBtn.dataset.provider as ProviderId;
      if (!nextId || nextId === snapshot.selectedProviderId) return;
      if (modal.kind !== "none") {
        await setModal({ kind: "none" });
      }
      reorderMode = false;
      reorderProviderId = null;
      await callCommand("select_provider", { providerId: nextId });
      // Batch-1: provider switch counts as activity — refresh quotas.
      triggerAutoSync();
      return;
    }
    const row = target.closest<HTMLElement>("[data-switch-uuid]");
    if (row) {
      // Batch-1: taps do nothing in reorder mode.
      if (reorderMode || modal.kind !== "none" || snapshot.conflicts.length > 0) {
        return;
      }
      const uuid = row.dataset.switchUuid;
      if (!uuid) return;
      if (
        snapshot.activeAccount?.providerId === provider.id &&
        snapshot.activeAccount?.uuid === uuid &&
        !isMobilePlatform(snapshot.platform)
      ) {
        return;
      }
      const alreadyWarm = snapshot.warmAccounts.some(
        (k) => k.providerId === provider.id && k.uuid === uuid
      );
      await callCommand(
        "switch_account",
        { providerId: provider.id, uuid },
        alreadyWarm ? null : { providerId: provider.id, uuid }
      );
      return;
    }

    const id = target.closest<HTMLElement>("[id]")?.id;
    switch (id) {
      case "open-site-btn":
      case "stage-open-site-btn":
        await callCommand("open_official_site", { providerId: provider.id });
        break;
      case "sync-all-btn": {
        const btn = document.querySelector("#sync-all-btn");
        btn?.classList.add("spinning");
        try {
          await callCommand("trigger_quota_sync");
        } finally {
          setTimeout(() => btn?.classList.remove("spinning"), 650);
        }
        break;
      }
      case "add-account-btn":
      case "stage-add-btn":
        await setModal({ kind: "add", providerId: provider.id, draft: "" });
        break;
      case "dismiss-notice-btn":
        await callCommand("dismiss_notice");
        break;
      case "check-update-btn":
        if (modal.kind !== "none") break;
        await setModal({ kind: "update" });
        if (!updateState.result && !updateState.checking && !updateState.installing) {
          await runCheckForUpdates(false);
        }
        break;
      case "update-recheck-btn":
        await runCheckForUpdates(false);
        break;
      case "update-install-btn":
        await runDownloadAndInstall();
        break;
      case "update-manual-install-btn":
      case "update-open-page-btn":
        await openReleasePage();
        break;
      case "reorder-done-btn":
        reorderMode = false;
        reorderProviderId = null;
        render();
        restoreScrollPosition();
        break;
      case "modal-cancel-btn":
        if (modal.kind === "update" && updateState.installing) break;
        await setModal({ kind: "none" });
        break;
      case "modal-confirm-btn": {
        const currentModal = modal;
        if (currentModal.kind === "add") {
          const alias = currentModal.draft.trim();
          await setModal({ kind: "none" });
          await callCommand("add_account", {
            providerId: currentModal.providerId,
            alias,
          });
        } else if (currentModal.kind === "rename") {
          const alias = currentModal.draft.trim();
          if (!alias) break;
          await setModal({ kind: "none" });
          await callCommand("rename_account", {
            providerId: currentModal.account.providerId,
            uuid: currentModal.account.uuid,
            alias,
          });
        } else if (currentModal.kind === "remove") {
          await setModal({ kind: "none" });
          await callCommand("remove_account", {
            providerId: currentModal.account.providerId,
            uuid: currentModal.account.uuid,
          });
        }
        break;
      }
      default:
        break;
    }
  });

  // Batch-1 reorder: long-press a row to enter reorder mode, then drag
  // by the whole row. The DOM order is committed on release.
  // (Drag rewritten 2026-11-21 after the first version failed on-device:
  // the old code listened for pointermove on the 28px handle itself and
  // relied on setPointerCapture to keep the moves coming; verified in a
  // touch test that capture does not retarget moves here, so once the
  // finger left the handle the drag went dead — and the midpoint-crossing
  // moves that actually reorder always happen over other rows. The drag
  // now starts on the whole row and listens for moves on window, via both
  // Pointer Events and Touch Events. One drag session guards against
  // double-handling: a touch fires touchstart then pointerdown, and the
  // second start is ignored while a drag is active.)
  root.addEventListener("pointerdown", (ev) => {
    const row = (ev.target as HTMLElement).closest<HTMLElement>(
      "[data-switch-uuid]"
    );
    if (!row) return;
    if ((ev.target as HTMLElement).closest("button")) return;
    if (reorderMode) {
      if (dragRow) return;
      ev.preventDefault();
      beginDrag(row);
      return;
    }
    longPressStart = { x: ev.clientX, y: ev.clientY };
    longPressTimer = setTimeout(() => {
      longPressTimer = null;
      longPressStart = null;
      if (modal.kind !== "none" || !snapshot) return;
      reorderMode = true;
      reorderProviderId = currentProvider(snapshot).id;
      render();
      restoreScrollPosition();
    }, 500);
  });
  root.addEventListener("pointermove", (ev) => {
    if (longPressTimer && longPressStart) {
      const dx = ev.clientX - longPressStart.x;
      const dy = ev.clientY - longPressStart.y;
      if (dx * dx + dy * dy > 100) cancelLongPress();
    }
  });
  root.addEventListener("pointerup", cancelLongPress);
  root.addEventListener("pointercancel", cancelLongPress);
  root.addEventListener(
    "touchstart",
    (ev) => {
      if (!reorderMode || dragRow) return;
      const row = (ev.target as HTMLElement).closest<HTMLElement>(
        "[data-switch-uuid]"
      );
      if (!row) return;
      if ((ev.target as HTMLElement).closest("button")) return;
      if (ev.touches.length !== 1) return;
      ev.preventDefault();
      dragTouchId = ev.touches[0].identifier;
      beginDrag(row);
    },
    { passive: false }
  );

  root.addEventListener("input", (ev) => {
    const input = ev.target as HTMLInputElement;
    if (input.id !== "modal-alias-input") return;
    if (modal.kind === "add" || modal.kind === "rename") {
      modal.draft = input.value;
    }
  });
  root.addEventListener("keydown", async (ev) => {
    const input = ev.target as HTMLElement;
    if (input.id !== "modal-alias-input") return;
    if (ev.key === "Enter") {
      ev.preventDefault();
      document.querySelector<HTMLButtonElement>("#modal-confirm-btn")?.click();
    } else if (ev.key === "Escape") {
      ev.preventDefault();
      await setModal({ kind: "none" });
    }
  });
}

async function bootstrap(): Promise<void> {
  // 0.1.9: event delegation is bound once here, before the first render.
  bindDelegatedEvents();

  await listen<ManagerSnapshot>("manager-state-changed", (event) => {
    saveScrollPosition();
    snapshot = event.payload;
    if (snapshot.activeAccount || !snapshot.switchingAccount) {
      optimisticSwitching = null;
    }
    render();
    restoreScrollPosition();
  });

  await listen<UpdateProgressEvent>("update-progress", (event) => {
    const evt = event.payload;
    updateState.progress = evt;
    if (evt.stage === "error") {
      updateState.error = evt.message;
    }
    if (modal.kind !== "update") {
      return;
    }
    const barEl = document.querySelector<HTMLElement>("#update-progress-bar");
    const textEl = document.querySelector<HTMLElement>("#update-progress-text");
    const pctEl = document.querySelector<HTMLElement>("#update-progress-pct");
    if (barEl && textEl && pctEl && lastRenderedUpdateStage === evt.stage && evt.stage !== "error") {
      textEl.textContent = evt.message;
      pctEl.textContent = formatProgressSummary(evt);
      if (evt.percent === null) {
        barEl.classList.add("indeterminate");
        barEl.style.width = "";
      } else {
        barEl.classList.remove("indeterminate");
        barEl.style.width = `${Math.max(0, Math.min(100, evt.percent))}%`;
      }
      return;
    }
    lastRenderedUpdateStage = evt.stage;
    rerenderPreservingScroll();
  });

  window.addEventListener("keydown", async (ev) => {
    if (modal.kind !== "none") {
      if (
        ev.key === "Escape" &&
        modal.kind !== "add" &&
        modal.kind !== "rename" &&
        !(modal.kind === "update" && updateState.installing)
      ) {
        ev.preventDefault();
        await setModal({ kind: "none" });
      }
      return;
    }
    const target = ev.target as HTMLElement | null;
    if (
      target &&
      (target.tagName === "INPUT" ||
        target.tagName === "TEXTAREA" ||
        target.isContentEditable)
    ) {
      return;
    }
    if ((ev.metaKey || ev.ctrlKey) && !ev.shiftKey && !ev.altKey) {
      const digit = parseInt(ev.key, 10);
      if (!isNaN(digit) && digit >= 1 && digit <= 9) {
        ev.preventDefault();
        if (!snapshot) return;
        const providerAccounts = snapshot.accounts.filter(
          (a) => a.providerId === snapshot!.selectedProviderId
        );
        const targetAcc = providerAccounts[digit - 1];
        if (targetAcc && !isSameKey(snapshot.activeAccount, targetAcc)) {
          const alreadyWarm = snapshot.warmAccounts.some(
            (k) =>
              k.providerId === targetAcc.providerId && k.uuid === targetAcc.uuid
          );
          await callCommand(
            "switch_account",
            { providerId: targetAcc.providerId, uuid: targetAcc.uuid },
            alreadyWarm ? null : { providerId: targetAcc.providerId, uuid: targetAcc.uuid }
          );
        }
        return;
      }
      if (
        ev.key === "ArrowUp" ||
        ev.key === "ArrowDown" ||
        ev.key === "[" ||
        ev.key === "]"
      ) {
        ev.preventDefault();
        if (!snapshot) return;
        const providerAccounts = snapshot.accounts.filter(
          (a) => a.providerId === snapshot!.selectedProviderId
        );
        if (providerAccounts.length <= 1) return;
        const curIdx = providerAccounts.findIndex((a) =>
          isSameKey(snapshot!.activeAccount, a)
        );
        const delta = ev.key === "ArrowUp" || ev.key === "[" ? -1 : 1;
        const nextIdx =
          (curIdx + delta + providerAccounts.length) % providerAccounts.length;
        const targetAcc = providerAccounts[nextIdx];
        if (targetAcc && !isSameKey(snapshot.activeAccount, targetAcc)) {
          const alreadyWarm = snapshot.warmAccounts.some(
            (k) =>
              k.providerId === targetAcc.providerId && k.uuid === targetAcc.uuid
          );
          await callCommand(
            "switch_account",
            { providerId: targetAcc.providerId, uuid: targetAcc.uuid },
            alreadyWarm ? null : { providerId: targetAcc.providerId, uuid: targetAcc.uuid }
          );
        }
      }
    }
  });

  // Spike 版判定：决定顶栏徽章显不显示。命令不存在（旧包）时 invoke
  // 失败，isSpikeBuild 保持 false，徽章不显示（fail-closed）。
  try {
    isSpikeBuild = await invoke<boolean>("is_spike_build");
  } catch {
    isSpikeBuild = false;
  }

  snapshot = await invoke<ManagerSnapshot>("get_manager_state");
  render();
  // Batch-1: silent sync on app entry and whenever the app becomes
  // active again (foreground / focus). No periodic background sync.
  triggerAutoSync();
  document.addEventListener("visibilitychange", () => {
    if (!document.hidden) triggerAutoSync();
  });
  window.addEventListener("focus", () => triggerAutoSync());
  setTimeout(() => {
    void runCheckForUpdates(true);
  }, 3500);
}

void bootstrap();
