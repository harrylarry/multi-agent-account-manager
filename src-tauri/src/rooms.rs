use crate::providers::ProviderId;
use crate::storage::{now_ms, save_json_atomic, AccountRecord};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use uuid::Uuid;

#[path = "room_relay.rs"]
mod relay;
#[path = "room_assignment.rs"]
mod assignment;
#[path = "room_records.rs"]
mod records;
#[path = "room_conversation.rs"]
mod conversation;
#[path = "room_artifacts.rs"]
mod artifacts;
#[path = "backup_restore.rs"]
pub(crate) mod backup_restore;
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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct RoleSeats {
  executors: Option<usize>,
  discussants: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Template {
  id: String,
  version: u32,
  title: String,
  flow: Flow,
  min_participants: usize,
  #[serde(default)]
  workflow: Vec<Stage>,
  #[serde(default)]
  work_seats: Option<usize>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  role_seats: Option<RoleSeats>,
}

impl Template {
  fn builtin() -> Vec<Self> {
    [("development", "代码开发", Flow::Build, 2), ("brainstorm", "头脑风暴", Flow::Brainstorm, 3),
      ("review", "评审", Flow::Review, 2), ("compare", "群发对比", Flow::Compare, 2),
      ("solo", "单账号任务", Flow::Solo, 1)]
      .into_iter().map(|(id, title, flow, min_participants)| Self {
        id: id.into(), title: title.into(), version: 1, flow, min_participants, workflow: Vec::new(), work_seats:None, role_seats:None,
      }).collect()
  }

  fn stages(&self) -> &[Stage] {
    if self.workflow.is_empty() { relay::default_workflow(self.flow) } else { &self.workflow }
  }

  fn seats(&self, role: Role, size: usize) -> std::ops::RangeInclusive<usize> {
    let configured = self.role_seats.as_ref();
    match role {
      Role::Commander | Role::Publisher => 1..=1,
      Role::Reviewer => { let count = usize::from(self.stages().contains(&Stage::Review)); count..=count }
      Role::Executor if self.flow == Flow::Solo => 1..=1,
      Role::Executor if !self.stages().contains(&Stage::Work) => 0..=0,
      Role::Executor => {
        if let Some(count) = configured.and_then(|seats| seats.executors)
          .or(self.work_seats.filter(|_| self.flow == Flow::Build)) { return count..=count; }
        match self.flow {
          Flow::Build => 1..=2,
          Flow::Compare if !self.stages().contains(&Stage::Review) => size..=size,
          Flow::Compare => { let count = size.saturating_sub(1); count..=count }
          _ => 1..=1,
        }
      }
      Role::Discussant if !self.stages().contains(&Stage::Discuss) => 0..=0,
      Role::Discussant => {
        if let Some(count) = configured.and_then(|seats| seats.discussants)
          .or(self.work_seats.filter(|_| self.flow == Flow::Brainstorm)) { return count..=count; }
        if self.flow == Flow::Brainstorm { 3..=5 } else { 2..=2 }
      }
    }
  }

  fn validate_config(&self, check_feasibility: bool) -> Result<(), String> {
    relay::validate_workflow(self.flow, &self.workflow)?;
    if self.work_seats.is_some_and(|count| match self.flow {
      Flow::Build => !(1..=2).contains(&count),
      Flow::Brainstorm => !(3..=5).contains(&count),
      _ => true,
    }) { return Err("旧版模板的执行或讨论席位无效".into()); }
    if let Some(seats) = &self.role_seats {
      let min_discussion = if self.flow == Flow::Brainstorm { 3 } else { 2 };
      if self.work_seats.is_some()
        || seats.executors.is_some_and(|count| !self.stages().contains(&Stage::Work) || !(1..=12).contains(&count))
        || seats.discussants.is_some_and(|count| !self.stages().contains(&Stage::Discuss) || !(min_discussion..=12).contains(&count))
      { return Err("角色席位必须对应流程阶段并符合人数要求".into()); }
    }
    let floor = match self.flow { Flow::Solo => 1, Flow::Brainstorm => 3, _ => 2 };
    let ceiling = if self.flow == Flow::Solo { 1 } else { 12 };
    if !check_feasibility { return Ok(()); }
    let possible = (self.min_participants..=ceiling).any(|size| {
      let counts = [Role::Executor, Role::Reviewer, Role::Discussant, Role::Commander, Role::Publisher]
        .map(|role| self.seats(role, size));
      let fixed_steps = self.stages().iter().filter(|stage| matches!(stage, Stage::Plan | Stage::Summarize | Stage::Publish)).count();
      counts.iter().all(|range| !range.is_empty() && *range.start() <= size)
        && counts[0].start() + counts[1].start() <= size
        && counts.iter().map(|range| *range.end()).sum::<usize>() >= size
        && counts[..3].iter().map(|range| *range.start()).sum::<usize>() + fixed_steps <= 24
    });
    if !(floor..=ceiling).contains(&self.min_participants) || !possible {
      return Err("模板人数与角色席位不相容，执行者和审核者必须相互独立".into());
    }
    Ok(())
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
  #[serde(default)]
  automatic: bool,
  #[serde(default)]
  required_artifact: Option<ArtifactKind>,
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
  #[serde(default)]
  assignment: assignment::Assignment,
  #[serde(default)]
  records: records::RoomRecords,
  #[serde(default)]
  artifacts: Vec<artifacts::Artifact>,
  #[serde(default)]
  bindings: Vec<artifacts::Binding>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  conversation: Option<conversation::Conversation>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind { Text, Link, File, CodeChange }

pub use artifacts::{commit_coordinates, ArtifactInput};

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
    if let Some(chat) = &self.conversation { return chat.validate_members(&self.draft.members, accounts); }
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
    if self.template.flow == Flow::Solo && members.len() != 1 { return Err("单账号任务只允许一个成员".into()); }
    for role in [Role::Commander, Role::Executor, Role::Reviewer, Role::Publisher, Role::Discussant] {
      let count = members.iter().filter(|member| member.roles.contains(&role)).count();
      if !self.template.seats(role, members.len()).contains(&count) {
        return Err("成员角色与模板席位不符，请核对指挥、执行、审核、上传和讨论人数".into());
      }
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

#[derive(Serialize)]
pub struct RoomBookView {
  rooms: Vec<Room>,
  templates: Vec<Template>,
}

impl From<RoomBook> for RoomBookView {
  fn from(book: RoomBook) -> Self {
    Self { rooms: book.rooms, templates: book.templates }
  }
}

impl RoomBook {
  pub fn archive_checksum(&self) -> Result<String, String> {
    let data = serde_json::to_vec(self).map_err(|_| "房间资料无法核验")?;
    Ok(digest_bytes(&data))
  }
  pub fn archive_counts(&self) -> (usize, usize) { (self.rooms.len(), self.custom_templates.len()) }

  pub fn validate_archive(&self) -> Result<(), String> {
    let unique = self.rooms.iter().map(|room| room.id).collect::<HashSet<_>>().len() == self.rooms.len();
    if self.version != 1 || self.rooms.len() > 100 || self.custom_templates.len() > 32 || !unique {
      return Err("房间归档版本、容量或标识无效".into());
    }
    for room in &self.rooms {
      let max_attempts = if room.conversation.is_some() { conversation::MAX_ATTEMPTS } else { 64 };
      if room.attempts.len() > max_attempts || room.draft.members.len() > 12 || !(1..=24).contains(&room.draft.round_limit) {
        return Err("房间归档含无效轮次或成员".into());
      }
      room.template.validate_config(room.template.role_seats.is_some())?;
      room.validate_archived_artifacts()?;
    }
    let builtin = Template::builtin();
    let mut custom_ids = HashSet::new();
    for template in &self.custom_templates {
      if !custom_ids.insert(&template.id) || builtin.iter().any(|entry| entry.id == template.id)
        || template.title.is_empty() || template.title.chars().count() > 60 || template.version == 0
        || !(1..=12).contains(&template.min_participants) {
        return Err("备份含无效自定义模板".into());
      }
      template.validate_config(template.role_seats.is_some())?;
    }
    Ok(())
  }

  #[cfg(target_os = "ios")]
  pub fn has_active_room(&self) -> bool { self.rooms.iter().any(Room::in_progress) }

  #[cfg(target_os = "ios")]
  pub fn next_relay_revision(&self, id: Uuid) -> Option<u64> {
    let room = self.rooms.iter().find(|room| room.id == id)?;
    (room.is_relay() && room.phase == Phase::Planning).then_some(room.revision)
  }

  #[cfg(target_os = "ios")]
  pub fn after_relay_attempt(&self, id: Uuid, completed: &RoomAttempt) -> Option<u64> {
    self.rooms.iter().find(|room| room.id == id)?.attempts.last()
      .filter(|step| step.id == completed.id && step.worker_id == completed.worker_id
        && (step.delivery.is_some() || step.status == "not_sent"))?;
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

pub(crate) fn prepare_restore(path: &Path, mut archive: RoomBook, overwrite: bool, checksum: &str) -> Result<RoomBook, String> {
  archive.validate_archive()?;
  let mut current = read(path, false)?;
  if current.archive_checksum()? != checksum { return Err("房间资料已变化，请重新预览".into()); }
  if current.rooms.iter().any(Room::in_progress) { return Err("请先结束当前活跃房间再恢复记录".into()); }
  for mut room in archive.rooms.drain(..) {
    if room.in_progress() { room.set_phase(Phase::Stopped); }
    if let Some(index) = current.rooms.iter().position(|item| item.id == room.id) {
      if overwrite { current.rooms[index] = room; }
    } else { current.rooms.push(room); }
  }
  for template in archive.custom_templates.drain(..) {
    if let Some(index) = current.custom_templates.iter().position(|item| item.id == template.id) {
      if overwrite { current.custom_templates[index] = template; }
    } else { current.custom_templates.push(template); }
  }
  current.validate_archive()?;
  Ok(current)
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum Command {
  List,
  Create { draft: RoomDraft, #[serde(default)] references: Vec<Uuid> },
  CreateConversation { name: String, members: Vec<String>, commander: String },
  PostMessage { id: Uuid, revision: u64, text: String },
  NewTopic { id: Uuid, revision: u64, name: String, goal: String, include_final: bool },
  SetReferences { id: Uuid, revision: u64, source_ids: Vec<Uuid> },
  DeleteRecord { id: Uuid, revision: u64 },
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
  SaveTemplate { title: String, base_id: String, min_participants: usize,
    #[serde(default)] edit_id: Option<String>, #[serde(default)] version: Option<u32>,
    #[serde(default)] workflow: Vec<Stage>, #[serde(default)] work_seats: Option<usize>, #[serde(default)] role_seats: Option<RoleSeats> },
  SaveArtifact { id: Uuid, revision: u64, input: artifacts::ArtifactInput },
  BindSession { id: Uuid, revision: u64, account_uuid: String, thread_id: Uuid },
  UnbindSession { id: Uuid, revision: u64, account_uuid: String, thread_id: Uuid },
}

fn read(path: &Path, recover: bool) -> Result<RoomBook, String> {
  backup_restore::recover(path)?;
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
  for room in &mut book.rooms { room.restore_brief(); }
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
  apply_verified(path, command, accounts, recover, None)
}

impl RoomBook {
  fn add_draft(&mut self, mut draft: RoomDraft, template: Template, references: &[Uuid], accounts: &[AccountRecord], conversation: Option<conversation::Conversation>) -> Result<(), String> {
      draft.name = draft.name.trim().into();
      draft.goal = draft.goal.trim().into();
      if draft.name.is_empty() || draft.name.chars().count() > 80 || (draft.goal.is_empty() && conversation.is_none())
        || draft.goal.chars().count() > 8000 || !(1..=24).contains(&draft.round_limit)
        || draft.members.len() > 12 || draft.members.iter().any(|member| member.roles.len() > 5)
        || self.rooms.len() >= 100 {
        return Err("请检查房间名称、目标、成员和 1–24 轮上限".into());
      }
      let mut room = Room {
        id: Uuid::new_v4(), revision: 0, draft, template, phase: Phase::Created,
        rounds: 0, returned: 0, run_id: None, steps: vec![Transition { at_ms: now_ms(), phase: Phase::Created }],
        attempts: Vec::new(), goal_revision: 0, rework_note: String::new(), approved_at_ms: None,
        relay: Relay::default(),
        assignment: assignment::Assignment::default(), records: records::RoomRecords::default(),
        artifacts: Vec::new(), bindings: Vec::new(), conversation,
      };
      room.records.references = self.resolve_references(None, references)?;
      if room.conversation.is_none() && (room.draft.automatic || room.draft.members.iter().any(|member| member.roles.is_empty())) {
        room.assign_initial(accounts)?;
      }
      else { room.validate(accounts)?; }
      self.rooms.insert(0, room);
      Ok(())
  }
}

pub fn apply_verified(path: &Path, command: Command, accounts: &[AccountRecord], recover: bool, evidence: Option<String>) -> Result<RoomBook, String> {
  let mut book = read(path, recover)?;
  match command {
    Command::List => return Ok(book),
    Command::Create { draft, references } => {
      let template = book.templates.iter().find(|item| item.id == draft.template_id).ok_or("模板不存在")?.clone();
      book.add_draft(draft, template, &references, accounts, None)?;
    }
    Command::CreateConversation { name, members, commander } => book.add_conversation(name, members, commander, accounts)?,
    Command::PostMessage { id, revision, text } => {
      if book.rooms.iter().any(|room| room.id != id && room.in_progress()) { return Err("已有其他房间正在运行".into()); }
      room_at_revision(&mut book, id, revision)?.post_message(text, accounts)?;
    }
    Command::NewTopic { id, revision, name, goal, include_final } => {
      let source = room_at_revision(&mut book, id, revision)?;
      if source.conversation.is_some() { return Err("可以直接在该房间发送下一条消息".into()); }
      if !matches!(source.phase, Phase::Finalized | Phase::Stopped | Phase::Failed) {
        return Err("请先结束当前房间再开新话题".into());
      }
      let (mut draft, template) = (source.draft.clone(), source.template.clone());
      if draft.automatic {
        draft.members.retain(|member| source.assignment.fixed.contains(&member.uuid));
      }
      draft.name = name;
      draft.goal = goal;
      let reference = [id];
      book.add_draft(draft, template, if include_final { &reference } else { &[] }, accounts, None)?;
    }
    Command::SetReferences { id, revision, source_ids } => {
      let references = book.resolve_references(Some(id), &source_ids)?;
      room_at_revision(&mut book, id, revision)?.set_references(references)?;
    }
    Command::DeleteRecord { id, revision } => {
      let room = room_at_revision(&mut book, id, revision)?;
      if room.in_progress() { return Err("请先结束活跃房间".into()); }
      book.rooms.retain(|room| room.id != id);
    }
    Command::SaveTemplate { title, base_id, min_participants, edit_id, version, workflow, work_seats, role_seats } => {
      let title = title.trim();
      let base = book.templates.iter().find(|item| item.id == base_id).ok_or("基础流程不存在")?;
      if title.is_empty() || title.chars().count() > 60
        || (edit_id.is_none() && book.custom_templates.len() >= 32)
        || book.templates.iter().any(|item| item.title == title && edit_id.as_deref() != Some(&item.id)) {
        return Err("请检查模板名称、人数要求或重名".into());
      }
      let (id, next_version) = match &edit_id {
        Some(id) => {
          let existing = book.custom_templates.iter().find(|item| &item.id == id && version == Some(item.version))
            .ok_or("模板已改变，重新打开后再编辑")?;
          (id.clone(), existing.version.checked_add(1).ok_or("模板版本已达上限")?)
        }
        None => (Uuid::new_v4().to_string(), 1),
      };
      let template = Template { id, version: next_version, title: title.into(), flow: base.flow, min_participants, workflow, work_seats, role_seats };
      template.validate_config(true)?;
      book.templates.retain(|item| item.id != template.id);
      book.custom_templates.retain(|item| item.id != template.id);
      book.templates.push(template.clone());
      book.custom_templates.push(template);
    }
    Command::SaveArtifact { id, revision, input } => room_at_revision(&mut book, id, revision)?.save_artifact(input, evidence)?,
    Command::BindSession { id, revision, account_uuid, thread_id } => {
      let title = evidence.ok_or("会话归属尚未核验")?;
      room_at_revision(&mut book, id, revision)?.bind_session(account_uuid, thread_id, title, accounts)?;
    }
    Command::UnbindSession { id, revision, account_uuid, thread_id } => {
      room_at_revision(&mut book, id, revision)?.unbind_session(&account_uuid, thread_id)?;
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
      if room.conversation.is_some() && matches!(signal, Signal::Prepare) { return Err("在房间发送一条消息即可开始".into()); }
      if matches!(signal, Signal::Confirm) && (!room.can_finalize() || !room.has_required_artifact()) {
        return Err("没有已回收并核验来源的交付物".into());
      }
      if room.is_relay() && matches!(signal, Signal::Rework) {
        room.supplement(rework.expect("validated rework note"))?;
        save_json_atomic(path, &book)?;
        return Ok(book);
      }
      if matches!(signal, Signal::Prepare | Signal::Resume) && !room.draft.automatic { room.validate(accounts)?; }
      if room.is_relay() && matches!(signal, Signal::Resume) { room.resume_relay()?; }
      else { room.transition(signal)?; }
      if matches!(signal, Signal::Confirm) {
        let approved_at = now_ms();
        room.approved_at_ms = Some(approved_at);
        if let Some(chat) = &mut room.conversation { chat.confirm_current(approved_at, room.relay.final_attempt); }
        room.record_final_decision();
      }
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
  pub attachments: Vec<muse_native_client::chat_send::ChatAttachment>,
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
  let mut attachments = Vec::new();
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
    if room.conversation.is_some() && room.attempts.len() >= conversation::MAX_ATTEMPTS {
      room.hold(Hold::ContextTooLarge);
      save_json_atomic(path, &book)?;
      return Ok(None);
    }
    if room.draft.automatic && !room.resolve_automatic_step(accounts)? {
      room.hold(Hold::AccountUnavailable);
      save_json_atomic(path, &book)?;
      return Ok(None);
    }
    let (member_uuid, stage) = if !room.is_relay() {
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
      room.append_record_context(&mut prompt, true);
      room.append_artifact_context(&mut prompt);
    }
    attachments = room.prepare_attachments(&mut prompt)?;
    room.transition(Signal::PlanReady)?;
    if room.phase != Phase::Dispatching {
      room.relay.hold = Some(Hold::RoundLimit);
      save_json_atomic(path, &book)?;
      return Ok(None);
    }
    let session_id = room.conversation.as_ref().and_then(|chat| chat.session(&member_uuid));
    let attempt = RoomAttempt {
      id: attempt_id, worker_id: Some(attempt_id), account_uuid: member_uuid, goal_revision: room.goal_revision,
      session_id, send_id: None, user_message_id: None, status: "prepared".into(), delivery: None,
      stage, review: None, remote_cancel_acknowledged: None,
    };
    room.attempts.push(attempt.clone());
    attempt
  };
  save_json_atomic(path, &book)?;
  Ok(Some(Dispatch { room_id: id, run_id, attempt, prompt, attachments }))
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
      if let Some(chat) = &mut room.conversation { chat.bind_session(&attempt.account_uuid, &session)?; }
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
      if attempt.stage == Some(Stage::Plan) && room.conversation.is_some() {
        room.complete_commander_reply()?;
      } else if attempt.stage.is_some() {
        room.complete_relay()?;
      } else {
        room.transition(Signal::Reply)?;
        room.transition(Signal::Assembled)?;
      }
      room.record_completed_step();
      room.capture_long_delivery();
    }
    Milestone::FailedBeforeSending if phase == Phase::Dispatching => {
      attempt.status = "not_sent".into();
      if attempt.stage.is_some() {
        if room.draft.automatic { room.automatic_send_failed(); }
        else { room.relay_send_failed(); }
      }
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
pub fn sha256(text: &str) -> String { digest_bytes(text.as_bytes()) }

pub fn digest_bytes(bytes: &[u8]) -> String {
  ring::digest::digest(&ring::digest::SHA256, bytes).as_ref()
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
        round_limit: limit, members: Vec::new(), automatic: false, required_artifact: None,
      }, template, phase: Phase::Created, rounds: 0, returned: 0, run_id: None, steps: Vec::new(),
      attempts: Vec::new(), goal_revision: 0, rework_note: String::new(), approved_at_ms: None,
      relay: Relay::default(),
      assignment: assignment::Assignment::default(), records: records::RoomRecords::default(),
      artifacts: Vec::new(), bindings: Vec::new(), conversation: None,
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
    assert!(apply(&path, Command::Create { draft: draft.clone(), references: Vec::new() }, &accounts, false).is_err());
    assert!(!path.exists());
    draft.members[1].roles.retain(|role| *role != Role::Reviewer);
    apply(&path, Command::Create { draft: draft.clone(), references: Vec::new() }, &accounts, false).unwrap();
    let two = apply(&path, Command::Create { draft, references: Vec::new() }, &accounts, false).unwrap();
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
    let created = apply(&path, Command::Create { draft, references: Vec::new() }, &accounts, false).unwrap();
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
      let created = apply(&path, Command::Create { draft, references: Vec::new() }, &accounts, false).unwrap();
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
      let session = dispatch.attempt.session_id.clone().unwrap_or_else(|| Uuid::new_v4().to_string());
      record(&self.path, &dispatch, Milestone::Ready { session, send_id:dispatch.attempt.id.to_string() }).unwrap();
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
  fn long_handoff_preserves_full_markdown_and_explicit_files_in_one_draft() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let fixture = RelayFixture::new(Flow::Review, 8, REVIEW_ROLES);
    let state = fixture.command(Command::Pause { id:fixture.id, revision:fixture.get().revision }).unwrap();
    fixture.command(Command::SaveArtifact { id:fixture.id, revision:state.rooms[0].revision,
      input:ArtifactInput::File { name:"evidence.txt".into(), base64:STANDARD.encode("supplied bytes") } }).unwrap();
    let paused = fixture.get();
    fixture.command(Command::Resume { id:fixture.id, revision:paused.revision }).unwrap();
    let work = fixture.send(Stage::Work, "a");
    assert_eq!(STANDARD.decode(&work.attachments[0].data_base64).unwrap(), b"supplied bytes");
    let long = "complete output\n".repeat(1024);
    fixture.reply(&work, &long);
    let book = read(&fixture.path, false).unwrap();
    let value = serde_json::to_value(&book.rooms[0]).unwrap();
    let id = Uuid::parse_str(value["artifacts"][1]["id"].as_str().unwrap()).unwrap();
    let (_, download) = book.artifact_for_export(fixture.id, id).unwrap();
    assert_eq!(STANDARD.decode(download).unwrap(), long.as_bytes());
    let audit = fixture.send(Stage::Review, "b");
    let context = audit.attachments.iter().find(|item| item.filename == "room-turn.md").unwrap();
    assert!(String::from_utf8(STANDARD.decode(&context.data_base64).unwrap()).unwrap().contains(&long));
  }

  #[test]
  fn rejects_template_audit_bypass_and_unverified_foreign_binding() {
    assert!(relay::validate_workflow(Flow::Review, &[Stage::Work, Stage::Publish]).is_err());
    assert!(relay::validate_workflow(Flow::Review, &[Stage::Plan, Stage::Work, Stage::Review, Stage::Summarize, Stage::Publish]).is_ok());
    let fixture = RelayFixture::new(Flow::Review, 8, REVIEW_ROLES);
    let paused = fixture.command(Command::Pause { id:fixture.id, revision:fixture.get().revision }).unwrap();
    let revision = paused.rooms[0].revision;
    let make = |account_uuid: &str| Command::BindSession { id:fixture.id, revision, account_uuid:account_uuid.into(), thread_id:Uuid::new_v4() };
    assert!(fixture.command(make("a")).is_err());
    assert!(apply_verified(&fixture.path, make("d"), &fixture.accounts, false, Some("Other".into())).is_err());
    assert!(apply_verified(&fixture.path, make("a"), &fixture.accounts, false, Some("Owned".into())).is_ok());
    let bad_commit = "https://github.com/a/b/commit/123";
    assert!(commit_coordinates(bad_commit).is_err());
  }

  #[test]
  fn conversation_routes_messages_automatically_and_keeps_earlier_confirmed_tasks() {
    let mut fixture = RelayFixture::new(Flow::Review, 8, REVIEW_ROLES);
    fixture.command(Command::Stop { id:fixture.id, revision:fixture.get().revision }).unwrap();
    let created = fixture.command(Command::CreateConversation {
      name:"Persistent room".into(), members:vec!["a".into(), "b".into(), "c".into()], commander:"a".into(),
    }).unwrap();
    fixture.id = created.rooms[0].id;
    assert!(created.rooms[0].draft.goal.is_empty() && created.rooms[0].attempts.is_empty());
    let post = |fixture: &RelayFixture, text: &str| fixture.command(Command::PostMessage {
      id:fixture.id, revision:fixture.get().revision, text:text.into(),
    }).unwrap();
    post(&fixture, "ordinary question");
    let first = fixture.send(Stage::Plan, "a");
    assert!(first.attempt.session_id.is_none());
    fixture.reply(&first, &serde_json::json!({"turnId":first.attempt.id,"reply":"a direct answer","steps":[]}).to_string());
    assert_eq!(fixture.get().phase, Phase::Created);
    let commander_session = fixture.get().attempts[0].session_id.clone().unwrap();
    post(&fixture, "worked task");
    let plan = fixture.send(Stage::Plan, "a");
    assert_eq!(plan.attempt.session_id.as_deref(), Some(commander_session.as_str()));
    assert!(plan.prompt.contains("a direct answer"));
    fixture.reply(&plan, &serde_json::json!({"turnId":plan.attempt.id,"reply":"route the work","steps":[
      {"member":2,"stage":"work","instruction":"Draft it"},{"member":3,"stage":"review","instruction":"Check it"},
    ]}).to_string());
    let work = fixture.send(Stage::Work, "b");
    assert!(work.attempt.session_id.is_none());
    assert!(work.prompt.contains("Draft it") && work.prompt.contains("route the work"));
    fixture.reply(&work,"specific work output");
    let audit = fixture.send(Stage::Review, "c");
    assert!(audit.prompt.contains("specific work output"));
    fixture.audit(&audit, "approve");
    let final_step = fixture.send(Stage::Publish, "a");
    assert_eq!(final_step.attempt.session_id.as_deref(), Some(commander_session.as_str()));
    fixture.reply(&final_step, "verified summary");
    fixture.command(Command::Confirm { id:fixture.id, revision:fixture.get().revision }).unwrap();
    let postponed = post(&fixture, "next question");
    assert_eq!(postponed.rooms[0].rounds, 0);
    assert_eq!(postponed.resolve_references(None, &[fixture.id]).unwrap().len(), 1);
    let step = fixture.send(Stage::Plan, "a");
    fixture.reply(&step, &serde_json::json!({"turnId":step.attempt.id,"reply":"bad plan","steps":[
      {"member":2,"stage":"work","instruction":"Draft it"}, {"member":2,"stage":"review","instruction":"Review yourself"}
    ]}).to_string());
    let stopped = fixture.get();
    assert_eq!(stopped.phase, Phase::NeedsUser);
    assert_eq!(stopped.relay.hold, Some(Hold::InvalidPlan));
    assert!(begin_dispatch(&fixture.path, fixture.id, stopped.revision, &fixture.accounts, false).is_err());
  }

  #[test]
  fn room_ipc_includes_builtin_and_custom_templates_without_changing_archive_format() {
    let fixture = RelayFixture::new(Flow::Review, 8, REVIEW_ROLES);
    let book = fixture.command(Command::SaveTemplate {
      title: "设备回归模板".into(), base_id: "review".into(), min_participants: 2,
      edit_id: None, version: None, workflow: Vec::new(), work_seats: None, role_seats: None,
    }).unwrap();
    let archive = serde_json::to_value(&book).unwrap();
    assert!(archive.get("templates").is_none());
    assert_eq!(archive["customTemplates"][0]["title"], "设备回归模板");
    let ipc = serde_json::to_value(RoomBookView::from(book)).unwrap();
    let templates = ipc["templates"].as_array().unwrap();
    assert!(templates.iter().any(|template| template["id"] == "solo"));
    assert!(templates.iter().any(|template| template["title"] == "设备回归模板"));
    assert_eq!(ipc["rooms"][0]["id"], fixture.id.to_string());
  }

  #[test]
  fn template_role_config_is_validated_versioned_and_used_by_both_assignment_modes() {
    let fixture = RelayFixture::new(Flow::Review, 12, REVIEW_ROLES);
    let workflow = vec![Stage::Plan, Stage::Discuss, Stage::Work, Stage::Review, Stage::Summarize, Stage::Publish];
    let save = |id: Option<String>, version, executors| Command::SaveTemplate {
      title:"Design review".into(), base_id:"review".into(), min_participants:3, edit_id:id, version,
      workflow:workflow.clone(), work_seats:None,
      role_seats:Some(RoleSeats { executors:Some(executors), discussants:Some(2) }),
    };
    assert!(fixture.command(save(None, None, 12)).is_err());
    let book = fixture.command(save(None, None, 2)).unwrap();
    let template = book.custom_templates[0].clone();
    let mut draft = fixture.get().draft;
    draft.template_id = template.id.clone();
    assert!(fixture.command(Command::Create { draft:draft.clone(), references:Vec::new() }).is_err());
    draft.members = vec![
      Member { uuid:"a".into(), roles:vec![Role::Executor, Role::Commander] },
      Member { uuid:"b".into(), roles:vec![Role::Executor, Role::Discussant] },
      Member { uuid:"c".into(), roles:vec![Role::Reviewer, Role::Discussant, Role::Publisher] },
    ];
    let created = fixture.command(Command::Create { draft:draft.clone(), references:Vec::new() }).unwrap();
    let saved_room = created.rooms[0].id;
    assert_eq!(created.rooms[0].stage_members(), vec![
      (Stage::Plan,"a"), (Stage::Discuss,"b"), (Stage::Discuss,"c"), (Stage::Work,"a"),
      (Stage::Work,"b"), (Stage::Review,"c"), (Stage::Summarize,"a"), (Stage::Publish,"c"),
    ]);
    let mut accounts = fixture.accounts.clone();
    for (account, used) in accounts.iter_mut().zip([10, 25, 30, 35]) {
      account.quota_observation = Some(crate::storage::QuotaObservation {
        source:"native".into(), window:Some("daily".into()), percent_used:Some(used), resets_at_ms:None,
        checked_at_ms:now_ms(), stale:false,
      });
    }
    draft.automatic = true;
    draft.members.retain(|member| member.uuid == "a");
    let assigned = apply(&fixture.path, Command::Create { draft, references:Vec::new() }, &accounts, false).unwrap();
    assert_eq!(assigned.rooms[0].assignment.fixed, vec!["a"]);
    assert_eq!(assigned.rooms[0].stage_members().iter().filter(|(stage, _)| *stage == Stage::Work).count(), 2);
    assert!(assigned.rooms[0].validate(&accounts).is_ok());
    assert!(fixture.command(save(Some(template.id.clone()), Some(0), 1)).is_err());
    let book = fixture.command(save(Some(template.id), Some(1), 1)).unwrap();
    assert_eq!(book.custom_templates[0].version, 2);
    assert_eq!(book.rooms.iter().find(|room| room.id == saved_room).unwrap().template.version, 1);
    let archived: RoomBook = serde_json::from_slice(&serde_json::to_vec(&book).unwrap()).unwrap();
    archived.validate_archive().unwrap();
    assert!(relay::validate_workflow(Flow::Brainstorm, &[Stage::Discuss, Stage::Work, Stage::Summarize, Stage::Publish]).is_err());
  }

  #[test]
  fn automatic_assignment_uses_comparable_quota_and_preserves_fixed_roles_on_failure() {
    let fixture = RelayFixture::new(Flow::Review, 8, REVIEW_ROLES);
    let now = now_ms();
    let mut accounts = fixture.accounts.clone();
    for (account, used) in accounts.iter_mut().zip([10, 65, 10, 5]) {
      account.quota_observation = Some(crate::storage::QuotaObservation {
        source:"native".into(), window:Some("daily".into()), percent_used:Some(used), resets_at_ms:None,
        checked_at_ms:now, stale:false,
      });
    }
    accounts[2].fingerprint_settings.manual_model = Some("model-c".into());
    accounts[3].quota_observation.as_mut().unwrap().window = Some("weekly".into());
    let mut room = fixture.get();
    room.draft.automatic = true;
    room.draft.members.clear();
    room.assign_initial(&accounts).unwrap();
    assert_eq!(room.next_step(), Some((Stage::Work, "c")));
    assert!(serde_json::to_string(&room).unwrap().contains("model-c"));
    let mut book = read(&fixture.path, false).unwrap();
    book.rooms = vec![room];
    save_json_atomic(&fixture.path, &book).unwrap();
    let first = begin_dispatch(&fixture.path, fixture.id, fixture.get().revision, &accounts, false).unwrap().unwrap();
    record(&fixture.path, &first, Milestone::FailedBeforeSending).unwrap();
    let replacement = begin_dispatch(&fixture.path, fixture.id, fixture.get().revision, &accounts, false).unwrap().unwrap();
    assert_eq!(replacement.attempt.account_uuid, "a");
    assert_eq!(fixture.get().rounds, 0);
    // A potentially written attempt must keep its original identity and stop dispatch.
    record(&fixture.path, &replacement, Milestone::Uncertain).unwrap();
    assert_eq!(fixture.get().phase, Phase::NeedsReconciliation);
    assert!(begin_dispatch(&fixture.path, fixture.id, fixture.get().revision, &accounts, false).is_err());

    let mut fixed = fixture.get();
    fixed.draft.members = vec![Member { uuid:"c".into(), roles:vec![Role::Executor] }];
    fixed.assign_initial(&accounts).unwrap();
    assert_eq!(fixed.draft.members[0].roles, vec![Role::Executor]);
    let mut stopped = fixed.clone();
    stopped.set_phase(Phase::Stopped);
    let revision = stopped.revision;
    book.rooms = vec![stopped];
    save_json_atomic(&fixture.path, &book).unwrap();
    let next = apply(&fixture.path, Command::NewTopic { id:fixture.id, revision,
      name:"New activity".into(), goal:"New goal".into(), include_final:false }, &accounts, false).unwrap();
    assert_eq!(next.rooms[0].assignment.fixed, vec!["c"]);
    assert!(next.rooms[0].draft.members.len() > 1);
    accounts[2].quota_observation.as_mut().unwrap().stale = true;
    assert!(!fixed.resolve_automatic_step(&accounts).unwrap());
    assert_eq!(fixed.next_step(), Some((Stage::Work, "c")));
  }

  #[test]
  fn only_finalized_explicit_references_enter_context_and_can_be_removed() {
    let fixture = RelayFixture::new(Flow::Review, 8, REVIEW_ROLES);
    assert!(read(&fixture.path, false).unwrap().resolve_references(None, &[fixture.id]).is_err());
    assert!(fixture.command(Command::NewTopic { id:fixture.id, revision:fixture.get().revision,
      name:"Next topic".into(), goal:"New goal".into(), include_final:false }).is_err());
    let work = fixture.send(Stage::Work, "a");
    fixture.reply(&work, "Draft");
    let review = fixture.send(Stage::Review, "b");
    fixture.audit(&review, "approve");
    let publish = fixture.send(Stage::Publish, "b");
    fixture.reply(&publish, "ACCEPTED PROJECT CONCLUSION");
    fixture.command(Command::Confirm { id:fixture.id, revision:fixture.get().revision }).unwrap();
    let book = read(&fixture.path, false).unwrap();
    assert!(serde_json::to_string(&book).unwrap().contains("用户已确认本次交付定稿"));
    assert!(book.resolve_references(Some(fixture.id), &[fixture.id]).is_err());
    let source = fixture.get();
    let book = fixture.command(Command::NewTopic { id:fixture.id, revision:source.revision,
      name:"Next topic".into(), goal:"New goal".into(), include_final:true }).unwrap();
    let new_room = &book.rooms[0];
    let target = new_room.id;
    assert_ne!(target, source.id);
    assert_eq!(new_room.draft.goal, "New goal");
    assert_eq!(new_room.template.version, source.template.version);
    assert_eq!(new_room.phase, Phase::Created);
    assert!(new_room.bindings.is_empty() && new_room.attempts.is_empty() && new_room.artifacts.is_empty());
    assert!(new_room.run_id.is_none());
    assert_eq!(book.rooms[1].revision, source.revision);
    let state = fixture.command(Command::Prepare { id:target, revision:0 }).unwrap();
    let step = begin_dispatch(&fixture.path, target, state.rooms[0].revision, &fixture.accounts, false).unwrap().unwrap();
    assert!(step.prompt.contains("ACCEPTED PROJECT CONCLUSION"));
    assert!(step.prompt.contains(&sha256("ACCEPTED PROJECT CONCLUSION")));
    assert!(fixture.command(Command::SetReferences { id:target, revision:read(&fixture.path, false).unwrap().rooms[0].revision,
      source_ids:Vec::new() }).is_err());
  }

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
