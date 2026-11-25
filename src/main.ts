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
  | { kind: "rename"; account: AccountRecord; draft: string; groupDraft: string; timezoneDraft: string }
  | { kind: "remove"; account: AccountRecord }
  | { kind: "tasks"; account: AccountRecord; checking: boolean; note: string; quotaChecking: boolean }
  | { kind: "backup"; busy: boolean; encrypted: string; fileName: string; preview: BackupPreview | null; message: string; mode: BackupMode; includeGroups: boolean; includeProxy: boolean; expiryDays: number }
  | { kind: "proxyPool"; account: AccountRecord; data: ProxyPoolView | null; setting: SpikeProxySettingView | null; busy: boolean; message: string }
  | { kind: "sessions"; account: AccountRecord; loading: boolean; items: MuseSession[] | null; error: string }
  | { kind: "cookies"; account: AccountRecord; busy: boolean; preview: CookiePreview | null; message: string; imported: boolean; stored: Pick<CookiePreview, "count" | "nextExpiry"> | null }
  | { kind: "approvals"; account: AccountRecord; loading: boolean; result: SpikeApprovalsResult | null; fetchedAt: number | null; decisionMessage: string }
  | { kind: "update" }
  // 应用内确认框（替代 window.confirm）。WKWebView 的 UIDelegate 没有
  // 实现 runJavaScriptConfirmPanel，iOS 上 confirm() 直接返回 false，
  // 所以不能依赖浏览器原生弹窗。
  | {
      kind: "confirm";
      title: string;
      desc: string;
      confirmLabel: string;
      danger: boolean;
    };

interface TaskObservation {
  activity: "idle" | "working" | "approval" | "unknown";
  source: "native" | "page" | null;
  observedAtMs: number | null;
}

const latestTaskObservations = new Map<string, TaskObservation>();
const accountTimezoneChoices = [
  ["", "跟随系统"],
  ["Asia/Shanghai", "上海"],
  ["Asia/Singapore", "新加坡"],
  ["Asia/Tokyo", "东京"],
  ["Europe/London", "伦敦"],
  ["America/New_York", "纽约"],
  ["America/Los_Angeles", "洛杉矶"],
  ["UTC", "UTC"],
];

type BackupMode = "light" | "full" | "custom";
interface BackupPreview {
  added: number;
  existing: number;
  conflicts: string[];
  proxyPoolCount: number;
  checksum: string;
}

interface ProxyPoolView {
  entries: { id: string; endpoint: string; requiresAuth: boolean; missingCredentials?: boolean; sourceId: string | null }[];
  sources: { id: string; host: string; refreshedAt: number }[];
}

interface MuseSession {
  id: string;
  title: string;
  updatedAtMs: number | null;
  pinned: boolean;
  archived: boolean;
}

interface CookiePreview {
  count: number;
  domains: string[];
  nextExpiry: number | null;
}

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
// 应用内确认框的 resolver：由 askConfirm 设置，确认/取消/被顶掉时结算。
let confirmResolver: ((ok: boolean) => void) | null = null;
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
  sessionScopeId: string; choices: { decision: string; scope: string | null }[];
}
interface SpikeApprovalsResult {
  ok: boolean; phase: string; status: number | null;
  count: number; items: SpikeApprovalItem[]; error: string | null;
}
let spikeApprovals: SpikeApprovalsResult | null = null;
let spikeApprovalsRunning = false;
let spikeApprovalsError: string | null = null;
interface SpikeQuota {
  tier: string; percentUsed: number; usageObserved?: boolean; state: string; resetsAtMs: number; source: string;
}
interface SpikeQuotaProbe {
  ok: boolean; quota: SpikeQuota | null; diag: string[]; error: string | null;
}
let spikeQuotaProbe: SpikeQuotaProbe | null = null;
let spikeQuotaRunning = false;
interface BackgroundQuotaDiagnostic {
  account: string;
  provider: ProviderId;
  proxy: string;
  network: string;
  quota: string;
}
let backgroundQuotaRows: BackgroundQuotaDiagnostic[] | null = null;
let backgroundQuotaRunning = false;
let backgroundQuotaError = "";
let spikeQuotaError: string | null = null;
interface SpikeLiveness {
  running: boolean; phase: string; startedMs: number; connectedMs: number | null;
  subscriptionReadyMs: number | null; deadlineMs: number | null; observedAtMs: number; maxObservationGapMs: number;
  lastEventMs: number | null; endedMs: number | null; lines: number;
  events: number; endReason: string | null; error: string | null;
}
let spikeLive: SpikeLiveness | null = null;
let spikeLiveError: string | null = null;
let spikeLiveRefreshing = false;
interface SpikeLease {
  ok: boolean; phase: string; assignmentOnly: boolean; sameVmOnRecheck: boolean | null;
  firstExpiresAtMs: number | null; ttlSeconds: number | null;
  expiryFieldPresent: boolean; renewal: string; renewalConfirmed: boolean;
  secondExpiresAtMs: number | null; calls: string[]; sideEffectsNote: string;
  error: string | null;
}
let spikeLease: SpikeLease | null = null;
let spikeLeaseRunning = false;
let spikeLeaseError: string | null = null;
interface SpikeSendResult {
  ok: boolean; status: string; sendId: string;
  ack: unknown; sessionId: string | null; error: string | null;
}
let spikeSend: SpikeSendResult | null = null;
let spikeSendRunning = false;
let spikeSendError: string | null = null;
// #18 模型指纹：只有 verdict=scored 才是有效识别；partial 只有候选排名；
// indeterminate 不可判定（样本不足/请求或参考库失败）。
interface SpikeFingerprintResult {
  verdict: "scored" | "partial" | "indeterminate";
  identification: boolean;
  prediction: string;
  predictionName: string;
  familyPrediction: string;
  familyPredictionName: string;
  candidates: Array<{
    model: string; displayName: string; family: string; familyName: string;
    score: number; probability: number | null;
  }>;
  usedSamples: number; requiredSamples: number;
  calibrated: boolean; probability: number | null; probabilityStatus: string;
  evidenceLabel: string; evidenceReason: string;
  referenceBuiltAt: string; method: string;
  samples: Array<{ expectedCount: number; parsedNumbers: number; minimumNumbers: number; accepted: boolean }>;
}
interface SpikeStreamEventCounts {
  user: number; start: number; delta: number; done: number; activity: number; other: number;
}

function formatStreamEventCounts(label: string, counts: SpikeStreamEventCounts): string {
  return `${label} 用户 ${counts.user} · 开始 ${counts.start} · 增量 ${counts.delta} · 结束 ${counts.done} · 活动 ${counts.activity} · 其他 ${counts.other}`;
}

function formatUnmatchedEvents(diagnostics: NonNullable<SpikeFingerprintSendRecord["diagnostics"]>): string {
  if (!diagnostics.unmatchedEvents) return "";
  const conflicts = diagnostics.conflictingEvents
    ? `（会话 ${diagnostics.sessionConflicts} · 明确回复指向 ${diagnostics.userLinkConflicts} · 轮次或任务 ${diagnostics.turnTaskConflicts} · 父消息 ${diagnostics.parentConflicts}）`
    : "";
  return `<div class="hint">未关联明细 · 本轮范围外 ${diagnostics.outsideTurnEvents} · 标识冲突 ${diagnostics.conflictingEvents}${conflicts} · 缺助手标识 ${diagnostics.missingMessageIdEvents} · 缺关联依据 ${diagnostics.unlinkedEvents}</div>`;
}

function formatMessageTiming(diagnostics: NonNullable<SpikeFingerprintSendRecord["diagnostics"]>): string {
  const first = diagnostics.firstMatchedDeltaMs;
  const span = diagnostics.firstDeltaToMessageDoneMs;
  return `开始采集至首个关联增量 ${first === null ? "未观测" : `${first} ms`} · 首个增量至消息结束 ${span === null ? "未观测" : `${span} ms`}`;
}

interface SpikeFingerprintSendRecord {
  probeId: string; sendStatus: string; textDone: boolean;
  sampleUsed: boolean; error: string | null;
  diagnostics: {
    endedBy: string; lines: number; textDeltas: number; readTimeouts: number;
    sendEvents: SpikeStreamEventCounts; subscriptionEvents: SpikeStreamEventCounts;
    firstMatchedDeltaMs: number | null;
    firstDeltaToMessageDoneMs: number | null;
    parseErrors: number; unmatchedEvents: number; sawUser: boolean;
    outsideTurnEvents: number; conflictingEvents: number;
    sessionConflicts: number; userLinkConflicts: number;
    turnTaskConflicts: number; parentConflicts: number;
    missingMessageIdEvents: number; unlinkedEvents: number;
    assistantMessages: number; sawIdle: boolean;
  } | null;
}
interface SpikeFingerprintProbe {
  ok: boolean; phase: string; verdict: string; identification: boolean;
  result: SpikeFingerprintResult | null;
  probesSent: number; probesConfirmed: number; samplesCollected: number;
  sends: SpikeFingerprintSendRecord[];
  wireTrace?: string[];
  sessionId: string | null;
  error: string | null;
}
interface SpikeHarkProbe {
  restStatus: number | null; sseStatus: number | null; sseEnd: string | null;
  sseFrames: number; pollStatus: number | null; pollFrames: number;
  summary: { malformedFrames: number; knownMessages: number; running: number; completed: number; failed: number; duplicateStates: number };
  error: string | null;
}
const harkProbeByAccount = new Map<string, { running: boolean; result: SpikeHarkProbe | null; error: string | null }>();

function harkTestAccount(): AccountRecord | undefined {
  const accounts = snapshot?.accounts.filter((account) =>
    account.providerId === "hark" && account.authState === "authenticated") ?? [];
  const active = snapshot?.activeAccount;
  return accounts.find((account) => active && isSameKey(active, account)) ?? accounts[0];
}

function harkProbeHtml(): string {
  const account = harkTestAccount();
  const state = account && harkProbeByAccount.get(account.uuid);
  const result = state?.result;
  const diagnosis = result
    ? `<div class="spike-diag-result">
        <div>REST ${result.restStatus ?? "未观测"} · SSE ${result.sseStatus ?? "未观测"} · Poll ${result.pollStatus ?? "未观测"}</div>
        <div>SSE ${result.sseFrames} 帧 · Poll ${result.pollFrames} 帧 · ${escapeHtml(result.sseEnd ?? "未观测")}</div>
        <div>已关联助手消息 ${result.summary.knownMessages} · 观测期间开始 ${result.summary.running} · 完成 ${result.summary.completed} · 失败 ${result.summary.failed}</div>
        <div>去重 ${result.summary.duplicateStates} · 坏帧 ${result.summary.malformedFrames}</div>
        ${result.error ? `<div>${escapeHtml(result.error)}</div>` : ""}
      </div>`
    : state?.error ? `<div class="spike-diag-result fail">${escapeHtml(state.error)}</div>` : "";
  return `<div class="spike-diag">
    <button id="spike-hark-watch-btn" class="mini-btn" type="button" ${!account || state?.running ? "disabled" : ""}>${state?.running ? "正在观测…" : "Hark 原生检测"}</button>
    ${diagnosis}
  </div>`;
}

async function runHarkWatch(): Promise<void> {
  const account = harkTestAccount();
  if (!account || harkProbeByAccount.get(account.uuid)?.running) return;
  const confirmed = await askConfirm({
    title: "检测 Hark 会话",
    desc: `对非主力测试账号「${account.alias}」进行一次只读检测，记录同步通道和状态变化。`,
    confirmLabel: "开始检测",
  });
  if (!confirmed || harkProbeByAccount.get(account.uuid)?.running) return;
  const state = { running: true, result: null as SpikeHarkProbe | null, error: null as string | null };
  harkProbeByAccount.set(account.uuid, state);
  render();
  try {
    state.result = await invoke<SpikeHarkProbe>("hark_native_watch_probe", {
      uuid: account.uuid,
      timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
    });
  } catch (e) {
    state.error = String(e);
  } finally {
    state.running = false;
    render();
  }
}

let spikeFingerprint: SpikeFingerprintProbe | null = null;
interface SpikeFingerprintTask {
  accountUuid: string; taskId: string;
}
interface SpikeFingerprintProgress extends SpikeFingerprintTask {
  phase: string; probe: number; confirmed: number; samples: number;
}
let spikeFingerprintRunning = false;
let spikeFingerprintTask: SpikeFingerprintTask | null = null;
let spikeFingerprintProgress: SpikeFingerprintProgress | null = null;
let spikeFingerprintError: string | null = null;
// #11/#16 代理探针：仅展示阶段/状态码/脱敏代理串，不含 cookie/token/口令。
interface SpikeProxyProbe {
  ok: boolean; phase: string; tunnelVia: string | null; proxyScheme: string | null;
  handshakeCompleted: boolean; pingStatus: number | null; failClosed: boolean;
  connectivityStatus: number | null;
  error: string | null;
}
let spikeProxyRunning = false;
let spikeProxyTask: SpikeFingerprintTask | null = null;
let spikeLiveTask: SpikeFingerprintTask | null = null;
let spikeLeaseTask: SpikeFingerprintTask | null = null;
let spikeSendTask: SpikeFingerprintTask | null = null;
let spikeWatchTask: SpikeFingerprintTask | null = null;
// #11/#16 账号级代理配置（脱敏视图；口令只进 Keychain）。
interface SpikeWebviewProxyView {
  applied: boolean; sessionOpen: boolean; reason: string | null;
  /** iOS：下发失败时 WebView 出口停在隔离态（不可达代理，不会回退直连）。 */
  quarantined?: boolean;
}
interface SpikeProxySettingView {
  enabled: boolean; display: string; hasCredentials: boolean;
  credentialAvailable: boolean | null;
  /** 当前操作观察到的 WebView 下发结果。 */
  webview?: SpikeWebviewProxyView | null;
}
interface SpikeProxyAccountState {
  uri: string;
  username: string;
  view: SpikeProxySettingView | null;
  result: SpikeProxyProbe | null;
  error: string | null;
  read: Promise<SpikeProxySettingView | null> | null;
  loaded: boolean;
  saveLines: string[];
  saveOk: boolean | null;
  clearPassword: boolean;
}

const spikeProxyAccounts = new Map<string, SpikeProxyAccountState>();
const emptyProxyAccount: SpikeProxyAccountState = {
  uri: "", username: "", view: null, result: null, error: null, read: null, loaded: false,
  saveLines: [], saveOk: null, clearPassword: false,
};

function spikeProxyAccount(uuid: string): SpikeProxyAccountState {
  const existing = spikeProxyAccounts.get(uuid);
  if (existing) return existing;
  const created = { ...emptyProxyAccount };
  spikeProxyAccounts.set(uuid, created);
  return created;
}

let spikeProxyBusy = false;
// 最近一次操作的阶段记录（脱敏）：真机出问题时，先看「点击→确认→IPC→返回」
// 停在哪一步，不用猜。只保留最近 10 条，不写入磁盘。
interface SpikeTraceEntry {
  at: number;
  label: string;
  stage: string;
  detail: string | null;
}
const spikeTraceEntries: SpikeTraceEntry[] = [];
// 中性提示（用户取消、已在运行等），与红色错误区分。
let spikeNotice: string | null = null;
let spikeWorkspaceOpen = false;
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
const accountFilters: Record<ProviderId, { query: string; group: string | null }> = {
  muse: { query: "", group: null },
  hark: { query: "", group: null },
};
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
  // 离开确认框的所有路径（取消按钮、Esc、切换账号等）都按「取消」结算，
  // 调用方的 await 永远不会悬挂。
  if (modal.kind === "confirm" && next.kind !== "confirm") {
    const resolve = confirmResolver;
    confirmResolver = null;
    resolve?.(false);
  }
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
  } else if (next.kind === "remove" || next.kind === "confirm") {
    document.querySelector<HTMLButtonElement>("#modal-confirm-btn")?.focus();
  }
}

// 应用内异步确认。返回 true 仅表示用户点了确认按钮；任何其他关闭方式
// （取消 / Esc / 被新的弹窗顶掉）都返回 false。
async function askConfirm(options: {
  title: string;
  desc: string;
  confirmLabel?: string;
  danger?: boolean;
}): Promise<boolean> {
  if (confirmResolver) {
    const previous = confirmResolver;
    confirmResolver = null;
    previous(false);
  }
  const answer = new Promise<boolean>((resolve) => {
    confirmResolver = resolve;
  });
  await setModal({
    kind: "confirm",
    title: options.title,
    desc: options.desc,
    confirmLabel: options.confirmLabel ?? "继续",
    danger: options.danger ?? false,
  });
  return answer;
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
    if (cmd === "switch_account" || cmd === "open_muse_thread") {
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
let foregroundTaskIndex = 0;
let foregroundTaskBusy = false;
let foregroundTaskTimer: number | null = null;
const taskRetryAt = new Map<string, number>();
const pendingTaskActivity = new Map<string, TaskObservation>();

function applyTaskObservation(key: string, result: TaskObservation): void {
  const previous = latestTaskObservations.get(key);
  if (!result.observedAtMs) {
    pendingTaskActivity.delete(key);
    result.observedAtMs = previous?.observedAtMs ?? null;
    result.source = previous?.source ?? null;
  } else if (previous?.activity !== result.activity) {
    const priorSample = pendingTaskActivity.get(key);
    if (priorSample?.activity !== result.activity || priorSample.source !== result.source) {
      pendingTaskActivity.set(key, result);
      result = { ...result, activity: "unknown" };
    } else if (result.observedAtMs - (priorSample.observedAtMs ?? 0) < 5_000) {
      result = { ...result, activity: "unknown" };
    } else {
      pendingTaskActivity.delete(key);
    }
  } else {
    pendingTaskActivity.delete(key);
  }
  latestTaskObservations.set(key, result);
}

async function refreshForegroundTask(): Promise<void> {
  if (!snapshot || document.hidden || !navigator.onLine || foregroundTaskBusy || (modal.kind === "tasks" && modal.checking)) return;
  const accounts = snapshot.accounts.filter((account) =>
    account.taskNotifications && account.authState === "authenticated"
      && Date.now() >= (taskRetryAt.get(`${account.providerId}:${account.uuid}`) ?? 0)
  );
  if (!accounts.length) return;
  const account = accounts[foregroundTaskIndex++ % accounts.length];
  foregroundTaskBusy = true;
  const key = `${account.providerId}:${account.uuid}`;
  try {
    const result = await invoke<TaskObservation>("observe_account_tasks", {
      uuid: account.uuid, providerId: account.providerId, automatic: true, timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
    });
    applyTaskObservation(key, result);
    taskRetryAt.set(key, Date.now() + (pendingTaskActivity.has(key) || result.activity === "working" ? 15_000 :
      result.activity === "unknown" ? 180_000 : 60_000));
  } catch {
    applyTaskObservation(key, { activity: "unknown", observedAtMs: null, source: null });
    taskRetryAt.set(key, Date.now() + 120_000);
  } finally {
    foregroundTaskBusy = false;
    if (modal.kind === "tasks" && modal.account.uuid === account.uuid) render();
  }
}

let lastProxyPoolRefresh = 0;

function triggerAutoSync(): void {
  if (snapshot?.platform === "ios" && Date.now() - lastProxyPoolRefresh >= 30 * 60 * 1000) {
    lastProxyPoolRefresh = Date.now();
    void invoke("manage_proxy_pool", { action: "refresh", entryId: null, input: null }).catch(() => {});
  }
  void invoke("trigger_quota_sync", { force: false }).catch(() => {});
}

// Spike 原生诊断（0.1.22）：诊断区只在 Spike 版的 Muse 页显示。
// 结果只同步诊断区 DOM（syncSpikeDiag），不走 render()，不重建列表。

// 阶段记录：点击后先记「已点击」，确认后记「已确认/已取消」，再记
// 「已调用后端」与返回结果。真机复测时这一段就是最直接的证据链。
function spikeTrace(label: string, stage: string, detail?: string): void {
  spikeTraceEntries.push({ at: Date.now(), label, stage, detail: detail ?? null });
  while (spikeTraceEntries.length > 10) spikeTraceEntries.shift();
  syncSpikeTrace();
}

function spikeTraceLinesHtml(): string {
  return spikeTraceEntries
    .slice(-6)
    .map((entry) => {
      const time = new Date(entry.at).toLocaleTimeString();
      const detail = entry.detail ? `（${entry.detail}）` : "";
      return `<div>${escapeHtml(time)}　${escapeHtml(entry.label)} · ${escapeHtml(entry.stage)}${escapeHtml(detail)}</div>`;
    })
    .join("");
}

// 只重写阶段记录容器：不重建整个诊断区，输入框（代理口令）不会被清空。
function syncSpikeTrace(): void {
  const el = document.querySelector<HTMLElement>("#spike-front-trace");
  if (!el) return;
  const html = spikeTraceLinesHtml();
  el.innerHTML = html;
  el.hidden = html === "";
}

function spikeDiagInnerHtml(accountUuid: string | undefined = pickWatchAccount()?.uuid): string {
  const proxyAccount = accountUuid ? spikeProxyAccount(accountUuid) : emptyProxyAccount;
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
  let liveHtml = "";
  if (spikeLive) {
    const l = spikeLive;
    const fmt = (ms: number) => new Date(ms).toLocaleTimeString();
    const observedElapsed = l.connectedMs
      ? Math.max(0, Math.round(((l.endedMs ?? Date.now()) - l.connectedMs) / 1000))
      : 0;
    const gapSeconds = Math.round((l.maxObservationGapMs ?? 0) / 1000);
    const suspendedGap = gapSeconds >= 15;
    const windowSeconds = l.deadlineMs && l.subscriptionReadyMs
      ? Math.max(0, Math.round((l.deadlineMs - l.subscriptionReadyMs) / 1000))
      : 0;
    liveHtml = `
      <div class="spike-diag-result ${l.error ? "fail" : "ok"}">
        <div>${l.running ? "● 探针任务运行中" : "■ 探针已结束"}（阶段 ${escapeHtml(l.phase || "—")}）</div>
        <div>订阅窗口 ${windowSeconds}s；墙钟经过 ${observedElapsed}s；事件 ${l.events}（读行 ${l.lines}）</div>
        <div>建立 ${l.connectedMs ? fmt(l.connectedMs) : "—"}　订阅就绪 ${l.subscriptionReadyMs ? fmt(l.subscriptionReadyMs) : "—"}　最后事件 ${l.lastEventMs ? fmt(l.lastEventMs) : "—"}　观测结束 ${l.endedMs ? fmt(l.endedMs) : "—"}</div>
        <div>最大任务观测间隔 ${gapSeconds}s${suspendedGap ? "（存在未观测区间，不能证明连接持续存活）" : ""}；结束原因 ${escapeHtml(l.endReason || "—")}</div>
        ${l.error ? `<div>错误：${escapeHtml(l.error)}</div>` : ""}
      </div>`;
  }
  if (!spikeLive && spikeLiveError) {
    liveHtml = `<div class="spike-diag-result fail">存活探针错误：${escapeHtml(spikeLiveError)}</div>`;
  }
  let leaseHtml = "";
  if (spikeLease) {
    const l = spikeLease;
    const fmt = (ms: number | null) => (ms ? new Date(ms).toLocaleString() : "—");
    const renewalLabel: Record<string, string> = {
      not_attempted: "未测（只取一次租约）",
      unobservable: "不可观测（到期字段缺失或前后不一致）",
      unchanged: "到期字段未变化",
      changed: "同一 VM 的到期字段已变化",
      vm_changed: "服务端分配了不同 VM，本轮无法比较到期时间",
      recheck_failed: "第二次观测失败",
    };
    leaseHtml = `
      <div class="spike-diag-result ${l.ok ? "ok" : "fail"}">
        <div>${l.ok ? (l.assignmentOnly ? "✓ 已确认 VM 分配" : "✓ 已取得 VM 租约") : "✗ 租约观测失败"}（阶段 ${escapeHtml(l.phase)}）</div>
        ${l.ok ? `<div>到期 ${fmt(l.firstExpiresAtMs)}　TTL ${l.ttlSeconds ?? "—"}s　到期字段${l.expiryFieldPresent ? "存在" : "缺失"}</div>` : ""}
        ${l.secondExpiresAtMs ? `<div>复查到期 ${fmt(l.secondExpiresAtMs)}</div>` : ""}
        ${l.ok ? `<div>续租：${escapeHtml(renewalLabel[l.renewal] ?? l.renewal)}${l.renewalConfirmed ? "" : "　（未确认）"}</div>` : ""}
        ${l.calls.length ? `<div>调用：${escapeHtml(l.calls.join("；"))}</div>` : ""}
        ${l.ok ? `<div class="hint">${escapeHtml(l.sideEffectsNote)}</div>` : ""}
        ${l.error ? `<div>错误：${escapeHtml(l.error)}</div>` : ""}
      </div>`;
  } else if (spikeLeaseError) {
    leaseHtml = `<div class="spike-diag-result fail">租约探针错误：${escapeHtml(spikeLeaseError)}</div>`;
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
  const spikeQuota = spikeQuotaProbe?.quota ?? null;
  if (spikeQuota) {
    const q = spikeQuota;
    quotaHtml = `
      <div class="spike-diag-result ok">
        <div>✓ 额度：${escapeHtml(q.tier || "—")}　已用 ${q.usageObserved === false ? "未观测" : `${q.percentUsed}%`}　状态 ${escapeHtml(q.state || "—")}</div>
        <div>重置：${q.resetsAtMs ? new Date(q.resetsAtMs).toLocaleString() : "—"}　来源：${escapeHtml(q.source)}</div>
      </div>`;
  } else if (spikeQuotaProbe) {
    quotaHtml = `
      <div class="spike-diag-result fail">
        <div>✗ 额度获取失败</div>
        ${(spikeQuotaProbe.diag ?? [])
          .map((d) => `<div>· ${escapeHtml(d)}</div>`)
          .join("")}
        ${spikeQuotaProbe.error ? `<div>${escapeHtml(spikeQuotaProbe.error)}</div>` : ""}
      </div>`;
  } else if (spikeQuotaError) {
    quotaHtml = `<div class="spike-diag-result fail">错误：${escapeHtml(spikeQuotaError)}</div>`;
  }
  let approvalsHtml = "";
  if (spikeApprovals) {
    const a = spikeApprovals;
    approvalsHtml = `
      <div class="spike-diag-result ${a.ok ? "ok" : "fail"}">
        <div>${a.ok ? "✓ 审批只读查询成功" : "✗ 审批查询失败"}（HTTP ${a.status ?? "—"}${a.ok ? `，服务端返回 ${a.count} 条` : ""}）</div>
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
  let proxyHtml = "";
  if (proxyAccount.view) {
    const v = proxyAccount.view;
    proxyHtml += `<div class="hint">账号代理：${v.enabled ? "已启用" : "未启用"}　${escapeHtml(v.display)}</div>`;
    const credential = v.hasCredentials ? (v.credentialAvailable ? "可读" : "不可用") : "无需认证";
    proxyHtml += `<div class="hint">Keychain 凭据：${credential}</div>`;
    const wv = v.webview;
    if (wv) {
      const desc = wv.applied
        ? wv.sessionOpen
          ? "已写入（当前会话需重新打开才生效）"
          : "已写入"
        : `未生效：${wv.reason ?? "该设备不支持 WebView 代理"}`;
      proxyHtml += `<div class="hint">账号 WebView：${escapeHtml(desc)}</div>`;
    } else {
      proxyHtml += `<div class="hint">账号 WebView：等待会话打开或保存时验证下发</div>`;
    }
  }
  if (proxyAccount.saveLines.length) {
    const cls =
      proxyAccount.saveOk === null ? "" : proxyAccount.saveOk ? "ok" : "fail";
    proxyHtml += `<div class="spike-diag-result ${cls}">${proxyAccount.saveLines
      .map((line) => `<div>${escapeHtml(line)}</div>`)
      .join("")}</div>`;
  }
  if (proxyAccount.result) {
    const p = proxyAccount.result;
    proxyHtml = `
      <div class="spike-diag-result ${p.ok ? "ok" : "fail"}">
        <div>${p.ok ? "✓" : "■"} 代理探针：阶段 ${escapeHtml(p.phase)}　协议 ${escapeHtml(p.proxyScheme ?? "—")}　隧道 ${escapeHtml(p.tunnelVia ?? "—")}</div>
        ${p.phase === "connectivity"
          ? `<div>代理 HTTPS 响应 ${p.connectivityStatus ?? "—"}　fail-closed ${p.failClosed ? "开启" : "关闭"}</div>`
          : `<div>WS 握手 ${p.handshakeCompleted ? "完成" : "未完成"}　ping ${p.pingStatus ?? "—"}　fail-closed ${p.failClosed ? "开启" : "关闭"}</div>`}
        ${p.error ? `<div>错误：${escapeHtml(p.error)}</div>` : ""}
      </div>`;
  } else if (proxyAccount.error) {
    proxyHtml += `<div class="spike-diag-result fail">代理探针错误：${escapeHtml(proxyAccount.error)}</div>`;
  }
  let fingerprintHtml = "";
  if (spikeFingerprintRunning && spikeFingerprintProgress) {
    const progress = spikeFingerprintProgress;
    const phase: Record<string, string> = {
      cookies: "读取测试账号登录态", reference_bank: "加载参考库", session: "建立并核对隔离会话",
      register_route: "注册隔离探针的推送能力",
      stream_delta: "已收到实时文本增量，等待流内结束",
      subscribe: "等待事件订阅就绪", sending: "发送探针", collecting: "采集完整回复", scoring: "本地评分",
    };
    fingerprintHtml = `<div class="spike-diag-result" role="status">${escapeHtml(phase[progress.phase] ?? progress.phase)}${progress.probe ? ` ${progress.probe}/3` : ""}　发送确认 ${progress.confirmed}　有效样本 ${progress.samples}</div>`;
  } else if (spikeFingerprint) {
    const f = spikeFingerprint;
    const r = f.result;
    const verdictLabel: Record<string, string> = {
      scored: "已判定（校准置信度）",
      partial: "仅候选排名（样本不足，不算有效识别）",
      indeterminate: "不可判定（无候选）",
    };
    const candHtml = r && r.candidates.length
      ? r.candidates
          .slice(0, 5)
          .map(
            (c, i) =>
              `<div>#${i + 1} ${escapeHtml(c.displayName || c.model)}（${escapeHtml(c.familyName || c.family)}）　score ${c.score.toFixed(3)}${c.probability != null ? `　p=${c.probability.toFixed(3)}` : ""}</div>`
          )
          .join("")
      : "";
    const sampleHtml = r && r.samples.length
      ? r.samples
          .map(
            (s, i) =>
              `探针${i + 1}：解析 ${s.parsedNumbers}/${s.expectedCount}（阈值 ${s.minimumNumbers}，${s.accepted ? "有效" : "无效"}）`
          )
          .join("　")
      : "";
    const sendsHtml = f.sends && f.sends.length
      ? f.sends
          .map(
            (s) =>
              `<div>探针 ${escapeHtml(s.probeId)}：${escapeHtml(s.sendStatus)}${s.textDone ? "（回复完成）" : ""}${s.sampleUsed ? "（入样）" : ""}${s.error ? `｜${escapeHtml(s.error)}` : ""}</div>
              ${s.diagnostics ? `<div class="hint">${escapeHtml(s.diagnostics.endedBy)} · 事件行 ${s.diagnostics.lines} · 文本增量 ${s.diagnostics.textDeltas} · 读取超时 ${s.diagnostics.readTimeouts} · 解析错误 ${s.diagnostics.parseErrors} · 未关联事件 ${s.diagnostics.unmatchedEvents} · 用户事件 ${s.diagnostics.sawUser ? "已关联" : "未观察"} · 助手消息 ${s.diagnostics.assistantMessages} · 空闲 ${s.diagnostics.sawIdle ? "已观察" : "未观察"}</div>${formatUnmatchedEvents(s.diagnostics)}<div class="hint">${formatStreamEventCounts("发送流", s.diagnostics.sendEvents)}</div><div class="hint">${formatStreamEventCounts("订阅流", s.diagnostics.subscriptionEvents)}</div><div class="hint">${formatMessageTiming(s.diagnostics)}</div>` : ""}`
          )
          .join("")
      : "";
    const cls = f.verdict === "scored" ? "ok" : f.verdict === "partial" ? "" : "fail";
    const wireTrace = f.wireTrace?.join("\n") ?? "";
    fingerprintHtml = `
      <div class="spike-diag-result ${cls}">
        <div>${f.ok ? "✓" : "■"} 指纹探针：${escapeHtml(verdictLabel[f.verdict] ?? f.verdict)}（阶段 ${escapeHtml(f.phase)}）</div>
        <dl class="spike-metrics">
          <div><dt>进入网络写入</dt><dd>${f.probesSent}/3</dd></div>
          <div><dt>发送确认</dt><dd>${f.probesConfirmed ?? 0}</dd></div>
          <div><dt>采到样本</dt><dd>${f.samplesCollected}</dd></div>
          <div><dt>参与评分</dt><dd>${r?.usedSamples ?? 0}/${r?.requiredSamples ?? 3}</dd></div>
        </dl>
        ${sendsHtml ? `<details class="spike-wire-details" data-spike-section="fingerprint-rounds"><summary>查看逐条探针结果</summary><div class="hint">${sendsHtml}</div></details>` : ""}
        ${wireTrace ? `<details class="spike-wire-details" data-spike-section="wire"><summary>查看流诊断日志</summary><pre id="spike-wire-trace">${escapeHtml(wireTrace)}</pre></details>` : ""}
        ${r && r.prediction ? `<div>候选首位：${escapeHtml(r.predictionName || r.prediction)}　家族 ${escapeHtml(r.familyPredictionName || r.familyPrediction)}</div>` : ""}
        ${r?.calibrated && r.probability != null ? `<div>校准置信度：${(r.probability * 100).toFixed(1)}%（${escapeHtml(r.probabilityStatus)}，闭集参考）</div>` : ""}
        ${candHtml ? `<div>${candHtml}</div>` : ""}
        ${sampleHtml ? `<div class="hint">${escapeHtml(sampleHtml)}</div>` : ""}
        ${r?.evidenceReason ? `<div class="hint">${escapeHtml(r.evidenceLabel)}：${escapeHtml(r.evidenceReason)}</div>` : ""}
        ${r?.referenceBuiltAt ? `<div class="hint">参考库 ${escapeHtml(r.referenceBuiltAt)}　${escapeHtml(r.method)}</div>` : ""}
        ${f.sessionId ? `<div class="hint">隔离会话标识末尾 ${escapeHtml(f.sessionId.slice(-8))}</div>` : ""}
        ${f.error ? `<div>错误：${escapeHtml(f.error)}</div>` : ""}
      </div>`;
  } else if (spikeFingerprintError) {
    fingerprintHtml = `<div class="spike-diag-result fail">指纹探针错误：${escapeHtml(spikeFingerprintError)}</div>`;
  }
  return `
    ${spikeNotice ? `<div class="spike-notice">${escapeHtml(spikeNotice)}</div>` : ""}
    <section class="spike-panel" aria-labelledby="spike-fingerprint-title">
      <h3 id="spike-fingerprint-title">指纹与实时接收</h3>
      <div class="spike-actions">
        <button id="spike-fingerprint-btn" class="mini-btn spike-primary" type="button" ${spikeFingerprintRunning ? "disabled" : ""}>${spikeFingerprintRunning ? "检测中…" : "指纹探针 3 条"}</button>
        ${spikeFingerprintRunning || spikeProxyRunning || spikeWatchRunning || spikeSendRunning || spikeLeaseRunning || spikeLive?.running ? `<button id="spike-stop-btn" class="mini-btn" type="button">停止</button>` : ""}
      </div>
      ${fingerprintHtml}
    </section>
    <details class="spike-panel" data-spike-section="connection">
      <summary>连接与消息测试</summary>
      <div class="spike-actions">
        <button id="spike-diagnose-btn" class="mini-btn" type="button" ${spikeDiagnoseRunning ? "disabled" : ""}>${btnLabel}</button>
        <button id="spike-watch-btn" class="mini-btn" type="button" ${spikeWatchRunning ? "disabled" : ""}>${spikeWatchRunning ? "订阅中…" : "订阅 15 秒"}</button>
        <button id="spike-send-btn" class="mini-btn" type="button" ${spikeSendRunning ? "disabled" : ""}>${spikeSendRunning ? "发送中…" : "发送探针"}</button>
        <button id="spike-live-btn" class="mini-btn" type="button" ${spikeLive?.running ? "disabled" : ""}>${spikeLive?.running ? "存活中…" : "存活 5 分钟"}</button>
        <button id="spike-lease-btn" class="mini-btn" type="button" ${spikeLeaseRunning ? "disabled" : ""}>${spikeLeaseRunning ? "观测中…" : "租约探针"}</button>
      </div>
      ${resultHtml}${watchHtml}${sendHtml}${liveHtml}${leaseHtml}
    </details>
    <details class="spike-panel" data-spike-section="account">
      <summary>额度与审批</summary>
      <div class="spike-actions">
        <button id="spike-quota-btn" class="mini-btn" type="button" ${spikeQuotaRunning ? "disabled" : ""}>${spikeQuotaRunning ? "查询中…" : "查询额度"}</button>
        <button id="spike-approvals-btn" class="mini-btn" type="button" ${spikeApprovalsRunning ? "disabled" : ""}>${spikeApprovalsRunning ? "查询中…" : "查询审批"}</button>
      </div>
      ${quotaHtml}${approvalsHtml}
      ${snapshot?.platform === "ios" ? `
        <div class="spike-actions">
          <button id="spike-quota-sync-btn" class="mini-btn" type="button" ${backgroundQuotaRunning ? "disabled" : ""}>${backgroundQuotaRunning ? "同步中…" : "测试后台额度同步"}</button>
          ${backgroundQuotaRows?.length ? '<button id="spike-quota-sync-copy-btn" class="mini-btn" type="button">复制脱敏同步结果</button>' : ""}
        </div>
        ${backgroundQuotaLines().map((line) => `<div class="spike-diag-result">${escapeHtml(line)}</div>`).join("")}
      ` : ""}
    </details>
    <details class="spike-panel" data-spike-section="proxy">
      <summary>账号代理</summary>
      <div class="spike-proxy-fields">
        <label for="spike-proxy-uri">代理 URI（不含密码）</label>
        <input id="spike-proxy-uri" type="text" placeholder="socks5://host:1080" value="${escapeHtml(proxyAccount.uri)}" />
        <label for="spike-proxy-user">用户名（可选）</label>
        <input id="spike-proxy-user" type="text" value="${escapeHtml(proxyAccount.username)}" />
        <label for="spike-proxy-pass">密码（仅存 Keychain）</label>
        <input id="spike-proxy-pass" type="password" />
      </div>
      <div class="spike-actions">
        <button id="spike-proxy-save-btn" class="mini-btn" type="button" ${spikeProxyBusy ? "disabled" : ""}>保存代理</button>
        <button id="spike-proxy-clear-btn" class="mini-btn" type="button" ${spikeProxyBusy ? "disabled" : ""}>清除代理</button>
        <button id="spike-proxy-btn" class="mini-btn" type="button" ${spikeProxyRunning ? "disabled" : ""}>${spikeProxyRunning ? "验证中…" : "验证原生代理"}</button>
        <button id="spike-proxy-connectivity-btn" class="mini-btn" type="button" ${spikeProxyRunning ? "disabled" : ""}>检测连通性</button>
      </div>
      ${proxyHtml}
    </details>
    <details class="spike-panel" data-spike-section="activity">
      <summary>运行记录</summary>
      <div class="spike-trace" id="spike-front-trace" ${spikeTraceEntries.length ? "" : "hidden"}>${spikeTraceLinesHtml()}</div>
    </details>`;
}

function backgroundQuotaLines(): string[] {
  if (backgroundQuotaError) return [backgroundQuotaError];
  if (backgroundQuotaRows === null) return [];
  if (!backgroundQuotaRows.length) return ["没有可同步的非活跃账号。请先退出会话，回到账号管理。"];
  const proxy: Record<string, string> = {
    applied: "已下发", disabled: "未启用", push_failed: "下发失败", blocked: "应用失败，已阻断",
  };
  const network: Record<string, string> = {
    not_attempted: "未发起", loaded: "主文档已提交", parked: "使用已停驻页面",
    navigation_commit_timeout: "主文档提交超时", probe_timeout: "主文档已提交，额度探测超时",
    no_foreground_window: "未取得前台窗口，本轮未发起联网",
    setup_failed: "页面初始化失败", webview_terminated: "WebView 进程终止", runner_failed: "同步组件未返回结果",
  };
  const quota: Record<string, string> = {
    updated: "已更新并保存", unchanged: "已取得，数据未变", not_updated: "未取得可更新的额度",
    signed_out: "登录已失效", account_changed: "账号状态已变化，本次跳过", save_failed: "保存失败",
  };
  return backgroundQuotaRows.map((row) => {
    const http = /^http_error_(\d{3})$/.exec(row.network);
    const navigation = /^navigation_(provisional|committed)_failed(?:_url_(-?\d+))?$/.exec(row.network);
    const networkLabel = http ? `页面请求失败（HTTP ${http[1]}）`
      : navigation ? `联网失败（${navigation[1] === "provisional" ? "主文档提交前" : "主文档提交后"}${navigation[2] ? `，URL 错误 ${navigation[2]}` : ""}）`
      : network[row.network] ?? "未知状态";
    const provider = row.provider === "muse" ? "Muse" : row.provider === "hark" ? "Hark" : "账号";
    return `${provider} ···${row.account} ｜ 代理 ${proxy[row.proxy] ?? "未知状态"} ｜ 联网 ${networkLabel} ｜ 额度 ${quota[row.quota] ?? "未知状态"}`;
  });
}

async function runBackgroundQuotaDiagnostic(): Promise<void> {
  if (backgroundQuotaRunning) return;
  backgroundQuotaRunning = true;
  backgroundQuotaRows = null;
  backgroundQuotaError = "";
  syncSpikeDiag();
  try {
    backgroundQuotaRows = await invoke<BackgroundQuotaDiagnostic[]>("spike_ios_quota_sync_probe");
  } catch (error) {
    backgroundQuotaError = error === "后台额度同步正在运行，请稍后重试"
      ? error
      : "未能启动后台额度同步。";
  } finally {
    backgroundQuotaRunning = false;
    syncSpikeDiag();
  }
}

function syncSpikeDiag(): void {
  const el = document.querySelector<HTMLElement>("#spike-workspace .spike-diag");
  if (!el) return;
  const accountUuid = pickWatchAccount()?.uuid;
  const state = accountUuid ? spikeProxyAccount(accountUuid) : null;
  const staysOnAccount = state && el.dataset.accountUuid === accountUuid;
  const scroller = el.closest<HTMLElement>(".spike-scroll");
  const scrollTop = scroller?.scrollTop ?? 0;
  const openSections = new Set(Array.from(el.querySelectorAll<HTMLDetailsElement>("details[open]"), (section) => section.dataset.spikeSection));
  const focused = document.activeElement;
  const focusedInput = staysOnAccount && focused instanceof HTMLInputElement && el.contains(focused) ? focused : null;
  const selection = focusedInput ? [focusedInput.selectionStart, focusedInput.selectionEnd] : null;
  const previousPassword = staysOnAccount && !state.clearPassword
    ? el.querySelector<HTMLInputElement>("#spike-proxy-pass")?.value
    : null;
  el.dataset.accountUuid = accountUuid ?? "";
  el.innerHTML = spikeDiagInnerHtml(accountUuid);
  el.querySelectorAll<HTMLDetailsElement>("details[data-spike-section]").forEach((section) => {
    section.open = openSections.has(section.dataset.spikeSection);
  });
  const password = el.querySelector<HTMLInputElement>("#spike-proxy-pass");
  if (password && previousPassword) password.value = previousPassword;
  if (focusedInput) {
    const replacement = document.getElementById(focusedInput.id);
    if (replacement instanceof HTMLInputElement) {
      replacement.focus({ preventScroll: true });
      replacement.setSelectionRange(selection?.[0] ?? 0, selection?.[1] ?? 0);
    }
  }
  if (scroller) scroller.scrollTop = scrollTop;
  const wireCopy = document.querySelector<HTMLButtonElement>("#spike-wire-copy-btn");
  if (wireCopy) wireCopy.disabled = !el.querySelector("#spike-wire-trace");
  const accountLabel = document.getElementById("spike-workspace-account");
  if (accountLabel) accountLabel.textContent = pickWatchAccount()?.alias ?? "未登录 Muse 账号";
  if (state) state.clearPassword = false;
  syncDiagProxyView();
}

function setSpikeWorkspaceOpen(open: boolean): void {
  const workspace = document.getElementById("spike-workspace");
  const accounts = document.getElementById("spike-account-view");
  if (!workspace || !accounts) return;
  spikeWorkspaceOpen = open;
  workspace.hidden = !open;
  accounts.hidden = open;
  if (open) syncDiagProxyView();
  document.getElementById(open ? "spike-workspace-title" : "spike-open-btn")?.focus({ preventScroll: true });
}

function ensureProxyView(acc: AccountRecord, refresh = false): Promise<SpikeProxySettingView | null> {
  const account = spikeProxyAccount(acc.uuid);
  if (account.read) return account.read;
  if (account.loaded && !refresh) return Promise.resolve(account.view);
  const pending = invoke<SpikeProxySettingView | null>("muse_native_get_account_proxy", {
    providerId: "muse",
    uuid: acc.uuid,
  }).catch(() => null).then((view) => {
    // A completed save or clear invalidates a refresh already in flight.
    if (account.read === pending) {
      account.view = view;
      account.read = null;
      account.loaded = true;
    }
    if (pickWatchAccount()?.uuid === acc.uuid) syncSpikeDiag();
    return account.view;
  });
  account.read = pending;
  return pending;
}

function syncDiagProxyView(): void {
  const acc = pickWatchAccount();
  if (acc) void ensureProxyView(acc);
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

async function refreshSpikeLive(): Promise<void> {
  // Guards against overlapping polls: a slow bridge must not queue up calls.
  if (spikeLiveRefreshing) return;
  spikeLiveRefreshing = true;
  try {
    spikeLive = await invoke<SpikeLiveness>("muse_native_liveness_status");
    if (!spikeLive.running) spikeLiveTask = null;
    spikeLiveError = null;
  } catch (e) {
    spikeLiveError = String(e);
  } finally {
    spikeLiveRefreshing = false;
  }
  syncSpikeDiag();
}

let spikeLiveTimer: ReturnType<typeof setInterval> | null = null;

function stopLiveTimer(): void {
  if (spikeLiveTimer) {
    clearInterval(spikeLiveTimer);
    spikeLiveTimer = null;
  }
}

function startLiveTimer(): void {
  stopLiveTimer();
  spikeLiveTimer = setInterval(() => {
    if (!spikeLive?.running) {
      stopLiveTimer();
      return;
    }
    void refreshSpikeLive();
  }, 3000);
}

async function startSpikeLive(): Promise<void> {
  if (!snapshot || spikeLiveTask) return;
  const acc = pickWatchAccount();
  if (!acc) {
    spikeLiveError = "没有已登录的 Muse 账号，无法开始存活探针。";
    spikeTrace("存活探针", "中止：没有可用的 Muse 账号");
    syncSpikeDiag();
    return;
  }
  spikeNotice = null;
  const task = { accountUuid: acc.uuid, taskId: createSpikeTaskId() };
  spikeLiveTask = task;
  spikeTrace("存活探针", "调用后端", "muse_native_liveness_start");
  try {
    spikeLive = await invoke<SpikeLiveness>("muse_native_liveness_start", {
      uuid: task.accountUuid,
      taskId: task.taskId,
      seconds: 300,
    });
    spikeLiveError = null;
    if (spikeLive.running) startLiveTimer();
    else spikeLiveTask = null;
  } catch (e) {
    spikeLiveTask = null;
    spikeLiveError = String(e);
  }
  syncSpikeDiag();
}

async function runSpikeLease(): Promise<void> {
  if (!snapshot) return;
  if (spikeLeaseRunning) {
    spikeNotice = "租约探针正在运行，请等待结果。";
    syncSpikeDiag();
    return;
  }
  spikeNotice = null;
  const acc = pickWatchAccount();
  if (!acc) {
    spikeLease = null;
    spikeLeaseError = "没有已登录的 Muse 账号。";
    syncSpikeDiag();
    return;
  }
  const task = { accountUuid: acc.uuid, taskId: createSpikeTaskId() };
  spikeLeaseTask = task;
  spikeLeaseRunning = true;
  spikeLease = null;
  spikeLeaseError = null;
  syncSpikeDiag();
  try {
    spikeLease = await invoke<SpikeLease>("muse_native_lease_probe", {
      providerId: "muse",
      uuid: task.accountUuid,
      taskId: task.taskId,
      twice: true,
    });
  } catch (e) {
    spikeLeaseError = String(e);
  } finally {
    spikeLeaseTask = null;
    spikeLeaseRunning = false;
    syncSpikeDiag();
  }
}

async function runSpikeSend(): Promise<void> {
  if (!snapshot) return;
  if (spikeSendRunning) {
    spikeNotice = "发送探针正在运行，请等待结果。";
    syncSpikeDiag();
    return;
  }
  spikeNotice = null;
  const acc = pickWatchAccount();
  if (!acc) {
    spikeSend = null;
    spikeSendError = "没有已登录的 Muse 账号。";
    syncSpikeDiag();
    return;
  }
  const task = { accountUuid: acc.uuid, taskId: createSpikeTaskId() };
  spikeSendTask = task;
  spikeSendRunning = true;
  spikeSendError = null;
  syncSpikeDiag();
  try {
    // 隔离测试会话：内容是无语义整数，不含隐私，也不发往用户主聊天。
    const probeText = `[spike b4] ${Math.floor(Math.random() * 1e6)}`;
    spikeSend = await invoke<SpikeSendResult>("muse_native_send_probe", {
        providerId: "muse",
        uuid: task.accountUuid,
        taskId: task.taskId,
        text: probeText,
        nodeId: "",
        sessionId: null,
      });
  } catch (e) {
    spikeSend = null;
    spikeSendError = String(e);
  }
  spikeSendTask = null;
  spikeSendRunning = false;
  syncSpikeDiag();
}

// #18 模型指纹：只应对已授权的**非主力测试账号**手动触发。探针内容是
// 无语义整数计数（不含隐私），发往隔离会话；3 条样本不足则显示不可判定。
async function runSpikeFingerprint(): Promise<void> {
  if (!snapshot) return;
  if (spikeFingerprintRunning) {
    spikeNotice = "指纹探针正在运行，请等待结果或点「停止」。";
    syncSpikeDiag();
    return;
  }
  spikeNotice = null;
  const acc = pickWatchAccount();
  if (!acc) {
    spikeFingerprint = null;
    spikeFingerprintError = "没有已登录的 Muse 账号。";
    spikeTrace("指纹探针", "中止：没有可用的 Muse 账号");
    syncSpikeDiag();
    return;
  }
  spikeTrace("指纹探针", "已点击", `账号 ${acc.alias}`);
  const confirmed = await askConfirm({
    title: "运行指纹探针（3 条）",
    desc:
      `将对测试账号「${acc.alias}」的隔离会话发送 3 条无语义计数探针（不是聊天内容），` +
      "采到回答后在本地评分，不会上传第三方。\n" +
      (snapshot.platform === "ios" ? "探针运行期间会保持亮屏，叠号退入后台时会停止。\n" : "") +
      "请确认该账号是非主力测试账号。",
    confirmLabel: "开始探针",
  });
  spikeTrace("指纹探针", confirmed ? "确认框返回 true" : "确认框返回 false（取消）");
  if (!confirmed) {
    spikeNotice = "已取消指纹探针：没有发送任何请求。";
    syncSpikeDiag();
    return;
  }
  const task = { accountUuid: acc.uuid, taskId: createSpikeTaskId() };
  spikeFingerprintTask = task;
  spikeFingerprintProgress = { ...task, phase: "cookies", probe: 0, confirmed: 0, samples: 0 };
  spikeFingerprintRunning = true;
  spikeFingerprint = null;
  spikeFingerprintError = null;
  spikeTrace("指纹探针", "调用后端", "muse_native_fingerprint_probe");
  syncSpikeDiag();
  const watchdog = window.setTimeout(() => {
    void cancelSpikeProbe(task);
  }, 570_000);
  try {
    spikeFingerprint = await invoke<SpikeFingerprintProbe>("muse_native_fingerprint_probe", {
      providerId: "muse",
      uuid: task.accountUuid,
      taskId: task.taskId,
    });
    spikeTrace("指纹探针", "后端已返回", `阶段 ${spikeFingerprint.phase}`);
  } catch (e) {
    spikeFingerprintError = String(e);
    spikeTrace("指纹探针", "后端失败", String(e));
    void cancelSpikeProbe(task);
  } finally {
    window.clearTimeout(watchdog);
    spikeFingerprintRunning = false;
    spikeFingerprintTask = null;
    spikeFingerprintProgress = null;
    syncSpikeDiag();
  }
}

// 保存代理后的结果摘要：把三层状态分开写，不让“Rust 侧已保存”被误读成
// “整个账号已生效”。三行分别是 Rust 配置 / Keychain 凭据 / WebView 下发。
function proxySaveSummary(view: SpikeProxySettingView | null): string[] {
  if (!view) {
    return [
      "保存请求已返回，但没有拿到代理配置视图。",
      "Rust 配置 / Keychain / WebView 三层状态均未知，请刷新后重新确认。",
    ];
  }
  const lines = [
    `Rust 配置：已保存（${view.enabled ? "已启用" : "未启用"}）${view.display}`,
    view.hasCredentials
      ? (view.credentialAvailable ? "Keychain：口令可读" : "Keychain：保存了凭据引用，口令暂不可读")
      : "Keychain：未使用认证凭据",
  ];
  const webview = view.webview;
  if (!webview) {
    lines.push("WebView：该平台没有这一层（桌面走系统代理；Android 未接入）");
  } else if (webview.applied) {
    lines.push(
      webview.sessionOpen
        ? "WebView：已写入，当前会话需重新打开后生效"
        : "WebView：已写入"
    );
  } else if (webview.quarantined) {
    lines.push(
      `WebView：未生效，出口已隔离（不会直连）：${webview.reason ?? "原因未返回"}`
    );
  } else {
    lines.push(
      `WebView：未生效：${webview.reason ?? "该设备不支持 WebView 代理"}`
    );
  }
  return lines;
}

// 代理 URI 脱敏：确认弹窗和诊断只显示 scheme://host:port。
function redactProxyUri(uri: string): string {
  try {
    const url = new URL(uri);
    if (url.hostname && ["http:", "https:", "socks5:", "socks5h:"].includes(url.protocol)) {
      return `${url.protocol}//${url.host}`;
    }
  } catch {
    // 保留统一的校验提示，避免把无法解析的输入写入诊断。
  }
  return "（代理 URI 格式无效）";
}

// #11/#16 代理探针：用**该账号已保存的代理**（ProxySetting + Keychain）验证
// 控制面 + WSS 隧道，与真实发送同源；前端不再把 URI/口令送进后端。
// 失败 fail-closed，不回退直连。
async function runSpikeProxy(connectivityOnly = false): Promise<void> {
  if (!snapshot) return;
  if (spikeProxyRunning) {
    spikeNotice = "代理探针正在运行，请等待结果或点「停止」。";
    syncSpikeDiag();
    return;
  }
  spikeNotice = null;
  const acc = pickWatchAccount();
  if (!acc) {
    spikeNotice = "没有已登录的 Muse 账号。";
    spikeTrace("代理探针", "中止：没有可用的 Muse 账号");
    syncSpikeDiag();
    return;
  }
  // 诊断只认该账号已保存的代理（ProxySetting + Keychain），
  // 不再把输入框里的 URI / 口令送进后端，避免「诊断与实际发送不同源」。
  const account = spikeProxyAccount(acc.uuid);
  const view = await ensureProxyView(acc, true);
  if (!view || !view.enabled) {
    account.result = null;
    account.error = "该账号还没有启用代理：请先「保存为账号代理」再跑代理探针。";
    spikeTrace("代理探针", "中止：账号未启用代理", `账号 ${acc.alias}`);
    syncSpikeDiag();
    return;
  }
  spikeTrace("代理探针", "已点击", `账号 ${acc.alias}，代理 ${view.display}`);
  const confirmed = await askConfirm({
    title: connectivityOnly ? "检测代理连通性" : "运行代理探针",
    desc:
      `将对测试账号「${acc.alias}」用已保存的代理 ${view.display} 跑一次隧道验证` +
      (connectivityOnly ? "（向 muse.ai 发起匿名 HTTPS 请求）。\n" : "（控制面 + WSS + ping）。\n") +
      "请确认这是你自己的测试代理、该账号是非主力测试账号。",
    confirmLabel: "开始验证",
  });
  spikeTrace("代理探针", confirmed ? "确认框返回 true" : "确认框返回 false（取消）");
  if (!confirmed) {
    spikeNotice = "已取消代理探针：没有发起任何连接。";
    syncSpikeDiag();
    return;
  }
  const task = { accountUuid: acc.uuid, taskId: createSpikeTaskId() };
  spikeProxyTask = task;
  spikeProxyRunning = true;
  account.result = null;
  account.error = null;
  spikeTrace("代理探针", "调用后端", "muse_native_proxy_probe");
  syncSpikeDiag();
  try {
    account.result = await Promise.race([
      invoke<SpikeProxyProbe>("muse_native_proxy_probe", {
        providerId: "muse",
        uuid: acc.uuid,
        connectivityOnly,
        taskId: task.taskId,
      }),
      new Promise<SpikeProxyProbe>((_, reject) =>
        setTimeout(() => reject(new Error("代理探针超时（90 秒）")), 90_000)
      ),
    ]);
    spikeTrace("代理探针", "后端已返回", `阶段 ${account.result.phase}`);
  } catch (e) {
    account.result = null;
    account.error = String(e);
    spikeTrace("代理探针", "后端失败/超时", String(e));
    // 前端超时/异常不能代替后端取消：显式请求后端停止，避免继续发请求。
    void cancelSpikeProbe(task);
  }
  spikeProxyRunning = false;
  spikeProxyTask = null;
  syncSpikeDiag();
}

// 保存账号级代理：口令只进 Keychain，前端与 accounts.json 都不保留明文。
async function saveSpikeProxy(): Promise<void> {
  if (!snapshot) return;
  if (spikeProxyBusy) {
    spikeNotice = "上一次代理操作还在进行，请稍候。";
    syncSpikeDiag();
    return;
  }
  spikeNotice = null;
  const acc = pickWatchAccount();
  if (!acc) {
    spikeNotice = "保存中止：没有已登录的 Muse 账号。";
    spikeTrace("保存代理", "中止：没有可用的 Muse 账号");
    syncSpikeDiag();
    return;
  }
  const proxyAccount = spikeProxyAccount(acc.uuid);
  const uri = (document.querySelector<HTMLInputElement>("#spike-proxy-uri")?.value ?? "").trim();
  const username = (document.querySelector<HTMLInputElement>("#spike-proxy-user")?.value ?? "").trim();
  const password = document.querySelector<HTMLInputElement>("#spike-proxy-pass")?.value ?? "";
  proxyAccount.uri = uri;
  proxyAccount.username = username;
  if (!uri) {
    proxyAccount.saveLines = ["请填写代理 URI（例如 socks5://host:1080）。"];
    proxyAccount.saveOk = false;
    spikeTrace("保存代理", "校验失败：URI 为空");
    syncSpikeDiag();
    return;
  }
  const redacted = redactProxyUri(uri);
  spikeTrace("保存代理", "已点击", `账号 ${acc.alias}，代理 ${redacted}`);
  const confirmed = await askConfirm({
    title: "保存账号代理",
    desc:
      `把代理 ${redacted} 保存为账号「${acc.alias}」的专用代理？\n` +
      "口令只写入系统 Keychain，不写入 accounts.json。",
    confirmLabel: "保存",
  });
  spikeTrace("保存代理", confirmed ? "确认框返回 true" : "确认框返回 false（取消）");
  if (!confirmed) {
    spikeNotice = "已取消保存：代理配置未改动。";
    syncSpikeDiag();
    return;
  }
  spikeProxyBusy = true;
  proxyAccount.saveLines = ["保存中…"];
  proxyAccount.saveOk = null;
  spikeTrace("保存代理", "调用后端", "muse_native_set_account_proxy");
  syncSpikeDiag();
  try {
    proxyAccount.view = await invoke<SpikeProxySettingView>(
      "muse_native_set_account_proxy",
      {
        providerId: "muse",
        uuid: acc.uuid,
        uri,
        username: username || null,
        password: password || null,
        enabled: true,
      }
    );
    proxyAccount.loaded = true;
    proxyAccount.read = null;
    proxyAccount.result = null;
    proxyAccount.error = null;
    proxyAccount.saveLines = proxySaveSummary(proxyAccount.view);
    proxyAccount.saveOk = true;
    // 口令已进 Keychain：从界面上清掉，避免留在输入框里。
    proxyAccount.clearPassword = true;
    spikeTrace("保存代理", "后端已返回", "Rust 配置已保存");
  } catch (e) {
    proxyAccount.saveLines = [
      `保存失败：${String(e)}`,
      "Rust 配置可能未落盘（失败可能发生在校验 / 隔离 / 写入任一阶段）；" +
        "按 fail-closed 设计，WebView 出口不会回退直连。",
    ];
    proxyAccount.saveOk = false;
    spikeTrace("保存代理", "后端失败", String(e));
  }
  spikeProxyBusy = false;
  syncSpikeDiag();
}

async function clearSpikeProxy(): Promise<void> {
  if (!snapshot) return;
  if (spikeProxyBusy) {
    spikeNotice = "上一次代理操作还在进行，请稍候。";
    syncSpikeDiag();
    return;
  }
  spikeNotice = null;
  const acc = pickWatchAccount();
  if (!acc) {
    spikeNotice = "清除中止：没有已登录的 Muse 账号。";
    spikeTrace("清除代理", "中止：没有可用的 Muse 账号");
    syncSpikeDiag();
    return;
  }
  const proxyAccount = spikeProxyAccount(acc.uuid);
  spikeTrace("清除代理", "已点击", `账号 ${acc.alias}`);
  const confirmed = await askConfirm({
    title: "清除账号代理",
    desc:
      `清除账号「${acc.alias}」的代理配置？\n` +
      "清除后该账号走系统网络；Keychain 中的口令一并删除。",
    confirmLabel: "清除",
    danger: true,
  });
  spikeTrace("清除代理", confirmed ? "确认框返回 true" : "确认框返回 false（取消）");
  if (!confirmed) {
    spikeNotice = "已取消清除：代理配置未改动。";
    syncSpikeDiag();
    return;
  }
  spikeProxyBusy = true;
  proxyAccount.saveLines = ["清除中…"];
  proxyAccount.saveOk = null;
  spikeTrace("清除代理", "调用后端", "muse_native_clear_account_proxy");
  syncSpikeDiag();
  try {
    const webview = await invoke<SpikeWebviewProxyView | null>("muse_native_clear_account_proxy", {
      providerId: "muse",
      uuid: acc.uuid,
    });
    proxyAccount.view = null;
    proxyAccount.loaded = true;
    proxyAccount.read = null;
    proxyAccount.result = null;
    proxyAccount.error = null;
    proxyAccount.clearPassword = true;
    proxyAccount.saveLines = [
      "Rust 配置：已清除（该账号恢复走系统网络）",
      "Keychain：口令已删除（包括旧的 proxy-<uuid> 引用）",
      webview?.applied
        ? "WebView：已清除，重新打开账号后验证"
        : `WebView：${webview?.reason ?? "未获得下发结果"}`,
    ];
    proxyAccount.saveOk = webview?.applied ?? null;
    spikeTrace("清除代理", "后端已返回");
  } catch (e) {
    proxyAccount.saveLines = [`清除失败：${String(e)}`];
    proxyAccount.saveOk = false;
    spikeTrace("清除代理", "后端失败", String(e));
  }
  spikeProxyBusy = false;
  syncSpikeDiag();
}

// 后端取消：Promise.race 超时后仍会调用它，确保 Rust 侧停止发起新请求。
function createSpikeTaskId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

async function cancelSpikeProbe(task?: SpikeFingerprintTask): Promise<void> {
  const tasks = task ? [task] : [spikeFingerprintTask, spikeProxyTask, spikeLeaseTask, spikeLiveTask, spikeSendTask, spikeWatchTask]
    .filter((entry): entry is SpikeFingerprintTask => entry !== null);
  const uuid = pickWatchAccount()?.uuid;
  if (!uuid && !tasks.length) {
    spikeNotice = "停止失败：没有已登录的 Muse 账号。";
    syncSpikeDiag();
    return;
  }
  try {
    const hits = tasks.length
      ? await Promise.all(tasks.map((entry) => invoke<number>("muse_native_cancel_probe", {
        providerId: "muse", uuid: entry.accountUuid, taskId: entry.taskId,
      })))
      : [await invoke<number>("muse_native_cancel_probe", { providerId: "muse", uuid, taskId: null })];
    const hit = hits.reduce((total, count) => total + count, 0);
    spikeNotice =
      hit > 0
        ? `已请求停止 ${hit} 个任务：不再发起新的请求/发送`
        : "没有正在运行的长任务";
    spikeTrace("停止", "已请求后端取消", `命中 ${hit} 个任务`);
  } catch (e) {
    spikeNotice = `停止失败：${String(e)}`;
    spikeTrace("停止", "后端失败", String(e));
  }
  syncSpikeDiag();
}

async function runSpikeQuota(): Promise<void> {
  if (!snapshot) return;
  if (spikeQuotaRunning) {
    spikeNotice = "额度探针正在运行，请等待结果。";
    syncSpikeDiag();
    return;
  }
  spikeNotice = null;
  const acc = pickWatchAccount();
  if (!acc) {
    spikeQuotaProbe = null;
    spikeQuotaError = "没有已登录的 Muse 账号。";
    syncSpikeDiag();
    return;
  }
  spikeQuotaRunning = true;
  spikeQuotaError = null;
  syncSpikeDiag();
  try {
    spikeQuotaProbe = await Promise.race([
      invoke<SpikeQuotaProbe>("muse_native_quota_probe", { providerId: "muse", uuid: acc.uuid }),
      new Promise<SpikeQuotaProbe>((_, reject) =>
        setTimeout(() => reject(new Error("额度探针超时（60 秒）")), 60_000)
      ),
    ]);
  } catch (e) {
    spikeQuotaProbe = null;
    spikeQuotaError = String(e);
  }
  spikeQuotaRunning = false;
  syncSpikeDiag();
}

async function runSpikeApprovals(): Promise<void> {
  if (!snapshot) return;
  if (spikeApprovalsRunning) {
    spikeNotice = "审批探针正在运行，请等待结果。";
    syncSpikeDiag();
    return;
  }
  spikeNotice = null;
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
  if (!snapshot) return;
  if (spikeWatchRunning) {
    spikeNotice = "订阅探针正在运行，请等待结果。";
    syncSpikeDiag();
    return;
  }
  spikeNotice = null;
  const acc = pickWatchAccount();
  spikeWatchAccount = acc?.alias ?? null;
  if (!acc) {
    spikeWatch = null;
    spikeWatchError = "没有已登录的 Muse 账号，请先登录一个测试账号。";
    syncSpikeDiag();
    return;
  }
  const task = { accountUuid: acc.uuid, taskId: createSpikeTaskId() };
  spikeWatchTask = task;
  spikeWatchRunning = true;
  spikeWatchError = null;
  syncSpikeDiag();
  try {
    spikeWatch = await invoke<SpikeWatchResult>("muse_native_watch_probe", {
        providerId: "muse",
        uuid: task.accountUuid,
        taskId: task.taskId,
        seconds: 15,
      });
  } catch (e) {
    spikeWatch = null;
    spikeWatchError = String(e);
  }
  spikeWatchTask = null;
  spikeWatchRunning = false;
  syncSpikeDiag();
}

async function runSpikeDiagnose(): Promise<void> {
  if (!snapshot) return;
  if (spikeDiagnoseRunning) {
    spikeNotice = "诊断正在运行，请等待结果。";
    syncSpikeDiag();
    return;
  }
  spikeNotice = null;
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

function renderApprovalsModal(view: Extract<ModalMode, { kind: "approvals" }>): string {
  const { account, loading, result, fetchedAt, decisionMessage } = view;
  const formatTime = (ts: number) => ts > 0 && ts < 8.64e15 ? new Date(ts).toLocaleString() : "未提供";
  const reason = result?.phase === "schema" || result?.phase === "parse"
    ? "审批响应格式无法识别，请打开账号核对。"
    : result?.phase === "no_cookies" || result?.status === 401 || result?.status === 403
      ? "账号登录态或权限不可用，请打开账号核对。"
      : "审批查询失败，当前待审批状态未知。";
  const items = result?.items ?? [];
  const content = loading ? "正在读取审批…" : !result?.ok ? reason : !items.length ? "当前没有待审批项。" : `
    <div class="approval-items">${items.map((item, index) => `
      <details class="approval-item">
        <summary>${escapeHtml(item.displayName || item.kind || "审批请求")} · ${item.expiresAtMs > 0 && item.expiresAtMs <= Date.now() ? "已过期" : escapeHtml(item.status === "pending" ? "待审批" : item.status)}</summary>
        <div>类型 ${escapeHtml(item.kind || "未提供")}</div>
        <div>请求时间 ${formatTime(item.requestedAtMs)}</div>
        <div>到期时间 ${formatTime(item.expiresAtMs)}</div>
        ${item.status === "pending" && item.expiresAtMs > Date.now() && item.sessionScopeId ?
          `<div class="proxy-pool-actions">${(item.choices ?? []).map((choice, choiceIndex) => {
            const label = { allow_once: "允许一次", deny: "拒绝", allow_always: "始终允许" }[choice.decision];
            if (!label || (choice.decision === "allow_always" && !choice.scope)) return "";
            return `<button class="mini-btn" type="button" data-approval-item="${index}" data-approval-choice="${choiceIndex}" ${loading ? "disabled" : ""}>${label}</button>`;
          }).join("")}</div>` : ""}
      </details>`).join("")}</div>`;
  return `
    <div class="inline-modal" role="dialog" aria-modal="true" aria-labelledby="approval-title">
      <div class="modal-card approval-modal">
        <div class="modal-title" id="approval-title">待审批 · ${escapeHtml(account.alias)}</div>
        <div class="modal-desc">提交前会复查账号、会话与有效期。${fetchedAt ? ` 最近读取 ${formatTime(fetchedAt)}` : ""}</div>
        ${result?.ok && !loading && items.length ? content : `<div role="status">${content}</div>`}
        ${decisionMessage ? `<div role="status">${escapeHtml(decisionMessage)}</div>` : ""}
        <div class="modal-footer">
          <button id="modal-cancel-btn" class="btn" type="button">关闭</button>
          <button id="approvals-refresh-btn" class="btn" type="button" ${loading ? "disabled" : ""}>刷新</button>
          <button id="approvals-open-account-btn" class="btn primary" type="button">打开账号</button>
        </div>
      </div>
    </div>`;
}

async function loadAccountApprovals(view: Extract<ModalMode, { kind: "approvals" }>): Promise<void> {
  if (modal !== view) return;
  view.loading = true;
  view.result = null;
  view.fetchedAt = null;
  render();
  try {
    view.result = await invoke<SpikeApprovalsResult>("muse_native_approvals_probe", {
      providerId: view.account.providerId, uuid: view.account.uuid,
    });
    if (view.result.ok) view.fetchedAt = Date.now();
  } catch {
    view.result = null;
  } finally {
    view.loading = false;
    if (modal === view) render();
  }
}

function formatAccountClock(ms: number, timezone?: string): string {
  try {
    return new Date(ms).toLocaleString(undefined, { timeZone: timezone });
  } catch {
    return new Date(ms).toLocaleString();
  }
}

function renderTasks(view: Extract<ModalMode, { kind: "tasks" }>): string {
  const { account, checking, note } = view;
  const current = snapshot?.accounts.find((entry) => entry.uuid === account.uuid && entry.providerId === account.providerId);
  const observation = latestTaskObservations.get(`${account.providerId}:${account.uuid}`);
  const status: Record<TaskObservation["activity"], string> = {
    working: "任务进行中", idle: "未见任务正在生成", approval: "等待审批", unknown: "暂不可判定",
  };
  const recent = current?.lastTaskReplyAtMs;
  const unreadStatus: Record<string, string> = {
    unread: "Muse 返回未读", zero: "服务已返回未读计数，本次为零",
    unavailable: "本次未收到未读计数", network_or_protocol: "未读查询失败", timeout: "未读查询超时",
    history_completed: "会话历史确认有新回复",
    history_no_completion: "会话历史暂未读到本轮的新完成回复",
    history_unavailable: "暂无法从会话历史确认回复",
    no_thread: "停驻页未提供可核对的会话，未见未读计数",
  };
  return `
    <div class="inline-modal" role="dialog" aria-modal="true" aria-labelledby="account-task-title">
      <div class="modal-card">
        <div class="modal-title" id="account-task-title">任务 · ${escapeHtml(account.alias)}</div>
        <div role="status">${checking ? "正在检测…" : escapeHtml(status[observation?.activity ?? "unknown"])}</div>
        ${observation?.observedAtMs ? `<div class="modal-desc">最近一次成功检测 ${new Date(observation.observedAtMs).toLocaleString()} · ${observation.source === "native" ? "原生" : "停驻页面"}</div>` : ""}
        ${current?.pendingApprovalCount != null ? `<div class="modal-desc">待审批 ${current.pendingApprovalCount} 项</div>` : ""}
        ${recent ? `<div class="modal-desc">最近发现回复 ${new Date(recent).toLocaleString()}</div>` : ""}
        ${current?.museUnreadStatus ? `<div class="modal-desc">回前台补查 · ${unreadStatus[current.museUnreadStatus] ?? "暂不可判定"}</div>` : ""}
        ${note ? `<div class="modal-desc">${escapeHtml(note)}</div>` : ""}
        ${current?.quotaObservation ? (() => {
          const quota = current.quotaObservation;
          const stale = quota.stale || Date.now() - quota.checkedAtMs > 10 * 60_000
            || (quota.resetsAtMs !== null && Date.now() >= quota.resetsAtMs);
          return `<div class="modal-desc">额度 ${stale ? "已过期" : "已同步"} · ${quota.source === "native" ? "原生" : "网页"} · ${formatAccountClock(quota.checkedAtMs, current.timezone)}
            <div>已用 ${quota.percentUsed != null ? `${quota.percentUsed}%` : "未观测"} · 窗口 ${escapeHtml(quota.window ?? "未观测")}</div>
            <div>额度重置 ${quota.resetsAtMs ? formatAccountClock(quota.resetsAtMs, current.timezone) : "未观测"}</div></div>`;
        })() : ""}
        <div class="modal-desc">自动检测在前台运行，低电量模式下暂停。回前台后会补查停驻页面。</div>
        <label class="proxy-pool-actions"><input id="account-task-notifications" type="checkbox" ${current?.taskNotifications ? "checked" : ""}/> 监测并在系统允许时提醒</label>
        <div class="modal-footer">
          <button id="modal-cancel-btn" class="btn" type="button">关闭</button>
          ${account.providerId === "muse" ? `<button id="account-quota-refresh" class="btn" type="button" ${view.quotaChecking ? "disabled" : ""}>刷新额度</button>` : ""}
          ${account.providerId === "hark" ? `<button id="account-task-open" class="btn" type="button">打开会话查看审批</button>` : ""}
          <button id="account-task-refresh" class="btn primary" type="button" ${checking ? "disabled" : ""}>刷新</button>
        </div>
      </div>
    </div>`;
}

async function checkAccountTasks(view: Extract<ModalMode, { kind: "tasks" }>): Promise<void> {
  if (modal !== view || view.checking || document.hidden) return;
  view.checking = true;
  view.note = "";
  render();
  const { uuid, providerId } = view.account;
  const key = `${providerId}:${uuid}`;
  try {
    const result = await invoke<TaskObservation>("observe_account_tasks", {
      uuid, providerId, timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
    });
    applyTaskObservation(key, result);
  } catch (error) {
    applyTaskObservation(key, { activity: "unknown", observedAtMs: null, source: null });
    view.note = String(error);
  } finally {
    view.checking = false;
    if (modal === view) render();
  }
}

function renderAccountBackup(view: Extract<ModalMode, { kind: "backup" }>): string {
  const { busy, fileName, preview, message, mode, includeGroups, includeProxy } = view;
  const disabled = busy ? "disabled" : "";
  return `
    <div class="inline-modal" role="dialog" aria-modal="true" aria-labelledby="account-backup-title">
      <div class="modal-card proxy-pool-modal">
        <div class="modal-title" id="account-backup-title">本机加密备份</div>
        <div class="modal-desc">备份包含账号资料。网页登录、Cookie 和代理口令需在恢复后重新配置。</div>
        <div class="proxy-pool-body">
          <label for="backup-mode">备份范围</label>
          <select id="backup-mode" class="modal-input" ${disabled}>
            <option value="light" ${mode === "light" ? "selected" : ""}>轻量（账号、备注及分组）</option>
            <option value="full" ${mode === "full" ? "selected" : ""}>全量（含代理池及账号代理端点）</option>
            <option value="custom" ${mode === "custom" ? "selected" : ""}>自定义</option>
          </select>
          ${mode === "custom" ? `<div class="proxy-pool-actions">
            <label><input id="backup-include-groups" type="checkbox" ${includeGroups ? "checked" : ""} ${disabled} />分组</label>
            <label><input id="backup-include-proxy" type="checkbox" ${includeProxy ? "checked" : ""} ${disabled} />账号代理端点</label>
          </div>` : ""}
          <label for="backup-expiry">导出有效期</label>
          <select id="backup-expiry" class="modal-input" ${disabled}>
            ${[[0, "长期保留"], [1, "1 天"], [7, "7 天"], [30, "30 天"]].map(([days, title]) =>
              `<option value="${days}" ${view.expiryDays === days ? "selected" : ""}>${title}</option>`
            ).join("")}
          </select>
          <label for="backup-file">选择加密备份以恢复</label>
          <input id="backup-file" type="file" class="modal-input" accept=".maambackup" ${disabled} />
          ${fileName ? `<div>${escapeHtml(fileName)}</div>` : ""}
          <label for="backup-password">${preview ? "再次输入备份口令确认恢复" : "备份口令（至少 8 字符）"}</label>
          <input id="backup-password" type="password" autocomplete="off" class="modal-input" ${disabled} />
          ${preview ? `<div class="approval-items">
            <div>新增 ${preview.added} · 已有 ${preview.existing} · 代理池 ${preview.proxyPoolCount} 条</div>
            ${preview.conflicts.map((conflict) => `<div>冲突 · ${escapeHtml(conflict)}</div>`).join("")}
          </div>` : ""}
        </div>
        ${message ? `<div role="status">${escapeHtml(message)}</div>` : ""}
        <div class="modal-footer account-cookie-actions">
          <button id="modal-cancel-btn" type="button" class="btn" ${disabled}>关闭</button>
          <button id="backup-export" type="button" class="btn primary" ${disabled}>导出备份</button>
          <button id="backup-preview" type="button" class="btn" ${disabled}>预览恢复</button>
          ${preview ? `<button id="backup-merge" type="button" class="btn" ${disabled}>合并</button><button id="backup-overwrite" type="button" class="btn" ${disabled}>覆盖冲突资料</button>` : ""}
        </div>
      </div>
    </div>`;
}

async function accountBackupAction(view: Extract<ModalMode, { kind: "backup" }>, action: "export" | "preview" | "merge" | "overwrite"): Promise<void> {
  if (modal !== view || view.busy) return;
  const passwordInput = document.querySelector<HTMLInputElement>("#backup-password");
  const password = passwordInput?.value ?? "";
  if (passwordInput) passwordInput.value = "";
  if (action === "overwrite" && !await askConfirm({
    title: "覆盖冲突资料",
    desc: "备份中的备注和分组会替换匹配账号的同名资料。",
    confirmLabel: "覆盖",
    danger: true,
  })) {
    if ((modal as ModalMode).kind === "none") await setModal(view);
    return;
  }
  if (action === "overwrite") await setModal(view);
  view.busy = true;
  view.message = "处理中…";
  render();
  try {
    if (action === "export") {
      const shared = await invoke<boolean>("export_account_backup", {
        password, expiryDays: view.expiryDays, includeGroups: view.mode !== "custom" || view.includeGroups,
        includeProxyEndpoints: view.mode === "full" || (view.mode === "custom" && view.includeProxy),
      });
      view.message = shared ? "备份已导出。" : "已取消导出。";
    } else if (action === "preview") {
      if (!view.encrypted) throw new Error("请先选择备份文件");
      view.preview = await invoke<BackupPreview>("preview_account_backup", { encrypted: view.encrypted, password });
      view.message = "";
    } else if (view.preview) {
      snapshot = await invoke<ManagerSnapshot>("restore_account_backup", {
        encrypted: view.encrypted, password, overwrite: action === "overwrite", previewChecksum: view.preview.checksum,
      });
      view.preview = null;
      view.encrypted = "";
      view.fileName = "";
      view.message = "账号资料已恢复。";
    }
  } catch (error) {
    if (action !== "export") view.preview = null;
    view.message = String(error);
  } finally {
    view.busy = false;
    if (modal === view) render();
  }
}

function renderProxyPool(view: Extract<ModalMode, { kind: "proxyPool" }>): string {
  const { account, data, setting, message, busy } = view;
  const disabled = busy ? "disabled" : "";
  const action = (label: string, name: string, id = "") =>
    `<button type="button" class="mini-btn" data-proxy-pool-action="${name}" data-pool-id="${escapeHtml(id)}" ${disabled}>${label}</button>`;
  return `
    <div class="inline-modal" role="dialog" aria-modal="true" aria-labelledby="proxy-pool-title">
      <div class="modal-card proxy-pool-modal">
        <div class="modal-title" id="proxy-pool-title">代理池 · ${escapeHtml(account.alias)}</div>
        <div class="modal-desc">${setting?.enabled ? `当前 ${escapeHtml(setting.display)}` : "当前账号未启用代理"}</div>
        <div class="proxy-pool-body">
          <label for="proxy-pool-input">代理 URI（每行一个）或 HTTPS 订阅</label>
          <div class="modal-desc">单个代理地址用「导入代理」。订阅请粘贴服务商的 HTTPS 下载链接，链接须直接返回纯文本，每行一个 http://、https:// 或 socks5:// 代理 URI。</div>
          <textarea id="proxy-pool-input" class="modal-input account-cookie-input" spellcheck="false" autocomplete="off" autocapitalize="off" ${disabled}></textarea>
          <div class="proxy-pool-actions">${action("导入代理", "import")}${action("添加订阅", "subscribe")}${setting?.enabled ? action("测速", "measure") + action("清除账号代理", "clear") : ""}</div>
          ${data?.sources.map((source) => `<div class="proxy-pool-row">
            <div>${escapeHtml(source.host)}<small>上次刷新 ${new Date(source.refreshedAt * 1000).toLocaleString()}</small></div>
            <div>${action("刷新", "refresh", source.id)}${action("删除订阅", "remove-source", source.id)}</div>
          </div>`).join("") ?? ""}
          ${data?.entries.map((entry) => `<div class="proxy-pool-row">
            <div>${escapeHtml(entry.endpoint)}${entry.missingCredentials ? "<small>待补认证</small>" : entry.requiresAuth ? "<small>需要认证</small>" : ""}</div>
            <div>${entry.missingCredentials ? "" : action("用此代理", "apply", entry.id)}${entry.sourceId ? "" : action("移除", "remove", entry.id)}</div>
          </div>`).join("") ?? ""}
        </div>
        ${message ? `<div role="status">${escapeHtml(message)}</div>` : ""}
        <div class="modal-footer">
          <button id="modal-cancel-btn" type="button" class="btn" ${disabled}>关闭</button>
        </div>
      </div>
    </div>`;
}

async function proxyPoolAction(view: Extract<ModalMode, { kind: "proxyPool" }>, action: string, entryId: string | null): Promise<void> {
  if (modal !== view || view.busy) return;
  const input = document.querySelector<HTMLTextAreaElement>("#proxy-pool-input");
  const secret = input?.value.trim() ?? "";
  if (input) input.value = "";
  const { providerId, uuid } = view.account;
  if (action === "apply" || action === "remove-source" || action === "clear") {
    const confirm = action === "apply" ? {
      title: "更换账号代理",
      desc: `将所选代理用于「${view.account.alias}」，已打开的会话需要重新打开。`,
      confirmLabel: "应用",
    } : action === "clear" ? {
      title: "清除账号代理",
      desc: `清除后「${view.account.alias}」将使用系统网络。`,
      confirmLabel: "清除",
      danger: true,
    } : {
      title: "删除订阅",
      desc: "删除订阅及其中的池条目。已经分配给账号的代理继续保留。",
      confirmLabel: "删除",
      danger: true,
    };
    if (!await askConfirm(confirm)) {
      if ((modal as ModalMode).kind === "none") await setModal(view);
      return;
    }
    await setModal(view);
  }

  view.busy = true;
  view.message = "处理中…";
  render();
  try {
    if (action === "apply") {
      view.setting = await invoke<SpikeProxySettingView>("apply_proxy_pool_entry", { providerId, uuid, entryId });
      view.message = proxySaveSummary(view.setting).join(" · ");
    } else if (action === "clear") {
      await invoke("muse_native_clear_account_proxy", { providerId, uuid });
      view.setting = null;
      view.message = "账号代理已清除。";
    } else if (action === "measure") {
      const result = await invoke<{ milliseconds: number; status: number }>("measure_account_proxy", { providerId, uuid });
      view.message = `代理 HTTPS · HTTP ${result.status} · ${result.milliseconds} ms`;
    } else {
      const poolAction = action === "remove-source" ? "remove" : action;
      view.data = await invoke<ProxyPoolView>("manage_proxy_pool", {
        action: poolAction, entryId, input: poolAction === "import" || poolAction === "subscribe" ? secret : null,
      });
      if (action === "list") {
        view.setting = await invoke<SpikeProxySettingView | null>("muse_native_get_account_proxy", { providerId, uuid });
      }
      view.message = "";
    }
  } catch (error) {
    view.message = String(error);
  } finally {
    view.busy = false;
    if (modal === view) render();
  }
}

function renderSessionsModal(view: Extract<ModalMode, { kind: "sessions" }>): string {
  const { account, loading, items, error } = view;
  const rows = items?.map((item) => `
    <button type="button" class="muse-thread-row" data-muse-thread="${escapeHtml(item.id)}">
      <span>${escapeHtml(item.title)}${item.id === account.lastMuseThread ? " · 上次打开" : ""}</span>
      <small>${item.archived ? "已归档 · " : item.pinned ? "已置顶 · " : ""}${item.updatedAtMs ? new Date(item.updatedAtMs).toLocaleString() : ""}</small>
    </button>`).join("");
  return `
    <div class="inline-modal" role="dialog" aria-modal="true" aria-labelledby="muse-sessions-title">
      <div class="modal-card">
        <div class="modal-title" id="muse-sessions-title">会话 · ${escapeHtml(account.alias)}</div>
        ${rows ? `<div class="approval-items">${rows}</div>` : `<div role="status">${escapeHtml(loading ? "正在读取会话…" : error || "该账号没有会话。")}</div>`}
        ${error && account.lastMuseThread ? `<button type="button" class="muse-thread-row" data-muse-thread="${escapeHtml(account.lastMuseThread)}">重新打开上次会话</button>` : ""}
        <div class="modal-footer">
          <button id="modal-cancel-btn" type="button" class="btn">关闭</button>
          <button id="muse-sessions-refresh" type="button" class="btn" ${loading ? "disabled" : ""}>刷新</button>
        </div>
      </div>
    </div>`;
}

async function loadMuseSessions(view: Extract<ModalMode, { kind: "sessions" }>): Promise<void> {
  if (modal !== view) return;
  view.loading = true;
  view.items = null;
  view.error = "";
  render();
  try {
    view.items = await invoke<MuseSession[]>("list_muse_sessions", { uuid: view.account.uuid });
  } catch (error) {
    view.error = String(error);
  } finally {
    view.loading = false;
    if (modal === view) render();
  }
}

function renderCookieModal(view: Extract<ModalMode, { kind: "cookies" }>): string {
  const { account, busy, preview, message, imported, stored } = view;
  return `
    <div class="inline-modal" role="dialog" aria-modal="true" aria-labelledby="cookie-title">
      <div class="modal-card">
        <div class="modal-title" id="cookie-title">登录恢复 · ${escapeHtml(account.alias)}</div>
        <div class="modal-desc">先粘贴自己的 Cookie 导出内容，再点「预览」。本机 Cookie 数量与下方输入框分开显示。</div>
        <div class="modal-desc">已记录的登录状态 ${account.authState === "authenticated" ? "已登录" : account.authState === "logged_out" ? "需登录" : "未验证"}。</div>
        <div class="modal-desc" id="account-cookie-stored">${stored?.count ? `本机会话有 ${stored.count} 条 Cookie · 最早已知到期 ${stored.nextExpiry ? new Date(stored.nextExpiry * 1000).toLocaleString() : "未提供"}` : stored ? "暂未读到本机 Cookie。可打开此账号确认登录后再查看。" : "本机 Cookie 状态暂不可用。"}</div>
        <label for="account-cookie-input">Cookie</label>
        <textarea id="account-cookie-input" class="modal-input account-cookie-input" placeholder="在这里粘贴 JSON、Netscape 或 Cookie 请求头" spellcheck="false" autocomplete="off" autocapitalize="off" ${busy ? "readonly" : ""}></textarea>
        <div id="account-cookie-preview" role="status">${preview ? `
          共 ${preview.count} 条 · ${escapeHtml(preview.domains.join("、"))}
          <div>最早已知到期 ${preview.nextExpiry ? new Date(preview.nextExpiry * 1000).toLocaleString() : "未提供"}</div>
          <div>同域、名称及路径的 Cookie 将被替换。</div>` : ""}</div>
        <div id="account-cookie-message" role="status">${escapeHtml(message)}</div>
        <div class="modal-footer account-cookie-actions">
          <button id="modal-cancel-btn" class="btn" type="button" ${busy ? "disabled" : ""}>关闭</button>
          <button id="account-cookie-export" class="btn" type="button" ${busy ? "disabled" : ""}>导出到剪贴板</button>
          <button id="account-cookie-preview-btn" class="btn" type="button" ${busy ? "disabled" : ""}>预览</button>
          <button id="account-cookie-import" class="btn primary" type="button" ${preview && !busy ? "" : "disabled"}>导入</button>
          <button id="account-cookie-open" class="btn" type="button" ${busy ? "disabled" : ""}>${imported ? "打开并验证" : "打开账号"}</button>
        </div>
      </div>
    </div>`;
}

async function accountCookieAction(action: "preview" | "import" | "export"): Promise<void> {
  if (modal.kind !== "cookies" || modal.busy) return;
  const view = modal;
  const { uuid, providerId } = view.account;
  const raw = document.querySelector<HTMLTextAreaElement>("#account-cookie-input")?.value ?? "";
  if (action === "import" && !view.preview) return;
  if (action === "preview" && !raw.trim()) {
    view.message = "请先把要检查的 Cookie 粘贴到输入框。";
    render();
    return;
  }
  if (action === "export" && !await askConfirm({
    title: "导出登录 Cookie",
    desc: "剪贴板内容可用于登录。导出只保留在本机剪贴板，60 秒后到期。",
    confirmLabel: "复制",
  })) {
    if ((modal as ModalMode).kind === "none") await setModal(view);
    return;
  }
  if (action === "export") await setModal(view);
  view.busy = true;
  view.message = "";
  if (action !== "import") view.preview = null;
  render();
  try {
    if (action === "preview") {
      view.preview = await invoke<CookiePreview>("preview_account_cookies", { providerId, raw });
    } else {
      const { count, skipped } = await invoke<{ count: number; skipped: number }>("transfer_account_cookies", {
        uuid, providerId, raw: action === "import" ? raw : null,
      });
      view.preview = null;
      view.imported = action === "import" || view.imported;
      view.message = action === "import"
        ? `已写入 ${count} 条 Cookie，请打开账号验证登录。`
        : `已复制 ${count} 条 Cookie，60 秒后到期。${skipped ? `其中有 ${skipped} 条本机记录未导出，可能需要重新登录。` : ""}`;
    }
  } catch (error) {
    view.preview = null;
    view.message = String(error);
  } finally {
    view.busy = false;
    if (modal === view) {
      render();
      if (action === "preview" && view.preview) {
        const input = document.querySelector<HTMLTextAreaElement>("#account-cookie-input");
        if (input) input.value = raw;
      }
      if (action === "import" && view.imported && view.message.startsWith("已写入")) {
        const input = document.querySelector<HTMLTextAreaElement>("#account-cookie-input");
        if (input) input.value = "";
      }
    }
  }
}

async function submitApproval(
  view: Extract<ModalMode, { kind: "approvals" }>, item: SpikeApprovalItem,
  choice: SpikeApprovalItem["choices"][number]
): Promise<void> {
  const permanent = choice.decision === "allow_always";
  const label = permanent ? "始终允许" : choice.decision === "deny" ? "拒绝" : "允许一次";
  const name = item.displayName || item.kind || "此审批";
  const accepted = await askConfirm({
    title: label,
    desc: `在「${view.account.alias}」对「${name}」执行「${label}」。`,
    confirmLabel: label,
    danger: permanent,
  });
  const confirmed = accepted && (!permanent || await askConfirm({
    title: "确认持久授权",
    desc: `后续同一${choice.scope === "destination_domain" ? "目标域名" : "对象"}的相关请求可能继续被允许。`,
    confirmLabel: "确认持久授权", danger: true,
  }));
  await setModal(view);
  if (!confirmed) return;
  view.loading = true;
  view.decisionMessage = "";
  render();
  try {
    await invoke("decide_muse_approval", {
      uuid: view.account.uuid,
      request: {
        approvalId: item.id, sessionScopeId: item.sessionScopeId, expiresAtMs: item.expiresAtMs,
        decision: choice.decision, scope: choice.scope, permanentConfirmed: permanent,
      },
    });
    view.decisionMessage = "审批结果已确认。";
  } catch (error) {
    view.decisionMessage = String(error);
  } finally {
    view.loading = false;
    await loadAccountApprovals(view);
  }
}

function renderModal(provider: ProviderInfo): string {
  if (modal.kind === "none") return "";
  if (modal.kind === "approvals") return renderApprovalsModal(modal);
  if (modal.kind === "cookies") return renderCookieModal(modal);
  if (modal.kind === "sessions") return renderSessionsModal(modal);
  if (modal.kind === "proxyPool") return renderProxyPool(modal);
  if (modal.kind === "backup") return renderAccountBackup(modal);
  if (modal.kind === "tasks") return renderTasks(modal);
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
    const timezone = modal.timezoneDraft;
    const customTimezone = !accountTimezoneChoices.some(([value]) => value === timezone);
    return `
      <div class="inline-modal" role="dialog" aria-modal="true">
        <div class="modal-card">
          <div class="modal-title">编辑账号</div>
          <label for="modal-alias-input">备注</label>
          <input id="modal-alias-input" class="modal-input" maxlength="60" placeholder="输入备注名称" value="${escapeHtml(modal.draft)}" />
          <label for="modal-group-input">分组</label>
          <input id="modal-group-input" class="modal-input" maxlength="32" placeholder="留空为未分组" value="${escapeHtml(modal.groupDraft)}" />
          ${snapshot?.platform === "ios" ? `<label for="modal-timezone-choice">账号时区</label>
            <select id="modal-timezone-choice" class="modal-input">
              ${accountTimezoneChoices.map(([value, label]) => `<option value="${value}" ${value === timezone ? "selected" : ""}>${label}</option>`).join("")}
              <option value="custom" ${customTimezone ? "selected" : ""}>自定义</option>
            </select>
            <div id="modal-timezone-custom" ${customTimezone ? "" : "hidden"}>
              <label for="modal-timezone-input">IANA 时区</label>
              <input id="modal-timezone-input" class="modal-input" maxlength="96" placeholder="例如 Europe/Paris" value="${customTimezone ? escapeHtml(timezone) : ""}" autocomplete="off" spellcheck="false" />
            </div>` : ""}
          <div class="modal-footer">
            ${snapshot?.platform === "ios" && modal.account.authState === "authenticated" ? '<button id="account-task-btn" class="btn" type="button">任务</button>' : ""}
            ${snapshot?.platform === "ios" ? '<button id="account-proxy-pool" class="btn" type="button">代理池</button>' : ""}
            <button id="modal-cancel-btn" class="btn" type="button">取消</button>
            <button id="modal-confirm-btn" class="btn primary" type="button">保存</button>
          </div>
        </div>
      </div>
    `;
  }
  if (modal.kind === "confirm") {
    const desc = escapeHtml(modal.desc).replace(/\n/g, "<br>");
    return `
      <div class="inline-modal" role="dialog" aria-modal="true">
        <div class="modal-card">
          <div class="modal-title">${escapeHtml(modal.title)}</div>
          <div class="modal-desc">${desc}</div>
          <div class="modal-footer">
            <button id="modal-cancel-btn" class="btn" type="button">取消</button>
            <button id="modal-confirm-btn" class="btn ${modal.danger ? "danger" : "primary"}" type="button">${escapeHtml(modal.confirmLabel)}</button>
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

function applyAccountFilter(state: ManagerSnapshot, provider: ProviderInfo): void {
  const { query, group } = accountFilters[provider.id];
  const needle = query.trim().toLocaleLowerCase();
  const accounts = new Map(state.accounts.filter((acc) => acc.providerId === provider.id).map((acc) => [acc.uuid, acc]));
  const rows = document.querySelectorAll<HTMLElement>(".account-list [data-switch-uuid]");
  let visible = 0;
  for (const row of rows) {
    const acc = accounts.get(row.dataset.switchUuid ?? "");
    if (!acc) continue;
    const label = acc.group ?? "";
    row.hidden = !reorderMode && (
      (group !== null && group !== label) ||
      (needle.length > 0 && !acc.alias.toLocaleLowerCase().includes(needle) && !label.toLocaleLowerCase().includes(needle))
    );
    if (!row.hidden) visible++;
  }
  const empty = document.getElementById("account-filter-empty");
  if (empty) empty.hidden = rows.length === 0 || visible > 0;
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
  const filter = accountFilters[provider.id];
  const groups = [...new Set(providerAccounts.map((account) => account.group).filter((group): group is string => Boolean(group)))].sort((a, b) => a.localeCompare(b));
  if (filter.group && !groups.includes(filter.group)) filter.group = null;
  const groupOptions = ["", ...groups].map((group) =>
    `<option value="group:${escapeHtml(group)}" ${filter.group === group ? "selected" : ""}>${escapeHtml(group || "未分组")}</option>`
  ).join("");
  const filtersHtml = provider.enabled && providerAccounts.length && !inReorder ? `
    <div class="account-filters">
      <input id="account-search" class="modal-input" type="search" aria-label="搜索账号" placeholder="搜索备注或分组" value="${escapeHtml(filter.query)}" />
      <select id="account-group-select" class="modal-input" aria-label="按分组筛选">
        <option value="all" ${filter.group === null ? "selected" : ""}>全部分组</option>
        ${groupOptions}
      </select>
    </div>` : "";
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
                  ${acc.group ? `<span class="account-group-label" title="${escapeHtml(acc.group)}">${escapeHtml(acc.group)}</span>` : ""}
                  ${shortcutHint}
                  ${alertBadgeHtml(d)}
                </div>
                ${subHtmlFor(acc, d, isMobile)}
              </div>
              <div class="row-ops">
                ${state.platform === "ios" && provider.id === "muse" && acc.authState === "authenticated" ? `<button class="action-icon-btn account-approval-btn" data-sessions-uuid="${escapeHtml(acc.uuid)}" type="button">会话</button>` : ""}
                ${state.platform === "ios" ? `<button class="action-icon-btn account-approval-btn" data-cookies-uuid="${escapeHtml(acc.uuid)}" type="button">登录恢复</button>` : ""}
                ${state.platform === "ios" && provider.id === "muse" && acc.authState === "authenticated" ? `<button class="action-icon-btn account-approval-btn" data-approvals-uuid="${escapeHtml(acc.uuid)}" type="button">审批</button>` : ""}
                <button class="action-icon-btn" data-rename-uuid="${escapeHtml(acc.uuid)}" title="编辑账号备注与分组" aria-label="编辑账号备注与分组" type="button">${ICON_EDIT}</button>
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
  const diagAccount = pickWatchAccount();
  const museDiagnostics = provider.id === "muse";
  const spikeDiagHtml = museDiagnostics ? `
    <section id="spike-workspace" class="spike-workspace" aria-labelledby="spike-workspace-title" ${spikeWorkspaceOpen ? "" : "hidden"}>
      <header class="spike-toolbar">
        <div class="spike-toolbar-title">
          <button id="spike-back-btn" class="mini-btn" type="button">返回</button>
          <div>
            <h2 id="spike-workspace-title" tabindex="-1">原生诊断（Spike）</h2>
            <div id="spike-workspace-account">${escapeHtml(diagAccount?.alias ?? "未登录 Muse 账号")}</div>
          </div>
        </div>
        <div class="spike-actions">
          <button id="spike-copy-btn" class="mini-btn" type="button">复制结果摘要</button>
          <button id="spike-wire-copy-btn" class="mini-btn" type="button" ${spikeFingerprint?.wireTrace?.length ? "" : "disabled"}>复制脱敏流诊断</button>
        </div>
      </header>
      <div class="spike-scroll" tabindex="0" role="region" aria-label="诊断内容">
        <div class="spike-diag" data-account-uuid="${escapeHtml(diagAccount?.uuid ?? "")}">${spikeDiagInnerHtml(diagAccount?.uuid)}</div>
      </div>
    </section>` : provider.id === "hark" && isSpikeBuild ? harkProbeHtml() : "";

  // Spike 顶栏标识（0.1.22）：只在 Spike 版显示（bundle id 门控）。
  const spikeBadgeHtml =
    isSpikeBuild && provider.id === "muse"
      ? `<span class="spike-badge" title="Spike 版">Spike</span>`
      : "";

  return `
    ${museDiagnostics ? `<div id="spike-account-view" class="spike-account-view" ${spikeWorkspaceOpen ? "hidden" : ""}>` : ""}
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
      <div class="header-actions">${museDiagnostics ? `<button id="spike-open-btn" class="mini-btn" type="button">诊断</button>` : ""}${state.platform === "ios" && !inReorder ? '<button id="account-backup-btn" class="mini-btn" type="button">备份</button>' : ""}${headerBtnHtml}</div>
    </div>
    ${noticeHtml}
    ${conflictsHtml}
    ${filtersHtml}
    <div class="account-list${inReorder ? " reorder-mode" : ""}">${listBody}<div id="account-filter-empty" class="empty-list" hidden>没有匹配的账号</div></div>
    ${museDiagnostics ? `</div>` : ""}
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
    JSON.stringify(providerAccounts.map((a) => [a.uuid, a.group, a.authState === "authenticated"])),
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
    case "confirm":
      // 内容在打开时确定，不会在显示期间变化；标题 + 长度足以区分。
      return `confirm:${modal.title}:${modal.desc.length}:${modal.confirmLabel}`;
    case "approvals":
    case "cookies":
    case "sessions":
    case "proxyPool":
    case "backup":
    case "tasks":
      return null;
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
  if (modal.kind === "cookies" && mount === lastModalMount && mount.querySelector("#cookie-title")) {
    const template = document.createElement("template");
    template.innerHTML = html;
    for (const id of ["account-cookie-stored", "account-cookie-preview", "account-cookie-message"]) {
      const current = mount.querySelector<HTMLElement>(`#${id}`);
      const next = template.content.querySelector<HTMLElement>(`#${id}`);
      if (current && next) current.innerHTML = next.innerHTML;
    }
    for (const button of mount.querySelectorAll<HTMLButtonElement>(".account-cookie-actions button")) {
      const next = template.content.querySelector<HTMLButtonElement>(`#${button.id}`);
      if (next) {
        button.disabled = next.disabled;
        button.textContent = next.textContent;
      }
    }
    const input = mount.querySelector<HTMLTextAreaElement>("#account-cookie-input");
    if (input) input.readOnly = modal.busy;
    lastModalKey = key;
    return;
  }
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
    applyAccountFilter(snapshot, provider);
    const diag = document.querySelector<HTMLElement>("#spike-workspace .spike-diag");
    if (diag) {
      const account = pickWatchAccount();
      if (diag.dataset.accountUuid !== (account?.uuid ?? "")) syncSpikeDiag();
      const label = document.getElementById("spike-workspace-account");
      if (label) label.textContent = account?.alias ?? "未登录 Muse 账号";
    }
    renderModalRoot(provider);
    syncUpdateButton();
    return;
  }
  // A structural change arriving mid-drag must not rebuild the list
  // under the finger; the drag-end handler re-renders with the latest
  // snapshot.
  if (dragActive) return;
  const spikeScrollTop = root.querySelector<HTMLElement>(".spike-scroll")?.scrollTop ?? 0;
  const spikeSections = Array.from(root.querySelectorAll<HTMLDetailsElement>("#spike-workspace details[open]"), (section) => section.dataset.spikeSection);
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
  applyAccountFilter(snapshot, provider);
  root.querySelectorAll<HTMLDetailsElement>("#spike-workspace details[data-spike-section]").forEach((section) => {
    section.open = spikeSections.includes(section.dataset.spikeSection);
  });
  const spikeScroller = root.querySelector<HTMLElement>(".spike-scroll");
  if (spikeScroller) spikeScroller.scrollTop = spikeScrollTop;
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
    const approvalChoice = target.closest<HTMLElement>("[data-approval-choice]");
    if (approvalChoice && modal.kind === "approvals" && !modal.loading) {
      const item = modal.result?.items[Number(approvalChoice.dataset.approvalItem)];
      const choice = item?.choices[Number(approvalChoice.dataset.approvalChoice)];
      if (item && choice) await submitApproval(modal, item, choice);
      return;
    }
    const poolAction = target.closest<HTMLElement>("[data-proxy-pool-action]");
    if (poolAction && modal.kind === "proxyPool") {
      await proxyPoolAction(modal, poolAction.dataset.proxyPoolAction ?? "", poolAction.dataset.poolId || null);
      return;
    }
    const sessionButton = target.closest<HTMLElement>("[data-sessions-uuid]");
    if (sessionButton) {
      if (modal.kind !== "none" || reorderMode) return;
      const account = snapshot.accounts.find((entry) => entry.providerId === "muse" && entry.uuid === sessionButton.dataset.sessionsUuid);
      if (account?.authState === "authenticated") {
        const view: Extract<ModalMode, { kind: "sessions" }> = {
          kind: "sessions", account, loading: true, items: null, error: "",
        };
        await setModal(view);
        await loadMuseSessions(view);
      }
      return;
    }
    const thread = target.closest<HTMLElement>("[data-muse-thread]")?.dataset.museThread;
    if (thread && modal.kind === "sessions" && (modal.items?.some((item) => item.id === thread) || modal.account.lastMuseThread === thread)) {
      const { providerId, uuid } = modal.account;
      await setModal({ kind: "none" });
      await callCommand("open_muse_thread", { uuid, threadId: thread }, { providerId, uuid });
      return;
    }
    const cookieButton = target.closest<HTMLElement>("[data-cookies-uuid]");
    if (cookieButton) {
      if (modal.kind !== "none" || reorderMode) return;
      const account = snapshot.accounts.find((entry) => entry.providerId === provider.id && entry.uuid === cookieButton.dataset.cookiesUuid);
      if (account) {
        const view: Extract<ModalMode, { kind: "cookies" }> = {
          kind: "cookies", account, busy: true, preview: null, message: "", imported: false, stored: null,
        };
        await setModal(view);
        try {
          view.stored = await invoke("inspect_account_cookies", { uuid: account.uuid, providerId: account.providerId });
        } catch {
          // Import remains available when the existing store cannot be read.
        } finally {
          view.busy = false;
          if ((modal as ModalMode) === view) render();
        }
      }
      return;
    }
    const approvalButton = target.closest<HTMLElement>("[data-approvals-uuid]");
    if (approvalButton) {
      if (modal.kind !== "none" || reorderMode) return;
      const account = snapshot.accounts.find((entry) => entry.providerId === "muse" && entry.uuid === approvalButton.dataset.approvalsUuid);
      if (account?.authState === "authenticated") {
        const view: Extract<ModalMode, { kind: "approvals" }> = {
          kind: "approvals", account, loading: true, result: null, fetchedAt: null, decisionMessage: "",
        };
        await setModal(view);
        await loadAccountApprovals(view);
      }
      return;
    }
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
          draft: acc.alias,
          groupDraft: acc.group ?? "",
          timezoneDraft: acc.timezone ?? "",
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
    if (target.closest("#spike-open-btn, #spike-back-btn")) {
      setSpikeWorkspaceOpen(Boolean(target.closest("#spike-open-btn")));
      return;
    }
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
    const leaseBtn = target.closest<HTMLElement>("#spike-lease-btn");
    if (leaseBtn) {
      await runSpikeLease();
      return;
    }
    const liveBtn = target.closest<HTMLElement>("#spike-live-btn");
    if (liveBtn) {
      if (spikeLive?.running) {
        await refreshSpikeLive();
      } else {
        await startSpikeLive();
      }
      return;
    }
    const sendBtn = target.closest<HTMLElement>("#spike-send-btn");
    if (sendBtn) {
      await runSpikeSend();
      return;
    }
    const fingerprintBtn = target.closest<HTMLElement>("#spike-fingerprint-btn");
    if (fingerprintBtn) {
      await runSpikeFingerprint();
      return;
    }
    if (target.closest("#spike-hark-watch-btn")) {
      await runHarkWatch();
      return;
    }
    const proxyBtn = target.closest<HTMLElement>("#spike-proxy-btn, #spike-proxy-connectivity-btn");
    if (proxyBtn) {
      await runSpikeProxy(proxyBtn.id === "spike-proxy-connectivity-btn");
      return;
    }
    const proxySaveBtn = target.closest<HTMLElement>("#spike-proxy-save-btn");
    if (proxySaveBtn) {
      await saveSpikeProxy();
      return;
    }
    const proxyClearBtn = target.closest<HTMLElement>("#spike-proxy-clear-btn");
    if (proxyClearBtn) {
      await clearSpikeProxy();
      return;
    }
    const stopBtn = target.closest<HTMLElement>("#spike-stop-btn");
    if (stopBtn) {
      await cancelSpikeProbe();
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
    if (target.closest("#spike-quota-sync-btn")) {
      await runBackgroundQuotaDiagnostic();
      return;
    }
    const copyBtn = target.closest<HTMLElement>("#spike-copy-btn, #spike-wire-copy-btn, #spike-quota-sync-copy-btn");
    if (copyBtn) {
      const el = document.querySelector<HTMLElement>("#spike-workspace .spike-diag");
      const wireOnly = copyBtn.id === "spike-wire-copy-btn";
      const quotaOnly = copyBtn.id === "spike-quota-sync-copy-btn";
      let text = quotaOnly ? backgroundQuotaLines().join("\n") : el?.querySelector("#spike-wire-trace")?.textContent ?? "";
      if (el && !wireOnly && !quotaOnly) {
        const sections = el.querySelectorAll(".spike-diag-result, .spike-notice, #spike-front-trace:not([hidden])");
        text = ["原生诊断（Spike）", ...Array.from(sections, (section) => {
          const clone = section.cloneNode(true) as HTMLElement;
          clone.querySelectorAll("[data-spike-section=wire]").forEach((details) => details.remove());
          clone.querySelectorAll("dt").forEach((term) => term.append(" "));
          clone.querySelectorAll("div, summary").forEach((line) => line.append("\n"));
          return clone.textContent ?? "";
        })].join("\n")
          .split("\n")
          .map((l) => l.trim())
          .filter((l) => l.length > 0)
          .join("\n");
      }
      let copied = true;
      try {
        await navigator.clipboard.writeText(text);
      } catch {
        const ta = document.createElement("textarea");
        ta.value = text;
        ta.readOnly = true;
        ta.style.position = "fixed";
        ta.style.opacity = "0";
        document.body.appendChild(ta);
        ta.select();
        try {
          copied = document.execCommand("copy");
        } catch {
          copied = false;
        } finally {
          ta.remove();
          copyBtn.focus({ preventScroll: true });
        }
      }
      copyBtn.textContent = copied ? "已复制" : "复制失败";
      setTimeout(() => {
        copyBtn.textContent = quotaOnly ? "复制脱敏同步结果" : wireOnly ? "复制脱敏流诊断" : "复制结果摘要";
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
      case "account-task-btn":
        if (modal.kind === "rename") {
          const view: Extract<ModalMode, { kind: "tasks" }> = {
            kind: "tasks", account: modal.account, checking: false, note: "", quotaChecking: false,
          };
          await setModal(view);
          await checkAccountTasks(view);
        }
        break;
      case "account-task-refresh":
        if (modal.kind === "tasks") await checkAccountTasks(modal);
        break;
      case "account-task-open":
        if (modal.kind === "tasks") {
          const { providerId, uuid } = modal.account;
          await setModal({ kind: "none" });
          await callCommand("switch_account", { providerId, uuid }, { providerId, uuid });
        }
        break;
      case "account-quota-refresh":
        if (modal.kind === "tasks" && !modal.quotaChecking) {
          const view = modal;
          view.quotaChecking = true;
          view.note = "正在查询额度…";
          render();
          try {
            const result = await invoke<SpikeQuotaProbe>("muse_native_quota_probe", {
              providerId: view.account.providerId, uuid: view.account.uuid,
            });
            view.note = result.ok ? "额度已更新。" : "额度未更新，请保留上次记录。";
          } catch {
            view.note = "额度未更新，请稍后重试。";
          } finally {
            view.quotaChecking = false;
            if (modal === view) render();
          }
        }
        break;
      case "account-backup-btn":
        if (modal.kind === "none") await setModal({
          kind: "backup", busy: false, encrypted: "", fileName: "", preview: null, message: "",
          mode: "light", includeGroups: true, includeProxy: false, expiryDays: 0,
        });
        break;
      case "backup-export":
        if (modal.kind === "backup") await accountBackupAction(modal, "export");
        break;
      case "backup-preview":
        if (modal.kind === "backup") await accountBackupAction(modal, "preview");
        break;
      case "backup-merge":
        if (modal.kind === "backup") await accountBackupAction(modal, "merge");
        break;
      case "backup-overwrite":
        if (modal.kind === "backup") await accountBackupAction(modal, "overwrite");
        break;
      case "account-proxy-pool":
        if (modal.kind === "rename") {
          const view: Extract<ModalMode, { kind: "proxyPool" }> = {
            kind: "proxyPool", account: modal.account, data: null, setting: null, busy: false, message: "",
          };
          await setModal(view);
          await proxyPoolAction(view, "list", null);
        }
        break;
      case "muse-sessions-refresh":
        if (modal.kind === "sessions" && !modal.loading) await loadMuseSessions(modal);
        break;
      case "account-cookie-preview-btn":
        await accountCookieAction("preview");
        break;
      case "account-cookie-import":
        await accountCookieAction("import");
        break;
      case "account-cookie-export":
        await accountCookieAction("export");
        break;
      case "account-cookie-open":
        if (modal.kind === "cookies" && !modal.busy) {
          const { providerId, uuid } = modal.account;
          await setModal({ kind: "none" });
          await callCommand("switch_account", { providerId, uuid }, { providerId, uuid });
        }
        break;
      case "approvals-refresh-btn":
        if (modal.kind === "approvals" && !modal.loading) await loadAccountApprovals(modal);
        break;
      case "approvals-open-account-btn":
        if (modal.kind === "approvals") {
          const { providerId, uuid } = modal.account;
          await setModal({ kind: "none" });
          await callCommand("switch_account", { providerId, uuid }, { providerId, uuid });
        }
        break;
      case "modal-cancel-btn":
        if (modal.kind === "update" && updateState.installing) break;
        await setModal({ kind: "none" });
        break;
      case "modal-confirm-btn": {
        const currentModal = modal;
        if (currentModal.kind === "confirm") {
          // 先摘掉 resolver，再关弹窗：setModal 的自动结算会看到 null，
          // 不会把这次确认覆盖成「取消」。
          const resolve = confirmResolver;
          confirmResolver = null;
          await setModal({ kind: "none" });
          resolve?.(true);
        } else if (currentModal.kind === "add") {
          const alias = currentModal.draft.trim();
          await setModal({ kind: "none" });
          await callCommand("add_account", {
            providerId: currentModal.providerId,
            alias,
          });
        } else if (currentModal.kind === "rename") {
          const alias = currentModal.draft.trim();
          if (!alias) break;
          const timezone = currentModal.timezoneDraft.trim();
          if (timezone) {
            try {
              new Intl.DateTimeFormat(undefined, { timeZone: timezone });
            } catch {
              const input = document.querySelector<HTMLInputElement>("#modal-timezone-input");
              input?.setCustomValidity("请输入有效的 IANA 时区，例如 Asia/Shanghai");
              input?.reportValidity();
              break;
            }
          }
          await setModal({ kind: "none" });
          await callCommand("rename_account", {
            providerId: currentModal.account.providerId,
            uuid: currentModal.account.uuid,
            alias,
            group: currentModal.groupDraft.trim(),
            timezone: snapshot?.platform === "ios" ? timezone : null,
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

  root.addEventListener("change", async (ev) => {
    const timezoneChoice = ev.target as HTMLSelectElement;
    if (timezoneChoice.id === "modal-timezone-choice" && modal.kind === "rename") {
      const input = document.querySelector<HTMLInputElement>("#modal-timezone-input");
      const custom = document.querySelector<HTMLElement>("#modal-timezone-custom");
      const useCustom = timezoneChoice.value === "custom";
      if (custom) custom.hidden = !useCustom;
      modal.timezoneDraft = useCustom ? input?.value ?? "" : timezoneChoice.value;
      if (useCustom) input?.focus();
      return;
    }
    const input = ev.target as HTMLInputElement;
    if (input.id === "account-task-notifications" && modal.kind === "tasks") {
      const view = modal;
      input.disabled = true;
      try {
        snapshot = await invoke<ManagerSnapshot>("set_account_task_notifications", {
          uuid: view.account.uuid, providerId: view.account.providerId, enabled: input.checked,
        });
        view.note = "";
        if (input.checked) void checkAccountTasks(view);
      } catch (error) {
        view.note = String(error);
        input.checked = !input.checked;
      } finally {
        input.disabled = false;
        if (modal === view) render();
      }
      return;
    }
    if (modal.kind === "backup") {
      const view = modal;
      if (input.id === "backup-mode") {
        view.mode = input.value as BackupMode;
        render();
      } else if (input.id === "backup-expiry") {
        view.expiryDays = Number(input.value);
      } else if (input.id === "backup-include-groups") {
        view.includeGroups = input.checked;
      } else if (input.id === "backup-include-proxy") {
        view.includeProxy = input.checked;
      } else if (input.id === "backup-file") {
        const file = input.files?.[0];
        view.preview = null;
        view.encrypted = "";
        view.fileName = "";
        if (file && file.size <= 2_800_000) {
          view.busy = true;
          try {
            view.encrypted = await file.text();
            view.fileName = file.name;
            view.message = "";
          } catch {
            view.message = "无法读取备份文件。";
          } finally {
            view.busy = false;
          }
        } else {
          view.message = file ? "备份文件过大。" : "";
        }
        if (modal === view) render();
      }
      return;
    }
    const select = ev.target as HTMLSelectElement;
    if (select.id !== "account-group-select" || !snapshot) return;
    const provider = currentProvider(snapshot);
    accountFilters[provider.id].group = select.value === "all" ? null : select.value.slice(6);
    applyAccountFilter(snapshot, provider);
  });

  root.addEventListener("input", (ev) => {
    const input = ev.target as HTMLInputElement;
    if (input.id === "account-cookie-input" && modal.kind === "cookies") {
      modal.preview = null;
      document.querySelector<HTMLButtonElement>("#account-cookie-import")?.setAttribute("disabled", "");
      const preview = document.getElementById("account-cookie-preview");
      if (preview) preview.textContent = "";
      return;
    }
    if (input.id === "account-search" && snapshot) {
      const provider = currentProvider(snapshot);
      accountFilters[provider.id].query = input.value;
      applyAccountFilter(snapshot, provider);
      return;
    }
    if (input.id === "modal-timezone-input" && modal.kind === "rename") {
      input.setCustomValidity("");
      modal.timezoneDraft = input.value;
      return;
    }
    if (input.id === "modal-group-input" && modal.kind === "rename") {
      modal.groupDraft = input.value;
      return;
    }
    if (input.id === "spike-proxy-uri" || input.id === "spike-proxy-user") {
      const accountUuid = input.closest<HTMLElement>(".spike-diag")?.dataset.accountUuid;
      if (accountUuid) {
        const account = spikeProxyAccount(accountUuid);
        if (input.id === "spike-proxy-uri") account.uri = input.value;
        else account.username = input.value;
      }
      return;
    }
    if (input.id !== "modal-alias-input") return;
    if (modal.kind === "add" || modal.kind === "rename") {
      modal.draft = input.value;
    }
  });
  root.addEventListener("keydown", async (ev) => {
    const input = ev.target as HTMLElement;
    if (!["modal-alias-input", "modal-group-input", "modal-timezone-input"].includes(input.id)) return;
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

  await listen<SpikeFingerprintProgress>("muse-fingerprint-progress", ({ payload }) => {
    if (payload.taskId !== spikeFingerprintTask?.taskId || payload.accountUuid !== spikeFingerprintTask.accountUuid) return;
    spikeFingerprintProgress = payload;
    syncSpikeDiag();
  });

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
      if (modal.kind === "confirm" && ev.key === "Enter") {
        ev.preventDefault();
        document.querySelector<HTMLButtonElement>("#modal-confirm-btn")?.click();
        return;
      }
      if (
        ev.key === "Escape" &&
        modal.kind !== "add" &&
        modal.kind !== "rename" &&
        !((modal.kind === "cookies" || modal.kind === "proxyPool" || modal.kind === "backup") && modal.busy) &&
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
  if (snapshot.platform === "ios") {
    foregroundTaskTimer = window.setInterval(() => void refreshForegroundTask(), 15_000);
    window.addEventListener("pagehide", () => {
      if (foregroundTaskTimer !== null) window.clearInterval(foregroundTaskTimer);
    }, { once: true });
    void refreshForegroundTask();
  }
  document.addEventListener("visibilitychange", () => {
    if (!document.hidden) {
      triggerAutoSync();
      // 回前台立刻取一次存活探针状态：后台期间定时器不会运行，
      // 界面上的秒数是旧的，必须以真实快照为准。
      if (spikeLive?.running) void refreshSpikeLive();
      if (modal.kind === "tasks") void checkAccountTasks(modal);
      else void refreshForegroundTask();
    }
  });
  window.addEventListener("focus", () => triggerAutoSync());
  setTimeout(() => {
    void runCheckForUpdates(true);
  }, 3500);
}

void bootstrap();
