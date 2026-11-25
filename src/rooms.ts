import type { AccountRecord } from "./types";

const roles = [
  ["commander", "指挥"], ["executor", "执行"], ["reviewer", "审核"], ["publisher", "上传"], ["discussant", "讨论"],
];

const phaseNames: Record<string, string> = {
  created: "草稿", validating: "校验成员", planning: "待规划", dispatching: "等待发送核对",
  waiting_reply: "等待回复", reviewing: "审核", assembling: "汇总",
  pending_confirmation: "待确认", finalized: "已定稿", paused: "已暂停", stopped: "已停止",
  needs_user: "等待裁决", needs_reconciliation: "等待核对远端状态", failed: "失败",
};

const stageNames: Record<string, string> = {
  plan: "指挥拆解", work: "执行", review: "独立审核", discuss: "讨论", summarize: "指挥汇总", publish: "整理交付",
};
const holdReasons: Record<string, string> = {
  round_limit: "已到轮次上限。接力已停止，请裁决。",
  review_limit: "审核已打回 3 次。接力已停止，请裁决。",
  invalid_review: "没有收到可核验的审核决定。已停止接力，审核者的原回复保存在记录中。",
  account_unavailable: "本轮没有发出。请检查该成员的登录、代理或发送占用，也可提议换人。",
  context_too_large: "必须交接的材料超过范围，已停止。本轮没有发送，前序材料仍保留。",
};

interface Member { uuid: string; roles: string[] }
interface Template { id: string; title: string; version: number; minParticipants: number; flow: string }
interface Attempt { accountUuid: string; status: string; sessionId: string | null; userMessageId: string | null;
  remoteCancelAcknowledged?: boolean;
  stage?: string; review?: { decision: "approve" | "return"; reason: string };
  delivery: { kind: string; text: string; capturedAtMs: number; source: string; sha256: string } | null }
interface Room {
  id: string; revision: number; phase: string; rounds: number; returned: number;
  draft: { name: string; goal: string; roundLimit: number; members: Member[] };
  template: Template;
  steps: { phase: string; atMs: number }[];
  attempts: Attempt[];
  approvedAtMs: number | null;
  relay?: {
    hold?: string; pendingNote?: string; notes?: string[]; finalAttempt?: string;
    proposal?: { id: string; members: Member[] };
  };
}
export interface RoomBook { templates: Template[]; rooms: Room[] }
export interface RoomsView { kind: "rooms"; book: RoomBook | null; busy: boolean; error: string; runningRoomId?: string }

export function renderRooms(view: RoomsView, accounts: AccountRecord[], esc: (text: string) => string): string {
  const muse = accounts.filter((account) => account.providerId === "muse");
  const options = view.book?.templates.map((item) => `<option value="${esc(item.id)}">${esc(item.title)} · 至少 ${item.minParticipants} 人</option>`).join("") ?? "";
  const name = (uuid: string) => esc(muse.find((account) => account.uuid === uuid)?.alias ?? "原成员");
  const memberLine = (member: Member) => `${name(member.uuid)} · ${member.roles.map((role) => roles.find(([id]) => id === role)?.[1] ?? "").join("、")}`;
  const roster = (selected: Member[]) => muse.map((account) => `<fieldset class="hark-reply" data-room-member="${esc(account.uuid)}"><legend>${esc(account.alias)}</legend>
    ${roles.map(([id, label]) => `<label><input type="checkbox" value="${id}" ${selected.some((member) => member.uuid === account.uuid && member.roles.includes(id)) ? "checked" : ""}> ${label}</label> `).join("")}
    </fieldset>`).join("") || '<div class="modal-desc">先添加 Muse 账号，再选择房间成员。</div>';
  return `
    <div class="inline-modal" role="dialog" aria-modal="true" aria-labelledby="rooms-title">
      <div class="modal-card proxy-pool-modal">
        <div class="modal-title" id="rooms-title">协作房间</div>
        <div class="modal-desc">开始接力后，房间按模板依次派发。每轮使用对应账号的隔离 Muse 会话和额度，定稿由你确认。</div>
        <div class="proxy-pool-body">
          ${view.book?.rooms.map((room) => {
            const attempt = room.attempts?.at(-1);
            const running = view.runningRoomId === room.id || ["dispatching","waiting_reply"].includes(room.phase);
            const multi = room.template.flow !== "solo";
            const hold = room.relay?.hold ?? "";
            const atLimit = room.rounds >= room.draft.roundLimit || room.returned >= 3;
            const proposal = room.relay?.proposal;
            const canSupplement = multi && !atLimit && !proposal && !room.relay?.pendingNote &&
              ["planning", "dispatching", "waiting_reply", "paused", "needs_user"].includes(room.phase);
            const sourceButton = (step: Attempt) => step.sessionId ? `<button type="button" class="btn" data-room-action="open_source"
                data-room-account="${esc(step.accountUuid)}" data-room-session="${esc(step.sessionId)}">打开原隔离会话</button>` : "";
            const actions = running ? [["interrupt", "停止本轮"]] :
              room.phase === "created" ? [["prepare", "校验并准备"]] :
              room.phase === "planning" ? [["dispatch", multi ? "开始接力" : "真实派发一轮"], ["pause", "暂停"], ["stop", "停止"]] :
              room.phase === "paused" ? [...(proposal ? [] : [["resume", "继续准备"]]), ["stop", "停止"]] :
              room.phase === "needs_reconciliation" ? [...(attempt?.userMessageId ? [["recover","只读回收回复"]] : []), ["abandon","结束房间"]] :
              room.phase === "needs_user" ? [
                ...(hold === "account_unavailable" && !atLimit && !proposal ? [["resume", "重新检查原分工"]] : []),
                ...(room.relay?.finalAttempt && !room.relay.pendingNote ? [["confirm", "确认文字定稿"]] : []),
                ["stop", "结束房间"],
              ] : room.phase === "pending_confirmation" ? [["confirm","确认文字定稿"], ["rework","要求返工"]] : [];
            if (canSupplement) actions.push(["supplement", running ? "本轮结束后暂停并补充" : "保存要求并暂停"]);
            return `
            <details class="hark-reply" data-room-panel="${room.id}">
              <summary>${esc(room.draft.name)} · ${phaseNames[room.phase] ?? esc(room.phase)} · ${room.rounds}/${room.draft.roundLimit} 轮</summary>
              <div class="hark-reply-text">${esc(room.draft.goal)}</div>
              <div class="modal-desc">${esc(room.template.title)} · v${room.template.version}</div>
              ${room.draft.members.map((member) => `<div>${memberLine(member)}</div>`).join("")}
              ${multi && ["build", "review"].includes(room.template.flow) ? `<div class="modal-desc">审核打回 ${room.returned}/3</div>` : ""}
              ${hold && room.phase === "needs_user" ? `<div role="status">${esc(holdReasons[hold] ?? "需要用户裁决")}</div>` : ""}
              ${running ? `<div role="status">${attempt?.stage && !attempt.delivery ? `${stageNames[attempt.stage]} · ${name(attempt.accountUuid)} · ` : ""}正在等待发送确认或回复。</div>` : ""}
              ${room.relay?.pendingNote ? '<div role="status">已记录补充要求。当前回复入账后暂停，等待你继续。</div>' : ""}
              ${room.relay?.notes?.length ? `<details><summary>用户补充（${room.relay.notes.length}）</summary>${room.relay.notes.map((note) => `<div class="hark-reply-text">${esc(note)}</div>`).join("")}</details>` : ""}
              ${room.attempts?.map((step, index) => step.delivery ? `<details data-room-step="${index}"><summary>第 ${index + 1} 次派发 · ${step.stage ? stageNames[step.stage] : "文字交付"}${step.review ? step.review.decision === "approve" ? "通过" : "打回" : ""}</summary>
                <div class="hark-reply-text">${esc(step.review?.reason ?? step.delivery.text)}</div>
                <div class="modal-desc">${name(step.accountUuid)}
                  · ${step.delivery.source === "history" ? "会话历史" : "实时订阅"} · ${new Date(step.delivery.capturedAtMs).toLocaleString()}</div>
                <div class="modal-desc" style="overflow-wrap:anywhere">SHA-256 ${esc(step.delivery.sha256)}</div>
                ${multi ? sourceButton(step) : ""}
              </details>` : "").join("") ?? ""}
              ${attempt && (!multi || !attempt.delivery) ? sourceButton(attempt) : ""}
              ${room.phase === "needs_reconciliation" ? '<div class="modal-desc">本轮结果需要核对。可以查看原会话或结束房间。</div>' : ""}
              ${room.phase === "needs_reconciliation" && attempt?.remoteCancelAcknowledged != null ? `<div class="modal-desc">${attempt.remoteCancelAcknowledged ? "远端已应答停止请求，生成是否停止仍需在原会话核对。" : "未收到远端停止应答，请在原会话核对。"}</div>` : ""}
              ${attempt?.status === "not_sent" ? '<div class="modal-desc">没有发出任务。请检查账号登录、代理和当前发送占用。</div>' : ""}
              ${canSupplement || room.phase === "pending_confirmation" ? `<label for="rework-${room.id}">${room.phase === "pending_confirmation" ? "返工要求" : "补充要求"}</label>
                <textarea id="rework-${room.id}" class="modal-input" maxlength="4000"></textarea>` : ""}
              ${proposal ? `<div class="hark-reply">
                <div>拟变更为以下成员与角色。确认后保留既有记录，按新分工重新规划；已用轮次与打回次数继续计算。</div>
                ${proposal.members.map((member) => `<div>${memberLine(member)}</div>`).join("")}
                <button type="button" class="btn" data-room-action="confirm_roster" data-room-id="${room.id}" data-room-revision="${room.revision}" data-room-proposal="${esc(proposal.id)}">确认变更成员与角色</button>
                <button type="button" class="btn" data-room-action="discard_roster" data-room-id="${room.id}" data-room-revision="${room.revision}">取消变更</button>
              </div>` : multi && !atLimit && !room.relay?.pendingNote && ["paused", "needs_user"].includes(room.phase) ? `<details data-room-roster>
                <summary>提议调整成员或固定角色</summary>
                ${roster(room.draft.members)}
                <button type="button" class="btn" data-room-action="propose_roster" data-room-id="${room.id}" data-room-revision="${room.revision}">预览变更</button>
              </details>` : ""}
              ${room.approvedAtMs ? `<div class="modal-desc">用户已确认 · ${new Date(room.approvedAtMs).toLocaleString()}</div>` : ""}
              <div class="proxy-pool-actions">
                ${actions.map(([action, label]) => `<button type="button" class="btn" data-room-action="${action}" data-room-id="${room.id}" data-room-revision="${room.revision}">${label}</button>`).join("")}
              </div>
            </details>`;
          }).join("") ?? ""}
          ${view.book ? `
            <details class="hark-reply" data-room-new><summary>新增房间</summary>
              <label for="room-name">房间名称</label><input id="room-name" class="modal-input" maxlength="80">
              <label for="room-goal">任务目标和参考资料</label><textarea id="room-goal" class="modal-input" maxlength="8000"></textarea>
              <label for="room-template">模板</label><select id="room-template" class="modal-input">${options}</select>
              <label for="room-round-limit">轮次上限</label><input id="room-round-limit" class="modal-input" type="number" min="1" max="24" value="8">
              ${roster([])}
              <button type="button" class="btn primary" data-room-action="create">保存房间</button>
            </details>
            <details class="hark-reply"><summary>自定义模板</summary>
              <label for="room-template-title">模板名称</label><input id="room-template-title" class="modal-input" maxlength="60">
              <label for="room-template-base">基础流程</label><select id="room-template-base" class="modal-input">${options}</select>
              <label for="room-template-min">至少几人</label><input id="room-template-min" class="modal-input" type="number" min="1" max="12" value="2">
              <button type="button" class="btn" data-room-action="save_template">保存模板</button>
            </details>` : ""}
        </div>
        <div id="room-result" class="modal-desc" role="status">${esc(view.error || (view.busy ? "正在读取房间…" : ""))}</div>
        <div class="modal-footer"><button id="modal-cancel-btn" class="btn" type="button">关闭</button></div>
      </div>
    </div>`;
}

export function roomCommand(button: HTMLElement): { action?: string; id?: string; revision?: number; [key: string]: unknown } {
  const action = button.dataset.roomAction;
  const value = (id: string) => document.querySelector<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>(`#${id}`)?.value ?? "";
  if (action === "create" || action === "propose_roster") {
    const panel = button.closest("[data-room-new], [data-room-roster]");
    const members = Array.from(panel?.querySelectorAll<HTMLElement>("[data-room-member]") ?? [], (element) => ({
      uuid: element.dataset.roomMember,
      roles: Array.from(element.querySelectorAll<HTMLInputElement>("input:checked"), (input) => input.value),
    })).filter((item) => item.roles.length > 0);
    if (action === "propose_roster") return { action, id: button.dataset.roomId, revision: Number(button.dataset.roomRevision), members };
    return { action, draft: { name: value("room-name"), goal: value("room-goal"),
      templateId: value("room-template"), roundLimit: Number(value("room-round-limit")), members } };
  }
  if (action === "save_template") return { action, title: value("room-template-title"),
    baseId: value("room-template-base"), minParticipants: Number(value("room-template-min")) };
  return { action, id: button.dataset.roomId, revision: Number(button.dataset.roomRevision),
    ...(action === "rework" || action === "supplement" ? { note: value(`rework-${button.dataset.roomId}`) } : {}),
    ...(action === "confirm_roster" ? { proposalId: button.dataset.roomProposal } : {}) };
}
