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
  invalid_plan: "指挥回复的分工无法校验，已停止派发。",
  account_unavailable: "本轮没有发出。请检查该成员的登录、代理或发送占用，也可提议换人。",
  context_too_large: "必须交接的材料超过范围，已停止。本轮没有发送，前序材料仍保留。",
};

interface Member { uuid: string; roles: string[] }
interface Template {
  id: string; title: string; version: number; minParticipants: number; flow: string; workflow?: string[];
  workSeats?: number; roleSeats?: { executors?: number; discussants?: number };
}
const orderedStages = ["plan", "discuss", "work", "review", "summarize", "publish"];
const baseStages: Record<string, string[]> = {
  build: ["plan", "work", "review", "publish"], review: ["work", "review", "publish"],
  brainstorm: ["discuss", "summarize", "publish"], compare: ["work", "publish"], solo: [],
};
const builtInTemplates = ["development", "brainstorm", "review", "compare", "solo"];
interface Attempt { id?: string; accountUuid: string; status: string; sessionId: string | null; userMessageId: string | null;
  remoteCancelAcknowledged?: boolean;
  stage?: string; review?: { decision: "approve" | "return"; reason: string };
  delivery: { kind: string; text: string; capturedAtMs: number; source: string; sha256: string } | null }
interface Room {
  id: string; revision: number; phase: string; rounds: number; returned: number;
  draft: { name: string; goal: string; roundLimit: number; members: Member[]; automatic?: boolean; requiredArtifact?: string };
  template: Template;
  steps: { phase: string; atMs: number }[];
  attempts: Attempt[];
  approvedAtMs: number | null;
  conversation?: { commander: string; messages: { id:string; text:string; atMs:number; approvedAtMs?:number }[]; replies: Record<string,string> };
  artifacts?: { id:string; kind:string; name:string; sha256:string; byteCount:number; url?:string; verifiedCommit?:string; attemptId?:string }[];
  bindings?: { accountUuid:string; threadId:string; title:string }[];
  assignment?: {
    quotaWindow?: string; fixed?: string[];
    basis?: (Member & { remaining?: number; model?: string; modelSource?: string; fixed?: boolean })[];
    changes?: { from: string; to: string; atMs: number }[];
  };
  records?: {
    brief: { kind: string; text: string; accountUuid: string; sha256: string; goalRevision: number }[];
    references: { roomId: string; title: string; sha256: string; excerpt: string; truncated: boolean }[];
  };
  relay?: {
    hold?: string; pendingNote?: string; notes?: string[]; finalAttempt?: string;
    proposal?: { id: string; members: Member[] };
  };
}
export interface RoomBook { templates: Template[]; rooms: Room[] }
export interface RoomsView { kind: "rooms"; book: RoomBook | null; busy: boolean; error: string; runningRoomId?: string }

function hasRoomFinal(room: Room): boolean {
  return room.phase === "finalized" || Boolean(room.conversation?.messages.some((row) => row.approvedAtMs));
}

function relatedTerms(text: string): Set<string> {
  const terms = new Set<string>();
  for (const word of text.toLowerCase().match(/[a-z0-9]{3,}|[\p{Script=Han}]{2,}/gu) ?? []) {
    const letters = Array.from(word);
    if (/[a-z0-9]/.test(word)) terms.add(word);
    else for (let index = 1; index < letters.length; index++) terms.add(letters[index - 1] + letters[index]);
  }
  return terms;
}

export function markRoomSuggestions(book: RoomBook | null, goal: string): void {
  const terms = relatedTerms(goal);
  const related = (book?.rooms ?? []).filter(hasRoomFinal)
    .map((room) => ({ id: room.id, score: [...relatedTerms(room.draft.name + " " + room.draft.goal)].filter((term) => terms.has(term)).length }))
    .filter((room) => room.score >= 2).sort((a, b) => b.score - a.score).slice(0, 3);
  for (const label of document.querySelectorAll<HTMLElement>("[data-room-new] [data-related-room]")) {
    label.hidden = !related.some((room) => room.id === label.dataset.relatedRoom);
  }
}

export function updateRoomTemplateForm(book: RoomBook | null, changed: string): void {
  const field = (id: string) => document.getElementById(id) as HTMLInputElement | HTMLSelectElement | null;
  const base = field("room-template-base");
  const edit = field("room-template-edit");
  if (!base || !edit) return;
  const editing = book?.templates.find((item) => item.id === edit.value);
  if (changed === edit.id && editing) {
    base.value = book?.templates.find((item) => item.flow === editing.flow && builtInTemplates.includes(item.id))?.id ?? base.value;
  }
  const template = book?.templates.find((item) => item.id === base.value);
  if (!template) return;
  const required = baseStages[template.flow] ?? [];
  const source = changed === edit.id && editing ? editing : template;
  const stageFields = Array.from(document.querySelectorAll<HTMLInputElement>("[data-template-stage]"));
  if (changed === edit.id || changed === base.id || !changed) {
    const title = field("room-template-title");
    const minimum = field("room-template-min");
    if (title && changed === edit.id) title.value = editing?.title ?? "";
    if (minimum) minimum.value = String(source.minParticipants);
    const selected = source.workflow?.length ? source.workflow : required;
    for (const input of stageFields) input.checked = selected.includes(input.value);
    const executors = field("room-template-executors");
    const discussants = field("room-template-discussants");
    if (executors) executors.value = String(source.roleSeats?.executors ?? (source.flow === "build" ? source.workSeats ?? "" : ""));
    if (discussants) discussants.value = String(source.roleSeats?.discussants ?? (source.flow === "brainstorm" ? source.workSeats ?? "" : ""));
  }
  const work = stageFields.some((input) => input.value === "work" && input.checked);
  for (const input of stageFields) {
    const mandatory = required.includes(input.value) || (template.flow === "brainstorm" && work && input.value === "review");
    const unavailable = template.flow === "solo" || (input.value === "review" && !work);
    input.disabled = unavailable || mandatory;
    if (mandatory) input.checked = true;
    if (unavailable) input.checked = false;
  }
  for (const [role, stage] of [["executors", "work"], ["discussants", "discuss"]]) {
    const input = field(`room-template-${role}`);
    if (!input) continue;
    input.disabled = !stageFields.some((item) => item.value === stage && item.checked);
  }
}

export function renderRooms(view: RoomsView, accounts: AccountRecord[], esc: (text: string) => string): string {
  const muse = accounts.filter((account) => account.providerId === "muse");
  const options = view.book?.templates.map((item) => `<option value="${esc(item.id)}">${esc(item.title)} · 至少 ${item.minParticipants} 人</option>`).join("") ?? "";
  const customTemplates = view.book?.templates.filter((item) => !builtInTemplates.includes(item.id)) ?? [];
  const templateOptions = view.book?.templates.filter((item) => builtInTemplates.includes(item.id))
    .map((item) => `<option value="${esc(item.id)}">${esc(item.title)}</option>`).join("") ?? "";
  const alias = (uuid: string) => muse.find((account) => account.uuid === uuid)?.alias ?? "原成员";
  const name = (uuid: string) => esc(alias(uuid));
  const memberLine = (member: Member) => [name(member.uuid), member.roles.map((role) => roles.find(([id]) => id === role)?.[1] ?? "").join("、")].filter(Boolean).join(" · ");
  const referenceOptions = (target?: Room) => (view.book?.rooms ?? [])
    .filter((source) => hasRoomFinal(source) && source.id !== target?.id)
    .map((source) => `<label><input data-room-reference type="checkbox" value="${esc(source.id)}"
      ${target?.records?.references.some((reference) => reference.roomId === source.id) ? "checked" : ""}> ${esc(source.draft.name)}
      <small data-related-room="${esc(source.id)}" hidden> · 与目标相关</small></label>`).join("<br>");
  const roster = (selected: Member[], creating = false) => muse.map((account) => `<fieldset class="hark-reply" data-room-member="${esc(account.uuid)}"><legend>${esc(account.alias)}</legend>
    ${creating ? '<label><input type="checkbox" data-room-include> 纳入手选成员</label> ' : ""}
    ${roles.map(([id, label]) => `<label><input type="checkbox" data-room-role value="${id}" ${selected.some((member) => member.uuid === account.uuid && member.roles.includes(id)) ? "checked" : ""}> ${label}</label> `).join("")}
    </fieldset>`).join("") || '<div class="modal-desc">先添加 Muse 账号，再选择房间成员。</div>';
  return `
    <div class="inline-modal" role="dialog" aria-modal="true" aria-labelledby="rooms-title">
      <div class="modal-card proxy-pool-modal">
        <div class="modal-title" id="rooms-title">协作房间</div>
        <div class="modal-desc">你的消息先交给指挥，指挥可直接回答或分派给房间成员。各成员使用独立的 Muse 会话，任务定稿由你确认。</div>
        <div class="proxy-pool-body">
          ${view.book?.rooms.map((room) => {
            const attempt = room.attempts?.at(-1);
            const running = view.runningRoomId === room.id || ["dispatching","waiting_reply"].includes(room.phase);
            const chat = room.conversation;
            const multi = Boolean(chat) || room.template.flow !== "solo";
            const hold = room.relay?.hold ?? "";
            const atLimit = room.rounds >= room.draft.roundLimit || room.returned >= 3;
            const proposal = room.relay?.proposal;
            const canSupplement = multi && !atLimit && !proposal && !room.relay?.pendingNote &&
              ["planning", "dispatching", "waiting_reply", "paused", "needs_user"].includes(room.phase);
            const sourceButton = (step: Attempt) => step.sessionId ? `<button type="button" class="btn" data-room-action="open_source"
                data-room-account="${esc(step.accountUuid)}" data-room-session="${esc(step.sessionId)}">打开原隔离会话</button>` : "";
            const actions = running ? [["interrupt", "停止本轮"]] :
              room.phase === "created" ? (chat ? [] : [["prepare", "校验并准备"]]) :
              room.phase === "planning" ? [["dispatch", multi ? "开始接力" : "真实派发一轮"], ["pause", "暂停"], ["stop", "停止"]] :
              room.phase === "paused" ? [...(proposal ? [] : [["resume", "继续准备"]]), ["stop", "停止"]] :
              room.phase === "needs_reconciliation" ? [...(attempt?.userMessageId ? [["recover","只读回收回复"]] : []), ["abandon","结束房间"]] :
              room.phase === "needs_user" ? [
                ...(hold === "account_unavailable" && !atLimit && !proposal ? [["resume", "重新检查原分工"]] : []),
                ...(room.relay?.finalAttempt && !room.relay.pendingNote ? [["confirm", "确认文字定稿"]] : []),
                ["stop", chat ? "结束本次任务" : "结束房间"],
              ] : room.phase === "pending_confirmation" ? [["confirm","确认文字定稿"], ["rework","要求返工"], ...(chat ? [["stop", "结束本次任务"]] : [])] : [];
            if (canSupplement) actions.push(["supplement", running ? "本轮结束后暂停并补充" : "保存要求并暂停"]);
            return `
            <details class="hark-reply" data-room-panel="${room.id}">
              <summary>${esc(room.draft.name)} · ${chat && room.phase === "created" ? "可发消息" : phaseNames[room.phase] ?? esc(room.phase)}${chat && room.phase === "created" ? "" : ` · ${room.rounds}/${room.draft.roundLimit} 轮`}</summary>
              ${chat ? "" : `<div class="hark-reply-text">${esc(room.draft.goal)}</div><div class="modal-desc">${esc(room.template.title)} · v${room.template.version}</div>`}
              ${room.draft.automatic ? `<div class="modal-desc">自动分派${room.assignment?.quotaWindow ? ` · ${esc(room.assignment.quotaWindow)}额度窗口` : ""}</div>` : ""}
              ${room.draft.members.map((member) => {
                const basis = room.assignment?.basis?.find((entry) => entry.uuid === member.uuid);
                return `<div>${memberLine(member)}${room.assignment?.fixed?.includes(member.uuid) ? " · 已固定" : ""}${basis?.remaining != null ? ` · 分派时剩余 ${basis.remaining}%` : ""}
                  ${basis?.model ? ` · ${basis.modelSource === "manual" ? "手动模型" : "指纹候选"} ${esc(basis.model)}` : ""}</div>`;
              }).join("")}
              ${room.draft.requiredArtifact && room.draft.requiredArtifact !== "text" ? `<div class="modal-desc">定稿需要 ${({link:"HTTPS 链接",file:"校验文件",code_change:"已核验的公开提交"} as Record<string,string>)[room.draft.requiredArtifact]}</div>` : ""}
              ${room.artifacts?.length ? `<details><summary>交付凭证（${room.artifacts.length}）</summary>
                ${room.artifacts.map((item) => `<div class="hark-reply"><div>${esc(item.name)} · ${item.byteCount} 字节</div>
                  <div class="modal-desc" style="overflow-wrap:anywhere">SHA-256 ${esc(item.sha256)}</div>
                  ${item.verifiedCommit ? `<div class="modal-desc">GitHub 已核验提交 ${esc(item.verifiedCommit)}</div>` : ""}
                  ${item.url ? `<div class="hark-reply-text">${esc(item.url)}</div>
                    <button type="button" class="btn" data-room-action="open_artifact_link" data-room-url="${esc(item.url)}">打开凭证链接</button>`
                    : `<button type="button" class="btn" data-room-action="share_artifact" data-room-artifact="${esc(item.id)}" data-room-id="${room.id}">核验并分享文件</button>`}
                </div>`).join("")}</details>` : ""}
              ${room.bindings?.length ? `<details><summary>绑定会话（${room.bindings.length}）</summary>
                ${room.bindings.map((binding) => `<div>${name(binding.accountUuid)} · ${esc(binding.title)}
                  <button type="button" class="btn" data-room-action="open_source" data-room-account="${esc(binding.accountUuid)}" data-room-session="${esc(binding.threadId)}">打开绑定会话</button>
                  ${["created","paused"].includes(room.phase) ? `<button type="button" class="btn" data-room-action="unbind_session" data-room-id="${room.id}" data-room-revision="${room.revision}" data-room-account="${esc(binding.accountUuid)}" data-room-session="${esc(binding.threadId)}">解除绑定</button>` : ""}
                </div>`).join("")}</details>` : ""}
              ${["created","paused"].includes(room.phase) ? `
                <details data-room-materials><summary>添加交付凭证</summary>
                  <label>文件（最多 256 KiB）<input type="file" data-artifact-file></label>
                  <button type="button" class="btn" data-room-action="save_file" data-room-id="${room.id}" data-room-revision="${room.revision}">保存并校验文件</button>
                  <label>HTTPS 链接或 GitHub 公开提交<input type="url" class="modal-input" data-artifact-link></label>
                  <button type="button" class="btn" data-room-action="save_link" data-room-id="${room.id}" data-room-revision="${room.revision}">保存链接</button>
                  <button type="button" class="btn" data-room-action="verify_commit" data-room-id="${room.id}" data-room-revision="${room.revision}">在线核验提交</button>
                </details>
                <details data-room-bind><summary>绑定已有 Muse 会话</summary>
                  <label>账号<select class="modal-input" data-binding-account>${room.draft.members.map((member) => `<option value="${esc(member.uuid)}">${name(member.uuid)}</option>`).join("")}</select></label>
                  <button type="button" class="btn" data-room-action="list_binding">读取该账号会话</button>
                  <label>会话<select class="modal-input" data-binding-thread></select></label>
                  <button type="button" class="btn" data-room-action="bind_session" data-room-id="${room.id}" data-room-revision="${room.revision}">确认绑定</button>
                  <div class="modal-desc">绑定用于快速打开已有会话。接力仍在每轮的独立会话里发送。</div>
                </details>` : ""}
              ${room.assignment?.changes?.length ? `<details><summary>自动换人（${room.assignment.changes.length}）</summary>
                ${room.assignment.changes.map((change) => `<div>${name(change.from)} → ${name(change.to)} · ${new Date(change.atMs).toLocaleString()}</div>`).join("")}</details>` : ""}
              <details><summary>运行简报</summary>
                <div>阶段 · ${phaseNames[room.phase] ?? esc(room.phase)}</div>
                ${room.records?.brief.length ? room.records.brief.map((entry) => `<div class="hark-reply">
                  <div>${({decision:"决定", dependency:"依赖与计划", todo:"待办", observation:"会话摘录"} as Record<string,string>)[entry.kind] ?? "记录"} · ${name(entry.accountUuid)} · 目标版本 ${entry.goalRevision + 1}</div>
                  <div class="hark-reply-text">${esc(entry.text)}</div><div class="modal-desc">来源 SHA-256 ${esc(entry.sha256.slice(0, 12))}</div>
                  </div>`).join("") : '<div class="modal-desc">等待会话结果入账。</div>'}
              </details>
              ${room.records?.references.length ? `<details><summary>已引用定稿（${room.records.references.length}）</summary>
                ${room.records.references.map((reference) => `<div class="hark-reply"><div>${esc(reference.title)}${reference.truncated ? " · 节选" : ""}</div>
                  <div class="hark-reply-text">${esc(reference.excerpt)}</div><div class="modal-desc">来源 SHA-256 ${esc(reference.sha256.slice(0, 12))}</div>
                  ${view.book?.rooms.some((source) => source.id === reference.roomId) ? `<button type="button" class="btn" data-room-action="show_record" data-room-id="${esc(reference.roomId)}">查看源房间</button>` : "原房间已删除"}
                </div>`).join("")}</details>` : ""}
              ${["created","paused"].includes(room.phase) && (referenceOptions(room) || room.records?.references.length) ? `<details data-room-references><summary>调整旧房间引用</summary>
                ${referenceOptions(room)}
                <button type="button" class="btn" data-room-action="set_references" data-room-id="${room.id}" data-room-revision="${room.revision}">确认选定引用</button></details>` : ""}
              ${multi && ["build", "review"].includes(room.template.flow) ? `<div class="modal-desc">审核打回 ${room.returned}/3</div>` : ""}
              ${hold && room.phase === "needs_user" ? `<div role="status">${esc(holdReasons[hold] ?? "需要用户裁决")}</div>` : ""}
              ${running ? `<div role="status">${attempt?.stage && !attempt.delivery ? `${stageNames[attempt.stage]} · ${name(attempt.accountUuid)} · ` : ""}正在等待发送确认或回复。</div>` : ""}
              ${room.relay?.pendingNote ? '<div role="status">已记录补充要求。当前回复入账后暂停，等待你继续。</div>' : ""}
              ${room.relay?.notes?.length ? `<details><summary>用户补充（${room.relay.notes.length}）</summary>${room.relay.notes.map((note) => `<div class="hark-reply-text">${esc(note)}</div>`).join("")}</details>` : ""}
              ${chat?.messages.length || room.attempts?.some((step) => step.delivery) ? `<ol class="room-timeline" aria-label="房间消息">${[
                ...(chat?.messages ?? []).map((row) => ({ at:row.atMs, html:`<li class="room-message"><div class="room-message-heading"><strong>你</strong><span class="modal-desc">${new Date(row.atMs).toLocaleTimeString()}${row.approvedAtMs ? " · 已确认定稿" : ""}</span></div><div class="hark-reply-text">${esc(row.text)}</div></li>` })),
                ...(room.attempts ?? []).flatMap((step, index) => step.delivery ? [{at:step.delivery.capturedAtMs, html:`<li class="room-message">
                <div class="room-message-heading"><span class="room-message-avatar" aria-hidden="true">${esc(Array.from(alias(step.accountUuid).trim())[0] ?? "●")}</span>
                  <div><strong>${name(step.accountUuid)}</strong><div class="modal-desc">${step.stage ? esc(stageNames[step.stage] ?? step.stage) : "文字交付"}${step.review ? step.review.decision === "approve" ? " · 通过" : " · 打回" : ""}
                  · ${new Date(step.delivery.capturedAtMs).toLocaleTimeString()}</div></div></div>
                <div class="hark-reply-text">${esc((step.id ? chat?.replies?.[step.id] : undefined) ?? step.review?.reason ?? (step.id && room.artifacts?.some((item) => item.attemptId === step.id)
                  ? step.delivery.text.slice(0, 1200) + "…\n完整正文见交付凭证中的 Markdown 附件。"
                  : step.delivery.text))}</div>
                <details data-room-step="${index}"><summary>消息来源</summary><div class="modal-desc">${step.delivery.source === "history" ? "会话历史" : "实时订阅"} · ${new Date(step.delivery.capturedAtMs).toLocaleString()}</div>
                <div class="modal-desc" style="overflow-wrap:anywhere">SHA-256 ${esc(step.delivery.sha256)}</div>
                ${multi ? sourceButton(step) : ""}
                </details></li>`}] : []),
              ].sort((a,b) => a.at - b.at).map((row) => row.html).join("")}</ol>` : ""}
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
              </div>` : !chat && multi && !atLimit && !room.relay?.pendingNote && ["paused", "needs_user"].includes(room.phase) ? `<details data-room-roster>
                <summary>提议调整成员或固定角色</summary>
                ${roster(room.draft.members)}
                <button type="button" class="btn" data-room-action="propose_roster" data-room-id="${room.id}" data-room-revision="${room.revision}">预览变更</button>
              </details>` : ""}
              ${room.approvedAtMs ? `<div class="modal-desc">用户已确认 · ${new Date(room.approvedAtMs).toLocaleString()}</div>` : ""}
              <div class="proxy-pool-actions">
                ${actions.map(([action, label]) => `<button type="button" class="btn" data-room-action="${action}" data-room-id="${room.id}" data-room-revision="${room.revision}">${label}</button>`).join("")}
              </div>
              ${chat && ["created","finalized","stopped","failed"].includes(room.phase) ? `<label for="message-${room.id}">发给房间</label>
                <textarea id="message-${room.id}" class="modal-input" maxlength="8000" placeholder="提问，或告诉指挥要完成什么"></textarea>
                <button type="button" class="btn primary" data-room-action="post_message" data-room-id="${room.id}" data-room-revision="${room.revision}">发送</button>` : ""}
              ${!chat && ["finalized","stopped","failed"].includes(room.phase) ? `<details data-room-topic><summary>开新话题</summary>
                <p class="modal-desc">沿用模板和分派方式。自动模式只保留你确认的固定角色，其余成员重新分派。</p>
                <label>话题名称<input class="modal-input" data-topic-name maxlength="80"></label>
                <label>任务目标<textarea class="modal-input" data-topic-goal maxlength="8000"></textarea></label>
                ${room.phase === "finalized" ? '<label><input type="checkbox" data-topic-reference> 引用这次定稿</label>' : ""}
                <button type="button" class="btn" data-room-action="new_topic" data-room-id="${room.id}" data-room-revision="${room.revision}">保存新话题</button></details>` : ""}
              ${["created","stopped","failed","finalized"].includes(room.phase) ? `<details data-room-delete><summary>删除房间记录</summary>
                <label><input type="checkbox"> 确认删除本机记录。其他房间已引用的节选会保留。</label>
                <button type="button" class="btn" data-room-action="delete_record" data-room-id="${room.id}" data-room-revision="${room.revision}">删除本机记录</button></details>` : ""}
            </details>`;
          }).join("") ?? ""}
          ${view.book ? `
            <details class="hark-reply" data-conversation-new><summary>新增房间</summary>
              <label for="conversation-name">房间名称（可选）</label><input id="conversation-name" class="modal-input" maxlength="80">
              <fieldset class="hark-reply"><legend>选择成员和一名指挥</legend>${muse.map((account) => `<div class="proxy-pool-actions">
                <label><input type="checkbox" data-conversation-member value="${esc(account.uuid)}"> ${esc(account.alias)}</label>
                <label><input type="radio" name="conversation-commander" value="${esc(account.uuid)}" aria-label="指定 ${esc(account.alias)} 为指挥"> 指挥</label>
              </div>`).join("")}</fieldset>
              <button type="button" class="btn primary" data-room-action="create_conversation">创建房间</button>
            </details>
            <details class="hark-reply" data-room-new><summary>按模板创建任务房间</summary>
              <label for="room-name">房间名称</label><input id="room-name" class="modal-input" maxlength="80">
              <label for="room-goal">任务目标和参考资料</label><textarea id="room-goal" class="modal-input" maxlength="8000"></textarea>
              <label for="room-template">模板</label><select id="room-template" class="modal-input">${options}</select>
              <label for="room-round-limit">轮次上限</label><input id="room-round-limit" class="modal-input" type="number" min="1" max="24" value="8">
              <label for="room-required-artifact">定稿要求</label><select id="room-required-artifact" class="modal-input">
                <option value="">文字回复</option><option value="link">HTTPS 链接</option><option value="file">校验文件</option><option value="code_change">已核验的 GitHub 公开提交</option>
              </select>
              <label for="room-assignment">成员分派</label><select id="room-assignment" class="modal-input">
                <option value="manual">手选成员与角色</option><option value="automatic">自动分派与换人</option>
              </select>
              <div class="modal-desc">自动模式可从所有符合条件的 Muse 账号选人和换人。手选模式可勾成员，让房间在这些成员内分派。勾具体角色即固定分工。</div>
              <button type="button" class="btn" data-room-action="refresh_assignment_quota">刷新 Muse 额度</button>
              ${roster([], true)}
              ${referenceOptions() ? `<details><summary>选择要引用的旧房间（最多 5 个）</summary>${referenceOptions()}</details>` : ""}
              <button type="button" class="btn primary" data-room-action="create">保存房间</button>
            </details>
            <details class="hark-reply" data-template-editor><summary>自定义模板</summary>
              <label for="room-template-edit">模板版本</label><select id="room-template-edit" class="modal-input">
                <option value="">新建</option>${customTemplates.map((item) => `<option value="${esc(item.id)}" data-version="${item.version}">${esc(item.title)} · v${item.version}</option>`).join("")}
              </select>
              <label for="room-template-title">模板名称</label><input id="room-template-title" class="modal-input" maxlength="60">
              <label for="room-template-base">基础流程</label><select id="room-template-base" class="modal-input">${templateOptions}</select>
              <label for="room-template-min">至少几人</label><input id="room-template-min" class="modal-input" type="number" min="1" max="12" value="2">
              <fieldset class="hark-reply"><legend>流程阶段（按顺序执行）</legend>
                ${orderedStages.map((stage) => `<label><input type="checkbox" data-template-stage value="${stage}" ${baseStages.build.includes(stage) ? "checked disabled" : ""}> ${stageNames[stage]}</label>`).join(" ")}
              </fieldset>
              <div class="modal-desc">指挥与上传各一席，可兼任。启用审核时需一名独立审核者，执行者不能兼审核。</div>
              <label for="room-template-executors">执行者人数（可选）</label>
              <input id="room-template-executors" class="modal-input" type="number" min="1" max="12" placeholder="按流程分派">
              <label for="room-template-discussants">讨论者人数（可选）</label>
              <input id="room-template-discussants" class="modal-input" type="number" min="2" max="12" placeholder="按流程分派" disabled>
              <button type="button" class="btn" data-room-action="save_template">保存模板</button>
            </details>` : ""}
        </div>
        <div id="room-result" class="modal-desc" role="status">${esc(view.error || (view.busy ? "正在读取房间…" : ""))}</div>
        <div class="modal-footer">
          ${view.error && !view.book ? '<button class="btn primary" type="button" data-room-action="list">重试读取房间</button>' : ""}
          <button id="modal-cancel-btn" class="btn" type="button">关闭</button>
        </div>
      </div>
    </div>`;
}

export function roomCommand(button: HTMLElement): { action?: string; id?: string; revision?: number; [key: string]: unknown } {
  const action = button.dataset.roomAction;
  const value = (id: string) => document.querySelector<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>(`#${id}`)?.value ?? "";
  const sourceIds = (scope: Element | null) =>
    Array.from(scope?.querySelectorAll<HTMLInputElement>("[data-room-reference]:checked") ?? [], (input) => input.value);
  if (action === "create_conversation") {
    const panel = button.closest("[data-conversation-new]");
    const commander = panel?.querySelector<HTMLInputElement>('input[name="conversation-commander"]:checked')?.value ?? "";
    const members = Array.from(panel?.querySelectorAll<HTMLInputElement>("[data-conversation-member]:checked") ?? [], (input) => input.value);
    if (commander && !members.includes(commander)) members.push(commander);
    return { action, name:value("conversation-name"), members, commander };
  }
  if (action === "new_topic") {
    const panel = button.closest("[data-room-topic]");
    return { action, id: button.dataset.roomId, revision: Number(button.dataset.roomRevision),
      name: panel?.querySelector<HTMLInputElement>("[data-topic-name]")?.value ?? "",
      goal: panel?.querySelector<HTMLTextAreaElement>("[data-topic-goal]")?.value ?? "",
      includeFinal: Boolean(panel?.querySelector("[data-topic-reference]:checked")) };
  }
  if (action === "create" || action === "propose_roster") {
    const panel = button.closest("[data-room-new], [data-room-roster]");
    const automatic = action === "create" && value("room-assignment") === "automatic";
    const members = Array.from(panel?.querySelectorAll<HTMLElement>("[data-room-member]") ?? [], (element) => ({
      selected: !automatic && !!element.querySelector("[data-room-include]:checked"),
      member: { uuid: element.dataset.roomMember,
        roles: Array.from(element.querySelectorAll<HTMLInputElement>("[data-room-role]:checked"), (input) => input.value) },
    })).filter((item) => item.selected || item.member.roles.length > 0).map((item) => item.member);
    if (action === "propose_roster") return { action, id: button.dataset.roomId, revision: Number(button.dataset.roomRevision), members };
    return { action, references: sourceIds(panel), draft: { name: value("room-name"), goal: value("room-goal"),
      templateId: value("room-template"), roundLimit: Number(value("room-round-limit")), members,
      automatic, requiredArtifact: value("room-required-artifact") || null } };
  }
  if (action === "save_template") {
    const workflow = Array.from(document.querySelectorAll<HTMLInputElement>("[data-template-stage]:checked"), (input) => input.value);
    const seatCount = (id: string) => {
      const input = document.querySelector<HTMLInputElement>(`#room-template-${id}`);
      return input && !input.disabled && input.value ? input.valueAsNumber : null;
    };
    const edit = document.querySelector<HTMLSelectElement>("#room-template-edit")?.selectedOptions[0];
    return { action, title:value("room-template-title"), baseId:value("room-template-base"),
      minParticipants:Number(value("room-template-min")), workflow,
      roleSeats:{ executors:seatCount("executors"), discussants:seatCount("discussants") }, editId:edit?.value || null,
      version: edit?.value ? Number(edit.dataset.version) : null };
  }
  return { action, id: button.dataset.roomId, revision: Number(button.dataset.roomRevision),
    ...(action === "post_message" ? { text:value(`message-${button.dataset.roomId}`) } : {}),
    ...(action === "rework" || action === "supplement" ? { note: value(`rework-${button.dataset.roomId}`) } : {}),
    ...(action === "confirm_roster" ? { proposalId: button.dataset.roomProposal } : {}),
    ...(action === "unbind_session" ? { accountUuid:button.dataset.roomAccount, threadId:button.dataset.roomSession } : {}),
    ...(action === "set_references" ? { sourceIds: sourceIds(button.closest("[data-room-references]")) } : {}) };
}
