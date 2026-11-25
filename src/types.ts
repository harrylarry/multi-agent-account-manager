export type ProviderId = "muse" | "hark";
export type AuthState = "unknown" | "authenticated" | "logged_out";

export interface AccountKey {
  providerId: ProviderId;
  uuid: string;
}

export interface ProviderInfo {
  id: ProviderId;
  name: string;
  shortName: string;
  enabled: boolean;
  statusText: string;
  entryUrl: string;
  homepageUrl: string;
  callbackOrigins: string[];
  verifiedMethods: string[];
}

export interface QuotaDetail {
  planLabel: string;
  planValue: string;
  planSubtext: string | null;
  planUsedPercent: number | null;
  extraLabel: string | null;
  extraValue: string | null;
  extraSubtext: string | null;
  extraUsedPercent: number | null;
}

export interface AccountRecord {
  providerId: ProviderId;
  uuid: string;
  alias: string;
  group?: string;
  timezone?: string;
  lastMuseThread?: string;
  createdAtMs: number;
  autoAlias: boolean;
  avatarUrl: string | null;
  quotaText: string | null;
  quotaPercent: number | null;
  quotaDetail: QuotaDetail | null;
  quotaObservation?: { source: string; window: string | null; percentUsed?: number | null; resetsAtMs: number | null; checkedAtMs: number; stale: boolean };
  authState: AuthState;
  // 0.1.15: the iOS task watcher saw a new reply for this account
  // while it was not active. Optional: the visual harness stubs and
  // pre-0.1.15 snapshots don't carry it.
  hasUnreadReply?: boolean;
  lastTaskReplyAtMs?: number | null;
  taskNotifications?: boolean;
  pendingApprovalCount?: number | null;
  museUnreadStatus?: string | null;
}

export interface ConflictItem {
  uuid: string;
  providers: ProviderId[];
}

export interface ManagerSnapshot {
  platform: string;
  providers: ProviderInfo[];
  accounts: AccountRecord[];
  selectedProviderId: ProviderId;
  activeAccount: AccountKey | null;
  switchingAccount: AccountKey | null;
  warmAccounts: AccountKey[];
  lastCommitted: AccountKey | null;
  pendingCleanup: AccountKey[];
  conflicts: ConflictItem[];
  notice: string | null;
  modalActive: boolean;
}

export interface UpdateCheckResult {
  currentVersion: string;
  latestVersion: string;
  hasUpdate: boolean;
  releaseNotes: string;
  pubDate: string | null;
  downloadUrl: string | null;
  sha256: string | null;
  filename: string | null;
  size: number | null;
  releasePageUrl: string;
}

export interface UpdateProgressEvent {
  stage: string;
  downloaded: number;
  total: number | null;
  percent: number | null;
  message: string;
}

export interface InstallUpdateOutcome {
  status: string;
  message: string;
}
