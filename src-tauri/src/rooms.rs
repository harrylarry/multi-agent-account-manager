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
enum Flow { Build, Brainstorm, Review, Compare }

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
      ("review", "评审", Flow::Review, 2), ("compare", "群发对比", Flow::Compare, 2)]
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
}

#[allow(dead_code)] // Model dispatch is connected in the next slice.
#[derive(Clone, Copy)]
enum Signal { Prepare, Valid, PlanReady, Written, Reply, Approved, Returned, Assembled, Confirm, Pause, Resume, Stop, Unknown, Failed }

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
        self.rounds += 1;
        WaitingReply
      }
      (WaitingReply, S::Reply) if matches!(self.template.flow, Flow::Build | Flow::Review) => Reviewing,
      (WaitingReply, S::Reply) => Assembling,
      (Reviewing, S::Approved) => Assembling,
      (Reviewing, S::Returned) => {
        self.returned += 1;
        if self.returned >= 3 || self.rounds >= self.draft.round_limit { NeedsUser } else { Planning }
      }
      (Assembling, S::Assembled) => PendingConfirmation,
      (PendingConfirmation, S::Confirm) => Finalized,
      (Planning, S::Pause) => Paused,
      (Paused, S::Resume) => Planning,
      (Dispatching | WaitingReply, S::Unknown | S::Stop) => NeedsReconciliation,
      (_, S::Stop) if self.in_progress() && self.phase != NeedsReconciliation => Stopped,
      (Validating | Planning | Reviewing | Assembling, S::Failed) => Failed,
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

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum Command {
  List,
  Create { draft: RoomDraft },
  Prepare { id: Uuid, revision: u64 },
  Pause { id: Uuid, revision: u64 },
  Resume { id: Uuid, revision: u64 },
  Stop { id: Uuid, revision: u64 },
  SaveTemplate { title: String, base_id: String, min_participants: usize },
}

pub fn apply(path: &Path, command: Command, accounts: &[AccountRecord], recover: bool) -> Result<RoomBook, String> {
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
      };
      room.validate(accounts)?;
      book.rooms.insert(0, room);
    }
    Command::SaveTemplate { title, base_id, min_participants } => {
      let title = title.trim();
      let base = book.templates.iter().find(|item| item.id == base_id).ok_or("基础流程不存在")?;
      let maximum = if base.flow == Flow::Brainstorm { 5 } else { 12 };
      if title.is_empty() || title.chars().count() > 60 || !(base.min_participants..=maximum).contains(&min_participants)
        || book.custom_templates.len() >= 32 || book.templates.iter().any(|item| item.title == title) {
        return Err("请检查模板名称、人数要求或重名".into());
      }
      let template = Template { id: Uuid::new_v4().to_string(), version: 1, title: title.into(), flow: base.flow, min_participants };
      book.templates.push(template.clone());
      book.custom_templates.push(template);
    }
    command => {
      let (id, revision, signal) = match command {
        Command::Prepare { id, revision } => (id, revision, Signal::Prepare),
        Command::Pause { id, revision } => (id, revision, Signal::Pause),
        Command::Resume { id, revision } => (id, revision, Signal::Resume),
        Command::Stop { id, revision } => (id, revision, Signal::Stop),
        _ => unreachable!(),
      };
      if matches!(signal, Signal::Prepare) && book.rooms.iter().any(Room::in_progress) {
        return Err("已有一个活跃房间，请先停止该房间".into());
      }
      let room = book.rooms.iter_mut().find(|room| room.id == id).ok_or("房间不存在")?;
      if room.revision != revision { return Err("房间状态已变化，请重新打开".into()); }
      if matches!(signal, Signal::Prepare | Signal::Resume) { room.validate(accounts)?; }
      room.transition(signal)?;
      if matches!(signal, Signal::Prepare) { room.transition(Signal::Valid)?; }
    }
  }
  save_json_atomic(path, &book)?;
  Ok(book)
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
}
