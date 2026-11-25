use crate::providers::ProviderId;
use crate::storage::{now_ms, save_json_atomic, AccountRecord};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use uuid::Uuid;

#[derive(Default)]
pub struct RoomGate(pub tokio::sync::Mutex<bool>);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
  Created, Validating, Planning, Dispatching, WaitingReply, Reviewing, Assembling,
  PendingConfirmation, Finalized, Paused, Stopped, NeedsUser, NeedsReconciliation, Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Role { Commander, Executor, Reviewer, Publisher, Discussant }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Flow { Build, Brainstorm, Review, Compare, Solo }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Template {
  id: String,
  version: u32,
  title: String,
  flow: Flow,
  min_participants: usize,
}

impl Template {
  fn builtin() -> Vec<Self> {
    [("development", "代码开发", Flow::Build, 2), ("brainstorm", "头脑风暴", Flow::Brainstorm, 3),
      ("review", "评审", Flow::Review, 2), ("compare", "群发对比", Flow::Compare, 2),
      ("solo", "单账号任务", Flow::Solo, 1)]
      .into_iter().map(|(id, title, flow, min_participants)| Self {
        id: id.into(), title: title.into(), version: 1, flow, min_participants,
      }).collect()
  }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Member { uuid: String, roles: Vec<Role> }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomDraft {
  name: String,
  goal: String,
  template_id: String,
  round_limit: u8,
  members: Vec<Member>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Transition { at_ms: u64, phase: Phase }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Room {
  id: Uuid,
  revision: u64,
  draft: RoomDraft,
  template: Template,
  phase: Phase,
  rounds: u8,
  returned: u8,
  run_id: Option<Uuid>,
  steps: Vec<Transition>,
  #[serde(default)]
  attempts: Vec<RoomAttempt>,
  #[serde(default)]
  goal_revision: u32,
  #[serde(default)]
  rework_note: String,
  #[serde(default)]
  approved_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomAttempt {
  pub id: Uuid,
  pub account_uuid: String,
  pub goal_revision: u32,
  pub session_id: Option<String>,
  pub send_id: Option<String>,
  pub user_message_id: Option<String>,
  pub status: String,
  pub delivery: Option<Delivery>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Delivery {
  kind: ArtifactKind,
  pub text: String,
  pub captured_at_ms: u64,
  pub source: String,
  pub sha256: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ArtifactKind { Text, Link, File, CodeChange }

#[allow(dead_code)] // Model dispatch is connected in the next slice.
#[derive(Clone, Copy)]
enum Signal { Prepare, Valid, PlanReady, Written, Reply, Approved, Returned, Assembled, Confirm, Rework, Recover, Abandon, Pause, Resume, Stop, Unknown, Failed }

impl Room {
  fn in_progress(&self) -> bool {
    !matches!(self.phase, Phase::Created | Phase::Stopped | Phase::Finalized | Phase::Failed)
  }

  fn transition(&mut self, signal: Signal) -> Result<(), String> {
    use Phase::*;
    use Signal as S;
    let next = match (self.phase, signal) {
      (Created, S::Prepare) => {
        self.run_id = Some(Uuid::new_v4());
        Validating
      }
      (Validating, S::Valid) => Planning,
      (Planning, S::PlanReady) if self.rounds < self.draft.round_limit => Dispatching,
      (Planning, S::PlanReady) => NeedsUser,
      (Dispatching, S::Written) => {
        WaitingReply
      }
      (WaitingReply, S::Reply) => {
        self.rounds += 1;
        if matches!(self.template.flow, Flow::Build | Flow::Review) { Reviewing } else { Assembling }
      }
      (Reviewing, S::Approved) => Assembling,
      (Reviewing, S::Returned) => {
        self.returned += 1;
        if self.returned >= 3 || self.rounds >= self.draft.round_limit { NeedsUser } else { Planning }
      }
      (Assembling, S::Assembled) => PendingConfirmation,
      (PendingConfirmation, S::Confirm) => Finalized,
      (PendingConfirmation, S::Rework) if self.rounds < self.draft.round_limit => Planning,
      (PendingConfirmation, S::Rework) => NeedsUser,
      (NeedsReconciliation, S::Recover) => WaitingReply,
      (NeedsReconciliation, S::Abandon) => Stopped,
      (Planning, S::Pause) => Paused,
      (Paused, S::Resume) => Planning,
      (Dispatching | WaitingReply, S::Unknown | S::Stop) => NeedsReconciliation,
      (_, S::Stop) if self.in_progress() && self.phase != NeedsReconciliation => Stopped,
      (Validating | Planning | Dispatching | Reviewing | Assembling, S::Failed) => Failed,
      _ => return Err("当前房间状态不接受此操作".into()),
    };
    self.phase = next;
    self.revision += 1;
    self.steps.push(Transition { at_ms: now_ms(), phase: next });
    Ok(())
  }

  fn validate(&self, accounts: &[AccountRecord]) -> Result<(), String> {
    if self.draft.members.len() < self.template.min_participants || self.draft.members.len() > 12 {
      return Err(format!("该模板至少需要 {} 个 Muse 成员，最多 12 个", self.template.min_participants));
    }
    let mut uuids = HashSet::new();
    for member in &self.draft.members {
      if !uuids.insert(&member.uuid) || !accounts.iter().any(|account| account.provider_id == ProviderId::Muse && account.uuid == member.uuid) {
        return Err("成员必须是不同的本机 Muse 账号".into());
      }
      let roles: HashSet<_> = member.roles.iter().collect();
      if roles.len() != member.roles.len() || roles.is_empty() || (roles.contains(&Role::Executor) && roles.contains(&Role::Reviewer)) {
        return Err("必须分配有效角色，执行者和审核者不能由同一账号担任".into());
      }
    }
    let count = |role| self.draft.members.iter().filter(|member| member.roles.contains(&role)).count();
    let flow_ok = match self.template.flow {
      Flow::Build => (1..=2).contains(&count(Role::Executor)) && count(Role::Reviewer) == 1 && count(Role::Discussant) == 0,
      Flow::Review => count(Role::Executor) == 1 && count(Role::Reviewer) == 1 && count(Role::Discussant) == 0,
      Flow::Brainstorm => (3..=5).contains(&count(Role::Discussant)) && count(Role::Reviewer) == 0 && count(Role::Executor) == 0,
      Flow::Compare => count(Role::Executor) == self.draft.members.len() && count(Role::Reviewer) == 0 && count(Role::Discussant) == 0,
      Flow::Solo => self.draft.members.len() == 1 && count(Role::Executor) == 1 && count(Role::Reviewer) == 0 && count(Role::Discussant) == 0,
    };
    if count(Role::Commander) != 1 || count(Role::Publisher) != 1 || !flow_ok {
      return Err("请设置一名指挥者和一名上传者，再核对模板所需的执行、审核或讨论角色".into());
    }
    Ok(())
  }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomBook {
  version: u32,
  custom_templates: Vec<Template>,
  rooms: Vec<Room>,
  #[serde(skip)]
  templates: Vec<Template>,
}

impl RoomBook {
  pub fn cancellation_target(&self, id: Uuid) -> Option<(String, Uuid)> {
    self.rooms.iter().find(|room| room.id == id)?.attempts.last()
      .map(|attempt| (attempt.account_uuid.clone(), attempt.id))
  }

  #[cfg(target_os = "ios")]
  pub fn is_in_flight(&self, id: Uuid, attempt_id: Uuid) -> bool {
    self.rooms.iter().find(|room| room.id == id).is_some_and(|room| {
      matches!(room.phase, Phase::Dispatching | Phase::WaitingReply)
        && room.attempts.last().is_some_and(|attempt| attempt.id == attempt_id)
    })
  }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum Command {
  List,
  Create { draft: RoomDraft },
  Prepare { id: Uuid, revision: u64 },
  Pause { id: Uuid, revision: u64 },
  Resume { id: Uuid, revision: u64 },
  Stop { id: Uuid, revision: u64 },
  Confirm { id: Uuid, revision: u64 },
  Rework { id: Uuid, revision: u64, note: String },
  Abandon { id: Uuid, revision: u64 },
  Interrupt { id: Uuid },
  SaveTemplate { title: String, base_id: String, min_participants: usize },
}

fn read(path: &Path, recover: bool) -> Result<RoomBook, String> {
  let mut book = match std::fs::read(path) {
    Ok(data) => serde_json::from_slice::<RoomBook>(&data).map_err(|_| "房间文件损坏，已保留原文件")?,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => RoomBook {
      version: 1, custom_templates: Vec::new(), rooms: Vec::new(), templates: Vec::new(),
    },
    Err(_) => return Err("房间文件读取失败".into()),
  };
  if book.version != 1 { return Err("房间文件版本不受支持".into()); }
  book.templates = Template::builtin();
  book.templates.extend(book.custom_templates.clone());
  let mut recovered = false;
  if recover {
    for room in &mut book.rooms {
      if matches!(room.phase, Phase::Dispatching | Phase::WaitingReply) {
        room.transition(Signal::Unknown)?;
        recovered = true;
      }
    }
  }
  if recovered { save_json_atomic(path, &book)?; }
  Ok(book)
}

pub fn apply(path: &Path, command: Command, accounts: &[AccountRecord], recover: bool) -> Result<RoomBook, String> {
  let mut book = read(path, recover)?;
  match command {
    Command::List => return Ok(book),
    Command::Create { mut draft } => {
      draft.name = draft.name.trim().into();
      draft.goal = draft.goal.trim().into();
      if draft.name.is_empty() || draft.name.chars().count() > 80 || draft.goal.is_empty()
        || draft.goal.chars().count() > 8000 || !(1..=24).contains(&draft.round_limit)
        || draft.members.len() > 12 || draft.members.iter().any(|member| member.roles.len() > 5)
        || book.rooms.len() >= 100 {
        return Err("请检查房间名称、目标、成员和 1–24 轮上限".into());
      }
      let template = book.templates.iter().find(|item| item.id == draft.template_id).ok_or("模板不存在")?.clone();
      let room = Room {
        id: Uuid::new_v4(), revision: 0, draft, template, phase: Phase::Created,
        rounds: 0, returned: 0, run_id: None, steps: vec![Transition { at_ms: now_ms(), phase: Phase::Created }],
        attempts: Vec::new(), goal_revision: 0, rework_note: String::new(), approved_at_ms: None,
      };
      room.validate(accounts)?;
      book.rooms.insert(0, room);
    }
    Command::SaveTemplate { title, base_id, min_participants } => {
      let title = title.trim();
      let base = book.templates.iter().find(|item| item.id == base_id).ok_or("基础流程不存在")?;
      let maximum = match base.flow { Flow::Solo => 1, Flow::Brainstorm => 5, _ => 12 };
      if title.is_empty() || title.chars().count() > 60 || !(base.min_participants..=maximum).contains(&min_participants)
        || book.custom_templates.len() >= 32 || book.templates.iter().any(|item| item.title == title) {
        return Err("请检查模板名称、人数要求或重名".into());
      }
      let template = Template { id: Uuid::new_v4().to_string(), version: 1, title: title.into(), flow: base.flow, min_participants };
      book.templates.push(template.clone());
      book.custom_templates.push(template);
    }
    command => {
      let mut rework = None;
      let (id, revision, signal) = match command {
        Command::Prepare { id, revision } => (id, revision, Signal::Prepare),
        Command::Pause { id, revision } => (id, revision, Signal::Pause),
        Command::Resume { id, revision } => (id, revision, Signal::Resume),
        Command::Stop { id, revision } => (id, revision, Signal::Stop),
        Command::Confirm { id, revision } => (id, revision, Signal::Confirm),
        Command::Rework { id, revision, note } => {
          let note = note.trim();
          if note.is_empty() || note.chars().count() > 4000 { return Err("请输入返工要求（最多 4000 字）".into()); }
          rework = Some(note.to_owned());
          (id, revision, Signal::Rework)
        }
        Command::Abandon { id, revision } => (id, revision, Signal::Abandon),
        Command::Interrupt { id } => {
          let room = book.rooms.iter().find(|room| room.id == id).ok_or("房间不存在")?;
          if !matches!(room.phase, Phase::Dispatching | Phase::WaitingReply) { return Err("该房间当前没有运行的轮次".into()); }
          (id, room.revision, Signal::Stop)
        }
        _ => unreachable!(),
      };
      if matches!(signal, Signal::Prepare) && book.rooms.iter().any(Room::in_progress) {
        return Err("已有一个活跃房间，请先停止该房间".into());
      }
      let room = book.rooms.iter_mut().find(|room| room.id == id).ok_or("房间不存在")?;
      if room.revision != revision { return Err("房间状态已变化，请重新打开".into()); }
      if matches!(signal, Signal::Confirm) && room.attempts.last().and_then(|attempt| attempt.delivery.as_ref()).is_none() {
        return Err("没有已回收并核验来源的交付物".into());
      }
      if matches!(signal, Signal::Prepare | Signal::Resume) { room.validate(accounts)?; }
      room.transition(signal)?;
      if matches!(signal, Signal::Confirm) { room.approved_at_ms = Some(now_ms()); }
      if let Some(note) = rework {
        room.goal_revision += 1;
        room.rework_note = note;
      }
      if matches!(signal, Signal::Prepare) { room.transition(Signal::Valid)?; }
    }
  }
  save_json_atomic(path, &book)?;
  Ok(book)
}

#[cfg(any(target_os = "ios", test))]
pub struct Dispatch {
  pub room_id: Uuid,
  pub attempt: RoomAttempt,
  pub prompt: String,
}

#[cfg(any(target_os = "ios", test))]
pub fn begin_dispatch(
  path: &Path, id: Uuid, revision: u64, accounts: &[AccountRecord], recover: bool,
) -> Result<Dispatch, String> {
  let mut book = read(path, false)?;
  if book.rooms.iter().any(|room| room.id != id && room.in_progress()) {
    return Err("已有其他活跃房间".into());
  }
  let room = book.rooms.iter_mut().find(|room| room.id == id).ok_or("房间不存在")?;
  if room.revision != revision || room.template.flow != Flow::Solo {
    return Err("房间状态已变化，或需要多账号编排".into());
  }
  room.validate(accounts)?;
  let member_uuid = room.draft.members[0].uuid.clone();
  let active = accounts.iter().any(|account| account.uuid == member_uuid && account.provider_id == ProviderId::Muse
    && account.auth_state == crate::storage::AuthState::Authenticated);
  if !active { return Err("请先登录该房间的 Muse 账号".into()); }
  let attempt = if recover {
    let attempt = room.attempts.last().filter(|attempt| attempt.session_id.is_some() && attempt.user_message_id.is_some())
      .cloned().ok_or("没有可核对的服务端回执，打开会话人工核对或结束该房间")?;
    room.transition(Signal::Recover)?;
    attempt
  } else {
    room.transition(Signal::PlanReady)?;
    if room.phase != Phase::Dispatching { save_json_atomic(path, &book)?; return Err("达到轮次上限，需要用户裁决".into()); }
    let attempt = RoomAttempt {
      id: Uuid::new_v4(), account_uuid: member_uuid, goal_revision: room.goal_revision,
      session_id: None, send_id: None, user_message_id: None, status: "prepared".into(), delivery: None,
    };
    room.attempts.push(attempt.clone());
    attempt
  };
  let mut prompt = format!(
    "这是用户的单账号协作房间任务。按下面的目标完成工作，并在回复中提交可供用户检查的文字交付。明确区分已完成的内容、未验证的外部操作和待交付项。\n\n任务目标及资料：\n{}",
    room.draft.goal
  );
  if !room.rework_note.is_empty() {
    prompt.push_str("\n\n用户返工要求：\n");
    prompt.push_str(&room.rework_note);
    if let Some(prior) = room.attempts.iter().rev().find_map(|attempt| attempt.delivery.as_ref()) {
      prompt.push_str("\n\n上次交付（参考）：\n");
      prompt.extend(prior.text.chars().take(5_000));
    }
  }
  save_json_atomic(path, &book)?;
  Ok(Dispatch { room_id: id, attempt, prompt })
}

#[cfg(any(target_os = "ios", test))]
pub enum Milestone {
  Ready { session: String, send_id: String },
  Acknowledged { user_message_id: String },
  Complete { text: String, source: &'static str },
  FailedBeforeSending,
  Uncertain,
}

#[cfg(any(target_os = "ios", test))]
pub fn record(path: &Path, dispatch: &Dispatch, event: Milestone) -> Result<RoomBook, String> {
  let mut book = read(path, false)?;
  let room = book.rooms.iter_mut().find(|room| room.id == dispatch.room_id).ok_or("房间不存在")?;
  let phase = room.phase;
  let attempt = room.attempts.last_mut()
    .filter(|attempt| attempt.id == dispatch.attempt.id && attempt.goal_revision == room.goal_revision)
    .ok_or("该回复属于旧轮次")?;
  match event {
    Milestone::Ready { session, send_id } if phase == Phase::Dispatching && attempt.send_id.is_none() => {
      attempt.session_id = Some(session);
      attempt.send_id = Some(send_id);
      attempt.status = "prepared".into();
    }
    Milestone::Acknowledged { user_message_id } if phase == Phase::Dispatching && attempt.send_id.is_some() => {
      attempt.status = "confirmed".into();
      attempt.user_message_id = Some(user_message_id);
      room.transition(Signal::Written)?;
    }
    Milestone::Complete { text, source } if phase == Phase::WaitingReply && attempt.user_message_id.is_some() => {
      if text.trim().is_empty() || text.len() > 128 * 1024 { return Err("回复为空或超过本机房间可保存范围".into()); }
      attempt.delivery = Some(Delivery {
        kind: ArtifactKind::Text, sha256: sha256(&text),
        text, source: source.to_owned(), captured_at_ms: now_ms(),
      });
      room.transition(Signal::Reply)?;
      room.transition(Signal::Assembled)?;
    }
    Milestone::FailedBeforeSending if phase == Phase::Dispatching => {
      attempt.status = "not_sent".into();
      room.transition(Signal::Failed)?;
    }
    Milestone::Uncertain if matches!(phase, Phase::Dispatching | Phase::WaitingReply) => {
      if attempt.user_message_id.is_none() { attempt.status = "unknown".into(); }
      room.transition(Signal::Unknown)?;
    }
    _ => return Err("房间已停止、改派或提交了该事件".into()),
  }
  save_json_atomic(path, &book)?;
  Ok(book)
}

#[cfg(any(target_os = "ios", test))]
pub fn sha256(text: &str) -> String {
  ring::digest::digest(&ring::digest::SHA256, text.as_bytes()).as_ref()
    .iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn room(flow: Flow, limit: u8) -> Room {
    let template = Template::builtin().into_iter().find(|item| item.flow == flow).unwrap();
    Room {
      id: Uuid::new_v4(), revision: 0, draft: RoomDraft {
        name: "Test".into(), goal: "Goal".into(), template_id: template.id.clone(),
        round_limit: limit, members: Vec::new(),
      }, template, phase: Phase::Created, rounds: 0, returned: 0, run_id: None, steps: Vec::new(),
      attempts: Vec::new(), goal_revision: 0, rework_note: String::new(), approved_at_ms: None,
    }
  }

  fn send_once(room: &mut Room) {
    room.transition(Signal::PlanReady).unwrap();
    room.transition(Signal::Written).unwrap();
    assert!(room.transition(Signal::Written).is_err());
    room.transition(Signal::Reply).unwrap();
  }

  #[test]
  fn final_review_requires_explicit_confirmation_and_refuses_late_replies() {
    let mut room = room(Flow::Build, 8);
    for signal in [Signal::Prepare, Signal::Valid, Signal::Pause, Signal::Resume] {
      room.transition(signal).unwrap();
    }
    send_once(&mut room);
    assert_eq!(room.phase, Phase::Reviewing);
    for signal in [Signal::Approved, Signal::Assembled] { room.transition(signal).unwrap(); }
    assert_eq!(room.phase, Phase::PendingConfirmation);
    assert!(room.transition(Signal::Reply).is_err());
    room.transition(Signal::Confirm).unwrap();
    assert_eq!(room.phase, Phase::Finalized);
  }

  #[test]
  fn first_budget_to_run_out_hands_control_to_user() {
    for limit in [1, 8] {
      let mut room = room(Flow::Review, limit);
      room.transition(Signal::Prepare).unwrap();
      room.transition(Signal::Valid).unwrap();
      for _ in 0..limit.min(3) {
        send_once(&mut room);
        room.transition(Signal::Returned).unwrap();
      }
      assert_eq!(room.phase, Phase::NeedsUser);
    }
  }

  #[test]
  fn interrupted_dispatch_is_durably_blocked_without_rewriting_again() {
    let dir = std::env::temp_dir().join(format!("maam-rooms-{}", Uuid::new_v4()));
    let path = dir.join("rooms-v1.json");
    let mut room = room(Flow::Compare, 8);
    for signal in [Signal::Prepare, Signal::Valid, Signal::PlanReady, Signal::Written] {
      room.transition(signal).unwrap();
    }
    let book = RoomBook { version: 1, custom_templates: Vec::new(), rooms: vec![room], templates: Vec::new() };
    save_json_atomic(&path, &book).unwrap();
    let recovered = apply(&path, Command::List, &[], true).unwrap();
    assert_eq!(recovered.rooms[0].phase, Phase::NeedsReconciliation);
    let again = apply(&path, Command::List, &[], true).unwrap();
    assert_eq!(recovered.rooms[0].revision, again.rooms[0].revision);
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir(dir).unwrap();
  }

  #[test]
  fn room_store_rejects_invalid_roles_and_a_second_active_room() {
    let dir = std::env::temp_dir().join(format!("maam-rooms-{}", Uuid::new_v4()));
    let path = dir.join("rooms-v1.json");
    let accounts = ["a", "b"].map(|id| serde_json::from_value::<AccountRecord>(serde_json::json!({
      "providerId":"muse", "uuid":id, "alias":id, "createdAtMs":0,
    })).unwrap());
    let mut draft = room(Flow::Review, 8).draft;
    draft.members = vec![
      Member { uuid: "a".into(), roles: vec![Role::Commander, Role::Reviewer] },
      Member { uuid: "b".into(), roles: vec![Role::Executor, Role::Reviewer, Role::Publisher] },
    ];
    assert!(apply(&path, Command::Create { draft: draft.clone() }, &accounts, false).is_err());
    assert!(!path.exists());
    draft.members[1].roles.retain(|role| *role != Role::Reviewer);
    apply(&path, Command::Create { draft: draft.clone() }, &accounts, false).unwrap();
    let two = apply(&path, Command::Create { draft }, &accounts, false).unwrap();
    let first = two.rooms[0].id;
    let second = two.rooms[1].id;
    let started = apply(&path, Command::Prepare { id: first, revision: 0 }, &accounts, false).unwrap();
    assert_eq!(started.rooms[0].phase, Phase::Planning);
    assert!(apply(&path, Command::Prepare { id: second, revision: 0 }, &accounts, false).is_err());
    apply(&path, Command::Stop { id: first, revision: 2 }, &accounts, false).unwrap();
    assert!(apply(&path, Command::Prepare { id: second, revision: 0 }, &accounts, true).is_ok());
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir(dir).unwrap();
  }

  #[test]
  fn solo_send_and_history_recovery_only_finalize_after_explicit_user_confirmation() {
    let dir = std::env::temp_dir().join(format!("maam-solo-{}", Uuid::new_v4()));
    let path = dir.join("rooms-v1.json");
    let mut draft = room(Flow::Solo, 2).draft;
    draft.members = vec![Member { uuid:"a".into(), roles:vec![Role::Commander, Role::Executor, Role::Publisher] }];
    let account = serde_json::from_value::<AccountRecord>(serde_json::json!({
      "providerId":"muse", "uuid":"a", "alias":"a", "createdAtMs":0, "authState":"authenticated",
    })).unwrap();
    let accounts = [account];
    let created = apply(&path, Command::Create { draft }, &accounts, false).unwrap();
    let id = created.rooms[0].id;
    let ready = apply(&path, Command::Prepare { id, revision:0 }, &accounts, false).unwrap();
    let dispatch = begin_dispatch(&path, id, ready.rooms[0].revision, &accounts, false).unwrap();
    assert!(dispatch.prompt.contains("Goal"));
    assert!(begin_dispatch(&path, id, ready.rooms[0].revision, &accounts, false).is_err());
    record(&path, &dispatch, Milestone::Ready { session:Uuid::new_v4().to_string(), send_id:"send".into() }).unwrap();
    let unknown = apply(&path, Command::List, &accounts, true).unwrap();
    assert_eq!(unknown.rooms[0].phase, Phase::NeedsReconciliation);
    assert!(begin_dispatch(&path, id, unknown.rooms[0].revision, &accounts, true).is_err());
    // Restore a persisted ACK scenario. It must permit only a read-only recovery.
    let mut acknowledged = unknown;
    acknowledged.rooms[0].phase = Phase::Dispatching;
    save_json_atomic(&path, &acknowledged).unwrap();
    record(&path, &dispatch, Milestone::Acknowledged { user_message_id:"receipt".into() }).unwrap();
    let before_recovery = record(&path, &dispatch, Milestone::Uncertain).unwrap();
    assert_eq!(before_recovery.rooms[0].rounds, 0);
    let recovery = begin_dispatch(&path, id, before_recovery.rooms[0].revision, &accounts, true).unwrap();
    assert_eq!(dispatch.attempt.id, recovery.attempt.id);
    let result = record(&path, &recovery, Milestone::Complete { text:"Delivered".into(), source:"history" }).unwrap();
    assert_eq!(result.rooms[0].phase, Phase::PendingConfirmation);
    assert_eq!(result.rooms[0].rounds, 1);
    let delivery = result.rooms[0].attempts[0].delivery.as_ref().unwrap();
    assert_eq!(delivery.sha256, sha256("Delivered"));
    assert!(record(&path, &dispatch, Milestone::Complete { text:"late".into(), source:"stream" }).is_err());
    let final_room = apply(&path, Command::Confirm { id, revision:result.rooms[0].revision }, &accounts, false).unwrap();
    assert_eq!(final_room.rooms[0].phase, Phase::Finalized);
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir(dir).unwrap();
  }
}
