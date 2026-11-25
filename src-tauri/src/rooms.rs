use crate::providers::ProviderId;
use crate::storage::{now_ms, save_json_atomic, AccountRecord};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use uuid::Uuid;

#[path = "room_relay.rs"]
mod relay;
use relay::{Hold, Relay, Stage, Verdict};

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
pub struct Member { uuid: String, roles: Vec<Role> }

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
  #[serde(default)]
  relay: Relay,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomAttempt {
  pub id: Uuid,
  #[serde(default)]
  pub worker_id: Option<Uuid>,
  pub account_uuid: String,
  pub goal_revision: u32,
  pub session_id: Option<String>,
  pub send_id: Option<String>,
  pub user_message_id: Option<String>,
  pub status: String,
  pub delivery: Option<Delivery>,
  #[serde(default)]
  stage: Option<Stage>,
  #[serde(default)]
  review: Option<Verdict>,
  #[serde(default)]
  remote_cancel_acknowledged: Option<bool>,
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

#[derive(Clone, Copy)]
enum Signal { Prepare, Valid, PlanReady, Written, Reply, Assembled, Confirm, Rework, Recover, Abandon, Pause, Resume, Stop, Unknown, Failed }

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
        Assembling
      }
      (Assembling, S::Assembled) => PendingConfirmation,
      (PendingConfirmation | NeedsUser, S::Confirm) => Finalized,
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
    self.set_phase(next);
    Ok(())
  }

  fn set_phase(&mut self, next: Phase) {
    self.phase = next;
    self.revision += 1;
    self.steps.push(Transition { at_ms: now_ms(), phase: next });
  }

  fn validate(&self, accounts: &[AccountRecord]) -> Result<(), String> {
    self.validate_members(accounts, &self.draft.members)
  }

  fn validate_members(&self, accounts: &[AccountRecord], members: &[Member]) -> Result<(), String> {
    if members.len() < self.template.min_participants || members.len() > 12 {
      return Err(format!("该模板至少需要 {} 个 Muse 成员，最多 12 个", self.template.min_participants));
    }
    let mut uuids = HashSet::new();
    for member in members {
      if !uuids.insert(&member.uuid) || !accounts.iter().any(|account| account.provider_id == ProviderId::Muse && account.uuid == member.uuid) {
        return Err("成员必须是不同的本机 Muse 账号".into());
      }
      let roles: HashSet<_> = member.roles.iter().collect();
      if roles.len() != member.roles.len() || roles.is_empty() || (roles.contains(&Role::Executor) && roles.contains(&Role::Reviewer)) {
        return Err("必须分配有效角色，执行者和审核者不能由同一账号担任".into());
      }
    }
    let count = |role| members.iter().filter(|member| member.roles.contains(&role)).count();
    let flow_ok = match self.template.flow {
      Flow::Build => (1..=2).contains(&count(Role::Executor)) && count(Role::Reviewer) == 1 && count(Role::Discussant) == 0,
      Flow::Review => count(Role::Executor) == 1 && count(Role::Reviewer) == 1 && count(Role::Discussant) == 0,
      Flow::Brainstorm => (3..=5).contains(&count(Role::Discussant)) && count(Role::Reviewer) == 0 && count(Role::Executor) == 0,
      Flow::Compare => count(Role::Executor) == members.len() && count(Role::Reviewer) == 0 && count(Role::Discussant) == 0,
      Flow::Solo => members.len() == 1 && count(Role::Executor) == 1 && count(Role::Reviewer) == 0 && count(Role::Discussant) == 0,
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
  #[cfg(target_os = "ios")]
  pub fn next_relay_revision(&self, id: Uuid) -> Option<u64> {
    let room = self.rooms.iter().find(|room| room.id == id)?;
    (room.template.flow != Flow::Solo && room.phase == Phase::Planning).then_some(room.revision)
  }

  #[cfg(target_os = "ios")]
  pub fn after_relay_attempt(&self, id: Uuid, completed: &RoomAttempt) -> Option<u64> {
    self.rooms.iter().find(|room| room.id == id)?.attempts.last()
      .filter(|step| step.id == completed.id && step.worker_id == completed.worker_id && step.delivery.is_some())?;
    self.next_relay_revision(id)
  }

  pub fn cancellation_target(&self, id: Uuid) -> Option<(String, Uuid)> {
    self.rooms.iter().find(|room| room.id == id)?.attempts.last()
      .map(|attempt| (attempt.account_uuid.clone(), attempt.worker_id.unwrap_or(attempt.id)))
  }

  #[cfg(target_os = "ios")]
  pub fn is_in_flight(&self, id: Uuid, attempt: &RoomAttempt) -> bool {
    self.rooms.iter().find(|room| room.id == id).is_some_and(|room| {
      matches!(room.phase, Phase::Dispatching | Phase::WaitingReply)
        && room.attempts.last().is_some_and(|latest| latest.id == attempt.id && latest.worker_id == attempt.worker_id)
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
  Supplement { id: Uuid, revision: u64, note: String },
  ProposeRoster { id: Uuid, revision: u64, members: Vec<Member> },
  ConfirmRoster { id: Uuid, revision: u64, proposal_id: Uuid },
  DiscardRoster { id: Uuid, revision: u64 },
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
        relay: Relay::default(),
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
    Command::Supplement { id, revision, note } => {
      let room = room_at_revision(&mut book, id, revision)?;
      room.supplement(note)?;
    }
    Command::ProposeRoster { id, revision, members } => {
      let room = room_at_revision(&mut book, id, revision)?;
      room.propose_roster(members, accounts)?;
    }
    Command::ConfirmRoster { id, revision, proposal_id } => {
      let room = room_at_revision(&mut book, id, revision)?;
      room.confirm_roster(proposal_id, accounts)?;
    }
    Command::DiscardRoster { id, revision } => {
      let room = room_at_revision(&mut book, id, revision)?;
      room.discard_roster()?;
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
          if !matches!(room.phase, Phase::Dispatching | Phase::WaitingReply | Phase::Planning) { return Err("该房间当前没有运行的轮次".into()); }
          (id, room.revision, Signal::Stop)
        }
        _ => unreachable!(),
      };
      if matches!(signal, Signal::Prepare) && book.rooms.iter().any(Room::in_progress) {
        return Err("已有一个活跃房间，请先停止该房间".into());
      }
      let room = room_at_revision(&mut book, id, revision)?;
      if matches!(signal, Signal::Confirm) && !room.can_finalize() {
        return Err("没有已回收并核验来源的交付物".into());
      }
      if room.template.flow != Flow::Solo && matches!(signal, Signal::Rework) {
        room.supplement(rework.expect("validated rework note"))?;
        save_json_atomic(path, &book)?;
        return Ok(book);
      }
      if matches!(signal, Signal::Prepare | Signal::Resume) { room.validate(accounts)?; }
      if room.template.flow != Flow::Solo && matches!(signal, Signal::Resume) { room.resume_relay()?; }
      else { room.transition(signal)?; }
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

fn room_at_revision(book: &mut RoomBook, id: Uuid, revision: u64) -> Result<&mut Room, String> {
  let room = book.rooms.iter_mut().find(|room| room.id == id).ok_or("房间不存在")?;
  if room.revision != revision { return Err("房间状态已变化，请重新打开".into()); }
  Ok(room)
}

#[cfg(any(target_os = "ios", test))]
pub struct Dispatch {
  pub room_id: Uuid,
  pub run_id: Uuid,
  pub attempt: RoomAttempt,
  pub prompt: String,
}

#[cfg(any(target_os = "ios", test))]
pub fn begin_dispatch(
  path: &Path, id: Uuid, revision: u64, accounts: &[AccountRecord], recover: bool,
) -> Result<Option<Dispatch>, String> {
  let mut book = read(path, false)?;
  if book.rooms.iter().any(|room| room.id != id && room.in_progress()) {
    return Err("已有其他活跃房间".into());
  }
  let room = room_at_revision(&mut book, id, revision)?;
  let run_id = room.run_id.ok_or("请先校验并准备房间")?;
  let mut prompt = String::new();
  let attempt = if recover {
    let mut attempt = room.attempts.last().filter(|attempt| attempt.session_id.is_some() && attempt.user_message_id.is_some())
      .cloned().ok_or("没有可核对的服务端回执，打开会话人工核对或结束该房间")?;
    if !authenticated(accounts, &attempt.account_uuid) { return Err("请先恢复本轮原账号登录".into()); }
    room.transition(Signal::Recover)?;
    attempt.worker_id = Some(Uuid::new_v4());
    *room.attempts.last_mut().ok_or("本轮发送记录不存在")? = attempt.clone();
    attempt
  } else {
    if room.phase != Phase::Planning { return Err("当前房间不能开始派发".into()); }
    let (member_uuid, stage) = if room.template.flow == Flow::Solo {
      (room.draft.members[0].uuid.clone(), None)
    } else {
      let Some((stage, uuid)) = room.next_step() else { return Err("房间接力步骤不存在".into()); };
      (uuid.to_owned(), Some(stage))
    };
    if !authenticated(accounts, &member_uuid) {
      if stage.is_none() { return Err("请先登录该房间的 Muse 账号".into()); }
      room.hold(Hold::AccountUnavailable);
      save_json_atomic(path, &book)?;
      return Ok(None);
    }
    let attempt_id = Uuid::new_v4();
    if let Some(stage) = stage {
      let Some(text) = room.relay_prompt(stage, &member_uuid, attempt_id) else {
        room.hold(Hold::ContextTooLarge);
        save_json_atomic(path, &book)?;
        return Ok(None);
      };
      prompt = text;
    } else {
      prompt = room.solo_prompt();
    }
    room.transition(Signal::PlanReady)?;
    if room.phase != Phase::Dispatching {
      room.relay.hold = Some(Hold::RoundLimit);
      save_json_atomic(path, &book)?;
      return Ok(None);
    }
    let attempt = RoomAttempt {
      id: attempt_id, worker_id: Some(attempt_id), account_uuid: member_uuid, goal_revision: room.goal_revision,
      session_id: None, send_id: None, user_message_id: None, status: "prepared".into(), delivery: None,
      stage, review: None, remote_cancel_acknowledged: None,
    };
    room.attempts.push(attempt.clone());
    attempt
  };
  save_json_atomic(path, &book)?;
  Ok(Some(Dispatch { room_id: id, run_id, attempt, prompt }))
}

#[cfg(any(target_os = "ios", test))]
fn authenticated(accounts: &[AccountRecord], uuid: &str) -> bool {
  accounts.iter().any(|account| account.uuid == uuid && account.provider_id == ProviderId::Muse
    && account.auth_state == crate::storage::AuthState::Authenticated)
}

#[cfg(any(target_os = "ios", test))]
impl Room {
  fn solo_prompt(&self) -> String {
    let mut prompt = format!(
    "这是用户的单账号协作房间任务。按下面的目标完成工作，并在回复中提交可供用户检查的文字交付。明确区分已完成的内容、未验证的外部操作和待交付项。\n\n任务目标及资料：\n{}",
    self.draft.goal
  );
  if !self.rework_note.is_empty() {
    prompt.push_str("\n\n用户返工要求：\n");
    prompt.push_str(&self.rework_note);
    if let Some(prior) = self.attempts.iter().rev().find_map(|attempt| attempt.delivery.as_ref()) {
      prompt.push_str("\n\n上次交付（参考）：\n");
      prompt.extend(prior.text.chars().take(5_000));
    }
  }
    prompt
  }
}

#[cfg(any(target_os = "ios", test))]
pub enum Milestone {
  Ready { session: String, send_id: String },
  Acknowledged { user_message_id: String },
  Complete { text: String, source: &'static str },
  #[cfg(target_os = "ios")]
  RemoteCancel { acknowledged: bool },
  FailedBeforeSending,
  Uncertain,
}

#[cfg(any(target_os = "ios", test))]
pub fn record(path: &Path, dispatch: &Dispatch, event: Milestone) -> Result<RoomBook, String> {
  let mut book = read(path, false)?;
  let room = book.rooms.iter_mut().find(|room| room.id == dispatch.room_id).ok_or("房间不存在")?;
  if room.run_id != Some(dispatch.run_id) { return Err("该回复属于旧任务".into()); }
  let phase = room.phase;
  let attempt = room.attempts.last_mut()
    .filter(|attempt| attempt.id == dispatch.attempt.id && attempt.goal_revision == room.goal_revision
      && attempt.account_uuid == dispatch.attempt.account_uuid && attempt.worker_id == dispatch.attempt.worker_id)
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
      if attempt.stage.is_some() {
        room.complete_relay()?;
      } else {
        room.transition(Signal::Reply)?;
        room.transition(Signal::Assembled)?;
      }
    }
    Milestone::FailedBeforeSending if phase == Phase::Dispatching => {
      attempt.status = "not_sent".into();
      if attempt.stage.is_some() { room.relay_send_failed(); }
      else { room.transition(Signal::Failed)?; }
    }
    Milestone::Uncertain if matches!(phase, Phase::Dispatching | Phase::WaitingReply) => {
      if attempt.user_message_id.is_none() { attempt.status = "unknown".into(); }
      room.transition(Signal::Unknown)?;
    }
    #[cfg(target_os = "ios")]
    Milestone::RemoteCancel { acknowledged } if phase == Phase::NeedsReconciliation => {
      attempt.remote_cancel_acknowledged = Some(acknowledged);
      room.revision += 1;
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
      relay: Relay::default(),
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
    let dispatch = begin_dispatch(&path, id, ready.rooms[0].revision, &accounts, false).unwrap().unwrap();
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
    let recovery = begin_dispatch(&path, id, before_recovery.rooms[0].revision, &accounts, true).unwrap().unwrap();
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

  struct RelayFixture {
    dir: std::path::PathBuf,
    path: std::path::PathBuf,
    accounts: Vec<AccountRecord>,
    id: Uuid,
  }

  impl RelayFixture {
    fn new(flow: Flow, limit: u8, roles: &[(&str, &[Role])]) -> Self {
      let dir = std::env::temp_dir().join(format!("maam-relay-{}", Uuid::new_v4()));
      let path = dir.join("rooms-v1.json");
      let accounts: Vec<AccountRecord> = ["a", "b", "c", "d"].map(|id| serde_json::from_value(serde_json::json!({
        "providerId":"muse", "uuid":id, "alias":id, "createdAtMs":0, "authState":"authenticated",
      })).unwrap()).into();
      let mut draft = room(flow, limit).draft;
      draft.members = roles.iter().map(|(uuid, roles)| Member { uuid:(*uuid).into(), roles:roles.to_vec() }).collect();
      let created = apply(&path, Command::Create { draft }, &accounts, false).unwrap();
      let id = created.rooms[0].id;
      apply(&path, Command::Prepare { id, revision:0 }, &accounts, false).unwrap();
      Self { dir, path, accounts, id }
    }

    fn get(&self) -> Room {
      read(&self.path, false).unwrap().rooms.remove(0)
    }

    fn command(&self, command: Command) -> Result<RoomBook, String> {
      apply(&self.path, command, &self.accounts, false)
    }

    fn send(&self, stage: Stage, account: &str) -> Dispatch {
      let revision = self.get().revision;
      let dispatch = begin_dispatch(&self.path, self.id, revision, &self.accounts, false).unwrap().unwrap();
      assert_eq!(dispatch.attempt.stage, Some(stage));
      assert_eq!(dispatch.attempt.account_uuid, account);
      assert!(begin_dispatch(&self.path, self.id, revision, &self.accounts, false).is_err());
      record(&self.path, &dispatch, Milestone::Ready { session:Uuid::new_v4().to_string(), send_id:dispatch.attempt.id.to_string() }).unwrap();
      record(&self.path, &dispatch, Milestone::Acknowledged { user_message_id:Uuid::new_v4().to_string() }).unwrap();
      dispatch
    }

    fn reply(&self, dispatch: &Dispatch, text: &str) {
      record(&self.path, dispatch, Milestone::Complete { text:text.into(), source:"stream" }).unwrap();
    }

    fn audit(&self, dispatch: &Dispatch, decision: &str) {
      self.reply(dispatch, &serde_json::json!({
        "reviewId":dispatch.attempt.id, "decision":decision, "reason":"具体的审核依据与修改要求",
      }).to_string());
    }
  }

  impl Drop for RelayFixture {
    fn drop(&mut self) {
      let _ = std::fs::remove_file(&self.path);
      let _ = std::fs::remove_dir(&self.dir);
    }
  }

  const REVIEW_ROLES: &[(&str, &[Role])] = &[
    ("a", &[Role::Executor]),
    ("b", &[Role::Commander, Role::Reviewer, Role::Publisher]),
  ];

  #[test]
  fn build_routes_two_executors_to_independent_review_then_publisher_with_audit_rework() {
    let fixture = RelayFixture::new(Flow::Build, 9, &[
      ("a", &[Role::Commander, Role::Reviewer]),
      ("b", &[Role::Executor]), ("c", &[Role::Executor, Role::Publisher]),
    ]);
    let plan = fixture.send(Stage::Plan, "a");
    fixture.reply(&plan, "PLAN SOURCE");
    for version in ["old", "revised"] {
      let one = fixture.send(Stage::Work, "b");
      assert!(one.prompt.contains("PLAN SOURCE"));
      if version == "revised" { assert!(one.prompt.contains("具体的审核依据")); }
      fixture.reply(&one, &format!("WORK B {version}"));
      let two = fixture.send(Stage::Work, "c");
      fixture.reply(&two, &format!("WORK C {version}"));
      let reviewer = fixture.send(Stage::Review, "a");
      assert!(reviewer.prompt.contains(&format!("WORK B {version}")));
      assert!(reviewer.prompt.contains(&format!("WORK C {version}")));
      if version == "revised" { assert!(!reviewer.prompt.contains("WORK B old")); }
      assert!(fixture.command(Command::Confirm { id:fixture.id, revision:fixture.get().revision }).is_err());
      fixture.audit(&reviewer, if version == "old" { "return" } else { "approve" });
    }
    let publish = fixture.send(Stage::Publish, "c");
    assert!(publish.prompt.contains("WORK B revised"));
    assert!(publish.prompt.contains("WORK C revised"));
    assert!(!publish.prompt.contains("WORK B old"));
    fixture.reply(&publish, "TEXT DELIVERY");
    let pending = fixture.get();
    assert_eq!((pending.phase, pending.rounds, pending.returned), (Phase::PendingConfirmation, 8, 1));
    fixture.command(Command::Confirm { id:fixture.id, revision:pending.revision }).unwrap();
    assert_eq!(fixture.get().phase, Phase::Finalized);
  }

  #[test]
  fn budget_and_third_return_stop_dispatch_and_never_accept_unreviewed_output() {
    for (limit, expected, returns) in [(2, Hold::RoundLimit, 1), (8, Hold::ReviewLimit, 3)] {
      let fixture = RelayFixture::new(Flow::Review, limit, REVIEW_ROLES);
      for _ in 0..returns {
        let draft = fixture.send(Stage::Work, "a");
        fixture.reply(&draft, "DRAFT");
        let review = fixture.send(Stage::Review, "b");
        fixture.audit(&review, "return");
      }
      let held = fixture.get();
      assert_eq!(held.phase, Phase::NeedsUser);
      assert_eq!(held.relay.hold, Some(expected));
      assert_eq!(held.returned, returns);
      assert!(begin_dispatch(&fixture.path, fixture.id, held.revision, &fixture.accounts, false).is_err());
      assert!(fixture.command(Command::Confirm { id:fixture.id, revision:held.revision }).is_err());
    }
  }

  #[test]
  fn wrong_review_identifier_never_unlocks_publishing() {
    let fixture = RelayFixture::new(Flow::Review, 8, REVIEW_ROLES);
    let draft = fixture.send(Stage::Work, "a");
    fixture.reply(&draft, "DRAFT");
    let review = fixture.send(Stage::Review, "b");
    fixture.reply(&review, &serde_json::json!({
      "reviewId":Uuid::new_v4(), "decision":"approve", "reason":"从其他轮次来的结论",
    }).to_string());
    let held = fixture.get();
    assert_eq!(held.relay.hold, Some(Hold::InvalidReview));
    assert_eq!((held.rounds, held.returned), (2, 0));
  }

  #[test]
  fn lost_account_or_pre_send_failure_never_reassigns_a_fixed_role() {
    let fixture = RelayFixture::new(Flow::Review, 8, REVIEW_ROLES);
    assert!(begin_dispatch(&fixture.path, fixture.id, fixture.get().revision, &fixture.accounts[1..], false).unwrap().is_none());
    let blocked = fixture.get();
    assert_eq!(blocked.relay.hold, Some(Hold::AccountUnavailable));
    assert!(blocked.attempts.is_empty());
    let id = fixture.id;
    fixture.command(Command::Resume { id, revision:blocked.revision }).unwrap();
    let attempt = begin_dispatch(&fixture.path, id, fixture.get().revision, &fixture.accounts, false).unwrap().unwrap();
    assert_eq!(attempt.attempt.account_uuid, "a");
    record(&fixture.path, &attempt, Milestone::FailedBeforeSending).unwrap();
    let failed = fixture.get();
    assert_eq!(failed.relay.hold, Some(Hold::AccountUnavailable));
    assert_eq!((failed.rounds, failed.attempts.len()), (0, 1));
    assert!(begin_dispatch(&fixture.path, id, failed.revision, &fixture.accounts, false).is_err());
  }

  #[test]
  fn brainstorming_replies_relay_in_order_and_compare_answers_are_independent() {
    let fixture = RelayFixture::new(Flow::Brainstorm, 8, &[
      ("a", &[Role::Commander, Role::Discussant]),
      ("b", &[Role::Discussant]),
      ("c", &[Role::Publisher, Role::Discussant]),
    ]);
    for (uuid, preceding) in [("a", ""), ("b", "VIEW a"), ("c", "VIEW b")] {
      let discuss = fixture.send(Stage::Discuss, uuid);
      assert!(discuss.prompt.contains(preceding));
      fixture.reply(&discuss, &format!("VIEW {uuid}"));
    }
    let synthesis = fixture.send(Stage::Summarize, "a");
    assert!(synthesis.prompt.contains("VIEW c"));
    fixture.reply(&synthesis, "SYNTHESIS");
    let publish = fixture.send(Stage::Publish, "c");
    assert!(publish.prompt.contains("SYNTHESIS"));

    let fixture = RelayFixture::new(Flow::Compare, 3, &[
      ("a", &[Role::Commander, Role::Executor]),
      ("b", &[Role::Executor, Role::Publisher]),
    ]);
    let first = fixture.send(Stage::Work, "a");
    fixture.reply(&first, "FIRST INDEPENDENT RESPONSE");
    let second = fixture.send(Stage::Work, "b");
    assert!(!second.prompt.contains("FIRST INDEPENDENT RESPONSE"));
    fixture.reply(&second, "SECOND INDEPENDENT RESPONSE");
    let compare = fixture.send(Stage::Publish, "b");
    assert!(compare.prompt.contains("FIRST INDEPENDENT RESPONSE"));
    assert!(compare.prompt.contains("SECOND INDEPENDENT RESPONSE"));
    fixture.reply(&compare, "Comparison");
    let held = fixture.get();
    assert_eq!(held.phase, Phase::NeedsUser);
    assert_eq!(held.relay.hold, Some(Hold::RoundLimit));
    assert!(fixture.command(Command::Confirm { id:fixture.id, revision:held.revision }).is_ok());
  }

  #[test]
  fn queued_user_input_pauses_at_boundary_and_manual_roster_needs_exact_confirmation() {
    let fixture = RelayFixture::new(Flow::Review, 8, REVIEW_ROLES);
    let draft = fixture.send(Stage::Work, "a");
    fixture.command(Command::Supplement { id:fixture.id, revision:fixture.get().revision, note:"改变要求".into() }).unwrap();
    fixture.reply(&draft, "original result");
    let paused = fixture.get();
    assert_eq!((paused.phase, paused.goal_revision, paused.rounds), (Phase::Paused, 1, 1));
    let mut members = paused.draft.members;
    members[0].uuid = "c".into();
    let id = fixture.id;
    fixture.command(Command::ProposeRoster { id, revision:paused.revision, members }).unwrap();
    let proposed = fixture.get();
    assert_eq!(proposed.draft.members[0].uuid, "a");
    assert!(fixture.command(Command::Resume { id, revision:proposed.revision }).is_err());
    assert!(fixture.command(Command::ConfirmRoster { id, revision:proposed.revision, proposal_id:Uuid::new_v4() }).is_err());
    let proposal_id = proposed.roster_proposal_id().unwrap();
    fixture.command(Command::ConfirmRoster { id, revision:proposed.revision, proposal_id }).unwrap();
    let changed = fixture.get();
    assert_eq!(changed.draft.members[0].uuid, "c");
    fixture.command(Command::Resume { id, revision:changed.revision }).unwrap();
    let next = fixture.send(Stage::Work, "c");
    assert!(next.prompt.contains("改变要求"));
    assert!(record(&fixture.path, &draft, Milestone::Complete { text:"late".into(), source:"stream" }).is_err());
    fixture.command(Command::Interrupt { id }).unwrap();
    assert!(record(&fixture.path, &next, Milestone::Complete { text:"after stop".into(), source:"stream" }).is_err());
    let halted = fixture.get();
    let recovered = begin_dispatch(&fixture.path, id, halted.revision, &fixture.accounts, true).unwrap().unwrap();
    assert_eq!(recovered.attempt.id, next.attempt.id);
    assert_ne!(recovered.attempt.worker_id, next.attempt.worker_id);
    assert!(record(&fixture.path, &next, Milestone::Uncertain).is_err());
    assert!(recovered.prompt.is_empty());
  }
}
