use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct Assignment {
  fixed: Vec<String>,
  quota_window: Option<String>,
  excluded: Vec<String>,
  basis: Vec<AssignmentBasis>,
  changes: Vec<AssignmentChange>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssignmentBasis {
  uuid: String,
  roles: Vec<Role>,
  remaining: Option<u8>,
  model: Option<String>,
  model_source: Option<String>,
  fixed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssignmentChange { from: String, to: String, at_ms: u64 }

struct Candidate<'a> {
  account: &'a AccountRecord,
  window: &'a str,
  remaining: u8,
  model: Option<&'a str>,
  model_source: Option<&'static str>,
}

impl<'a> Candidate<'a> {
  fn from_account(account: &'a AccountRecord, now: u64) -> Option<Self> {
    if account.provider_id != ProviderId::Muse || account.auth_state != crate::storage::AuthState::Authenticated
      || account.proxy.as_ref().is_some_and(|proxy| proxy.enabled && proxy.uri.is_empty()) {
      return None;
    }
    let (window, remaining) = account.quota_observation.as_ref()?.comparable_remaining(now)?;
    if remaining == 0 { return None; }
    let (model, model_source) = if let Some(model) = account.fingerprint_settings.manual_model.as_deref() {
      (Some(model), Some("manual"))
    } else {
      let row = account.fingerprint_history.first().filter(|row| row.valid_samples == 3 && row.verdict == "scored"
        && row.recorded_at_ms <= now && now - row.recorded_at_ms <= 30 * 86_400_000
        && row.probability_ppm.is_some_and(|p| p >= 500_000));
      let model = row.and_then(|row| row.model.as_deref());
      (model, model.map(|_| "fingerprint"))
    };
    Some(Self { account, window, remaining, model, model_source })
  }

  fn basis(&self, member: &Member, fixed: bool) -> AssignmentBasis {
    AssignmentBasis { uuid: member.uuid.clone(), roles: member.roles.clone(), remaining: Some(self.remaining),
      model: self.model.map(str::to_owned), model_source: self.model_source.map(str::to_owned), fixed }
  }
}

fn add_role(members: &mut Vec<Member>, uuid: &str, role: Role) {
  if let Some(member) = members.iter_mut().find(|member| member.uuid == uuid) { member.roles.push(role); }
  else { members.push(Member { uuid: uuid.into(), roles: vec![role] }); }
}

fn compatible(existing: &[Role], role: Role) -> bool {
  !existing.contains(&role) && match role {
    Role::Reviewer => !existing.contains(&Role::Executor),
    Role::Executor => !existing.contains(&Role::Reviewer),
    _ => true,
  }
}

impl Room {
  pub(super) fn assign_initial(&mut self, accounts: &[AccountRecord]) -> Result<(), String> {
    let required = self.template.min_participants;
    let fixed: Vec<_> = self.draft.members.iter().filter(|member| !member.roles.is_empty()).cloned().collect();
    let pool: Vec<_> = self.draft.members.iter().map(|member| member.uuid.clone()).collect();
    let restricted = !self.draft.automatic;
    if self.template.flow == Flow::Solo { return Err("单账号任务请选择手选模式".into()); }
    if pool.iter().collect::<HashSet<_>>().len() != pool.len() {
      return Err("成员必须是不同的本机 Muse 账号".into());
    }
    let now = now_ms();
    let mut groups = BTreeMap::<&str, Vec<Candidate<'_>>>::new();
    for candidate in accounts.iter().filter_map(|account| Candidate::from_account(account, now)) {
      if restricted && !pool.contains(&candidate.account.uuid) { continue; }
      groups.entry(candidate.window).or_default().push(candidate);
    }
    let mut groups: Vec<_> = groups.into_iter().filter(|(_, group)| group.len() >= required
      && fixed.iter().all(|member| group.iter().any(|candidate| candidate.account.uuid == member.uuid))).collect();
    groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));
    for (window, group) in groups {
      let max = match self.template.flow { Flow::Build | Flow::Brainstorm => 5, Flow::Review => 4, _ => 12 };
      let minimum = if restricted { required.max(pool.len()) } else { required.max(fixed.len()) };
      for target in minimum..=max.min(group.len()) {
        let Some(members) = self.pick_members(accounts, &group, &fixed, target) else { continue; };
        if restricted && members.len() != pool.len() { continue; }
        self.assignment.fixed = fixed.iter().map(|member| member.uuid.clone()).collect();
        self.assignment.basis = members.iter().filter_map(|member| group.iter()
          .find(|candidate| candidate.account.uuid == member.uuid)
          .map(|candidate| candidate.basis(member, self.assignment.fixed.contains(&member.uuid)))).collect();
        self.assignment.quota_window = Some(window.to_owned());
        self.draft.members = members;
        return Ok(());
      }
    }
    Err("自动分派需要足够多已登录且在同一额度窗口有新鲜剩余额度的 Muse 账号。可先刷新额度，或手动指定全部角色。".into())
  }

  fn pick_members(&self, accounts: &[AccountRecord], group: &[Candidate<'_>], fixed: &[Member], target: usize) -> Option<Vec<Member>> {
      let mut members = fixed.to_vec();
      let mut seats = match self.template.flow {
        Flow::Build => vec![(Role::Executor, if target >= 4 { 2 } else { 1 }), (Role::Reviewer, 1)],
        Flow::Review => vec![(Role::Executor, 1), (Role::Reviewer, 1)],
        Flow::Brainstorm => vec![(Role::Discussant, target)],
        Flow::Compare => vec![(Role::Executor, target)],
        Flow::Solo => unreachable!(),
      };
      seats.extend([(Role::Commander, 1), (Role::Publisher, 1)]);
      for (role, seats) in seats {
        let assigned = members.iter().filter(|member| member.roles.contains(&role)).count();
        for _ in assigned..seats {
          let eligible = group.iter().filter(|candidate| {
            let present = members.iter().find(|member| member.uuid == candidate.account.uuid);
            present.map_or(members.len() < target, |member|
              !fixed.iter().any(|item| item.uuid == member.uuid) && compatible(&member.roles, role))
          });
          let candidate = eligible.min_by_key(|candidate| {
            let present = members.iter().any(|member| member.uuid == candidate.account.uuid);
            let high = matches!(role, Role::Executor | Role::Discussant | Role::Commander);
            let spread = members.len() < target && !matches!(role, Role::Commander);
            (spread && present, if high { 100 - candidate.remaining } else { candidate.remaining },
              candidate.model.is_none(), candidate.account.uuid.as_str())
          });
          if let Some(candidate) = candidate { add_role(&mut members, &candidate.account.uuid, role); }
        }
      }
      self.validate_members(accounts, &members).ok()?;
      Some(members)
  }

  pub(super) fn lock_confirmed_roster(&mut self) {
    if self.draft.automatic {
      self.assignment.fixed = self.draft.members.iter().map(|member| member.uuid.clone()).collect();
      self.assignment.excluded.clear();
      self.assignment.basis.clear();
      self.assignment.quota_window = None;
    }
  }

  pub(super) fn reconsider_accounts(&mut self) {
    self.assignment.excluded.clear();
  }

  #[cfg(any(target_os = "ios", test))]
  pub(super) fn automatic_send_failed(&mut self) {
    let Some(step) = self.attempts.last() else { return; };
    let failed = step.account_uuid.clone();
    if self.assignment.fixed.contains(&failed) || self.relay.pending_note.is_some() {
      self.relay_send_failed();
    } else {
      self.assignment.excluded.push(failed);
      self.set_phase(Phase::Planning);
    }
  }

  #[cfg(any(target_os = "ios", test))]
  pub(super) fn resolve_automatic_step(&mut self, accounts: &[AccountRecord]) -> Result<bool, String> {
    let (stage, uuid) = self.next_step().ok_or("房间接力步骤不存在")?;
    let uuid = uuid.to_owned();
    let now = now_ms();
    let current = accounts.iter().find(|account| account.uuid == uuid).and_then(|account| Candidate::from_account(account, now));
    if self.assignment.fixed.contains(&uuid) {
      // A fixed role follows the user's explicit route; a failed fixed account never gets substituted.
      return Ok(current.is_some() || (self.assignment.quota_window.is_none() && authenticated(accounts, &uuid)));
    }
    if current.is_some_and(|candidate| self.assignment.quota_window.as_deref() == Some(candidate.window))
      && !self.assignment.excluded.contains(&uuid) { return Ok(true); }
    if !self.assignment.excluded.contains(&uuid) { self.assignment.excluded.push(uuid.clone()); }
    let slot = self.draft.members.iter().position(|member| member.uuid == uuid).ok_or("原成员不存在")?;
    let unavailable = &self.draft.members[slot];
    let mut candidates: Vec<_> = accounts.iter().filter_map(|account| Candidate::from_account(account, now))
      .filter(|candidate| self.assignment.quota_window.as_deref() == Some(candidate.window)
        && !self.assignment.excluded.contains(&candidate.account.uuid)
        && !self.assignment.fixed.contains(&candidate.account.uuid)).collect();
    let high = matches!(stage, Stage::Work | Stage::Discuss | Stage::Plan);
    candidates.sort_by_key(|candidate| {
      let present = self.draft.members.iter().any(|member| member.uuid == candidate.account.uuid);
      (present, if high { 100 - candidate.remaining } else { candidate.remaining },
        candidate.model.is_none(), candidate.account.uuid.as_str())
    });
    for candidate in candidates {
      let mut proposed = self.draft.members.clone();
      let mut replacement = unavailable.clone();
      replacement.uuid = candidate.account.uuid.clone();
      if let Some(other) = proposed.iter().position(|member| member.uuid == replacement.uuid) {
        // Swapping compatible existing slots preserves the ordinal of every planned step.
        proposed[other].uuid = uuid.clone();
        // An unavailable account cannot be moved into an unsent slot.
        if self.stage_members().iter().skip(self.relay.next).any(|(_, id)| *id == candidate.account.uuid) {
          continue;
        }
      }
      proposed[slot] = replacement.clone();
      if self.validate_members(accounts, &proposed).is_err() { continue; }
      let roles = &replacement.roles;
      if self.attempts.iter().any(|step| step.goal_revision == self.goal_revision && step.delivery.is_some()
        && step.account_uuid == replacement.uuid
        && ((step.stage == Some(Stage::Work) && roles.contains(&Role::Reviewer))
          || (step.stage == Some(Stage::Review) && roles.contains(&Role::Executor)))) {
        continue;
      }
      self.assignment.basis.retain(|basis| basis.uuid != uuid && basis.uuid != replacement.uuid);
      self.assignment.basis.push(candidate.basis(&replacement, false));
      self.assignment.changes.push(AssignmentChange { from:uuid, to:replacement.uuid.clone(), at_ms:now });
      self.draft.members = proposed;
      return Ok(true);
    }
    Ok(false)
  }
}
