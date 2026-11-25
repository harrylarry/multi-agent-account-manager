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

interface Template { id: string; title: string; version: number; minParticipants: number }
interface Room {
  id: string; revision: number; phase: string; rounds: number; returned: number;
  draft: { name: string; goal: string; roundLimit: number; members: { uuid: string; roles: string[] }[] };
  template: Template;
  steps: { phase: string; atMs: number }[];
}
export interface RoomBook { templates: Template[]; rooms: Room[] }
export interface RoomsView { kind: "rooms"; book: RoomBook | null; busy: boolean; error: string }

export function renderRooms(view: RoomsView, accounts: AccountRecord[], esc: (text: string) => string): string {
  const muse = accounts.filter((account) => account.providerId === "muse");
  const options = view.book?.templates.map((item) => `<option value="${esc(item.id)}">${esc(item.title)} · 至少 ${item.minParticipants} 人</option>`).join("") ?? "";
  return `
    <div class="inline-modal" role="dialog" aria-modal="true" aria-labelledby="rooms-title">
      <div class="modal-card proxy-pool-modal">
        <div class="modal-title" id="rooms-title">协作房间</div>
        <div class="modal-desc">本版用于建房、核对角色和保存计划。任务发送将在下一阶段接入。</div>
        <div class="proxy-pool-body">
          ${view.book?.rooms.map((room) => `
            <details class="hark-reply">
              <summary>${esc(room.draft.name)} · ${phaseNames[room.phase] ?? esc(room.phase)} · ${room.rounds}/${room.draft.roundLimit} 轮</summary>
              <div class="hark-reply-text">${esc(room.draft.goal)}</div>
              <div class="modal-desc">${esc(room.template.title)} · v${room.template.version}</div>
              ${room.draft.members.map((member) => `<div>${esc(muse.find((account) => account.uuid === member.uuid)?.alias ?? "已移除成员")} · ${member.roles.map((role) => roles.find(([id]) => id === role)?.[1] ?? "").join("、")}</div>`).join("")}
              <div class="modal-desc">${room.steps.map((step) => phaseNames[step.phase]).join(" → ")}</div>
              <div class="proxy-pool-actions">
                ${(room.phase === "created" ? [["prepare", "校验并准备"]] :
                  room.phase === "planning" ? [["pause", "暂停"], ["stop", "停止"]] :
                  room.phase === "paused" ? [["resume", "继续准备"], ["stop", "停止"]] : [])
                  .map(([action, label]) => `<button type="button" class="btn" data-room-action="${action}" data-room-id="${room.id}" data-room-revision="${room.revision}">${label}</button>`).join("")}
              </div>
            </details>`).join("") ?? ""}
          ${view.book ? `
            <details class="hark-reply"><summary>新增房间</summary>
              <label for="room-name">房间名称</label><input id="room-name" class="modal-input" maxlength="80">
              <label for="room-goal">任务目标和参考资料</label><textarea id="room-goal" class="modal-input" maxlength="8000"></textarea>
              <label for="room-template">模板</label><select id="room-template" class="modal-input">${options}</select>
              <label for="room-round-limit">轮次上限</label><input id="room-round-limit" class="modal-input" type="number" min="1" max="24" value="8">
              ${muse.map((account) => `<fieldset class="hark-reply" data-room-member="${esc(account.uuid)}"><legend>${esc(account.alias)}</legend>
                ${roles.map(([id, label]) => `<label><input type="checkbox" value="${id}"> ${label}</label> `).join("")}
              </fieldset>`).join("") || '<div class="modal-desc">先添加 Muse 账号，再选择房间成员。</div>'}
              <button type="button" class="btn primary" data-room-action="create">保存房间</button>
            </details>
            <details class="hark-reply"><summary>自定义模板</summary>
              <label for="room-template-title">模板名称</label><input id="room-template-title" class="modal-input" maxlength="60">
              <label for="room-template-base">基础流程</label><select id="room-template-base" class="modal-input">${options}</select>
              <label for="room-template-min">至少几人</label><input id="room-template-min" class="modal-input" type="number" min="2" max="12" value="2">
              <button type="button" class="btn" data-room-action="save_template">保存模板</button>
            </details>` : ""}
        </div>
        <div id="room-result" class="modal-desc" role="status">${esc(view.error || (view.busy ? "正在读取房间…" : ""))}</div>
        <div class="modal-footer"><button id="modal-cancel-btn" class="btn" type="button">关闭</button></div>
      </div>
    </div>`;
}

export function roomCommand(button: HTMLElement): object {
  const action = button.dataset.roomAction;
  const value = (id: string) => document.querySelector<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>(`#${id}`)?.value ?? "";
  if (action === "create") {
    const members = Array.from(document.querySelectorAll<HTMLElement>("[data-room-member]"), (element) => ({
      uuid: element.dataset.roomMember,
      roles: Array.from(element.querySelectorAll<HTMLInputElement>("input:checked"), (input) => input.value),
    })).filter((item) => item.roles.length > 0);
    return { action, draft: { name: value("room-name"), goal: value("room-goal"),
      templateId: value("room-template"), roundLimit: Number(value("room-round-limit")), members } };
  }
  if (action === "save_template") return { action, title: value("room-template-title"),
    baseId: value("room-template-base"), minParticipants: Number(value("room-template-min")) };
  return { action, id: button.dataset.roomId, revision: Number(button.dataset.roomRevision) };
}
