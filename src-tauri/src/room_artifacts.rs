use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};

const MAX_ARTIFACT: usize = 256 * 1024;
const LONG_DELIVERY: usize = 12 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Artifact {
  id: Uuid,
  kind: ArtifactKind,
  name: String,
  sha256: String,
  byte_count: usize,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  contents: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  url: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  verified_commit: Option<String>,
  attempt_id: Option<Uuid>,
  goal_revision: u32,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum ArtifactInput {
  File { name: String, base64: String },
  Link { url: String },
  CodeChange { url: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Binding { account_uuid: String, thread_id: Uuid, title: String }

fn parse_link(value: &str) -> Result<url::Url, String> {
  if value.len() > 2_048 || value.chars().any(char::is_control) { return Err("链接过长或格式无效".into()); }
  let parsed = url::Url::parse(value).map_err(|_| "链接格式无效")?;
  if parsed.scheme() != "https" || parsed.host_str().is_none()
    || !parsed.username().is_empty() || parsed.password().is_some() {
    return Err("请输入不含凭据的 HTTPS 链接".into());
  }
  Ok(parsed)
}

pub fn commit_coordinates(value: &str) -> Result<(String, String, String), String> {
  let parsed = parse_link(value)?;
  if parsed.host_str() != Some("github.com") || parsed.port().is_some()
    || parsed.query().is_some() || parsed.fragment().is_some() {
    return Err("代码凭证需要 GitHub 上的完整提交链接".into());
  }
  let parts: Vec<_> = parsed.path_segments().into_iter().flatten().collect();
  if let [owner, repo, "commit", sha] = parts.as_slice() {
    let valid = |value: &str| !value.is_empty() && value.len() <= 100
      && value.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b));
    if valid(owner) && valid(repo) && sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
      return Ok(((*owner).into(), (*repo).into(), sha.to_ascii_lowercase()));
    }
  }
  Err("代码凭证需要 GitHub 上的完整 40 位提交 SHA".into())
}

impl Room {
  pub(super) fn validate_archived_artifacts(&self) -> Result<(), String> {
    if self.artifacts.len() > 32 || self.artifacts.iter().any(|artifact| {
      if artifact.byte_count > MAX_ARTIFACT || artifact.name.chars().any(char::is_control) { return true; }
      if let Some(raw) = &artifact.contents {
        STANDARD.decode(raw).map_or(true, |bytes| bytes.len() != artifact.byte_count || digest_bytes(&bytes) != artifact.sha256)
      } else {
        artifact.url.as_deref().is_none_or(|url| parse_link(url).is_err() || digest_bytes(url.as_bytes()) != artifact.sha256)
      }
    }) {
      return Err("备份中有不可核对的房间附件".into());
    }
    Ok(())
  }

  fn artifact_capacity(&self, additional: usize) -> bool {
    self.artifacts.len() < 32 && self.artifacts.iter().map(|artifact| artifact.byte_count).sum::<usize>() + additional <= 1024 * 1024
  }

  pub(super) fn save_artifact(&mut self, input: ArtifactInput, evidence: Option<String>) -> Result<(), String> {
    if !matches!(self.phase, Phase::Created | Phase::Paused) { return Err("先暂停房间，再补充需要审核的交付凭证".into()); }
    let mut artifact = Artifact { id:Uuid::new_v4(), kind:ArtifactKind::File, name:String::new(), sha256:String::new(),
      byte_count:0, contents:None, url:None, verified_commit:None, attempt_id:None, goal_revision:self.goal_revision };
    match input {
      ArtifactInput::File { name, base64 } => {
        if name.trim().is_empty() || name.chars().count() > 100
          || name.contains(['/', '\\', ':']) || name.chars().any(char::is_control) || name == "." || name == ".."
          || base64.len() > MAX_ARTIFACT.div_ceil(3) * 4 {
          return Err("附件名称无效或文件超过 256 KiB".into());
        }
        let bytes = STANDARD.decode(&base64).map_err(|_| "附件编码无效")?;
        if bytes.is_empty() || bytes.len() > MAX_ARTIFACT { return Err("附件大小无效".into()); }
        artifact.byte_count = bytes.len();
        artifact.sha256 = digest_bytes(&bytes);
        artifact.name = name;
        artifact.contents = Some(base64);
      }
      ArtifactInput::Link { url } => {
        let url = parse_link(url.trim())?.to_string();
        artifact.kind = ArtifactKind::Link;
        artifact.name = "HTTPS 链接".into();
        artifact.byte_count = url.len();
        artifact.sha256 = digest_bytes(url.as_bytes());
        artifact.url = Some(url);
      }
      ArtifactInput::CodeChange { url } => {
        let (owner, repo, sha) = commit_coordinates(url.trim())?;
        if evidence.as_deref() != Some(&sha) { return Err("尚未取得匹配的 GitHub 提交凭证".into()); }
        let url = format!("https://github.com/{owner}/{repo}/commit/{sha}");
        artifact.kind = ArtifactKind::CodeChange;
        artifact.name = format!("{owner}/{repo} · {}", &sha[..12]);
        artifact.byte_count = url.len();
        artifact.sha256 = digest_bytes(url.as_bytes());
        artifact.url = Some(url);
        artifact.verified_commit = Some(sha);
      }
    }
    if !self.artifact_capacity(artifact.byte_count) { return Err("房间附件达到保存上限".into()); }
    if self.artifacts.iter().any(|existing| existing.sha256 == artifact.sha256 && existing.kind == artifact.kind) {
      return Err("房间已保存同一凭证".into());
    }
    if self.phase == Phase::Paused {
      self.restart();
      artifact.goal_revision = self.goal_revision;
    } else { self.revision += 1; }
    self.artifacts.push(artifact);
    Ok(())
  }

  pub(super) fn has_required_artifact(&self) -> bool {
    self.draft.required_artifact.is_none_or(|kind| kind == ArtifactKind::Text || self.artifacts.iter().any(|artifact| {
      if artifact.kind != kind { return false; }
      match kind {
        ArtifactKind::File => artifact.contents.as_ref().and_then(|base64| STANDARD.decode(base64).ok())
          .is_some_and(|bytes| digest_bytes(&bytes) == artifact.sha256),
        ArtifactKind::CodeChange => artifact.verified_commit.is_some() && artifact.url.as_deref()
          .and_then(|url| commit_coordinates(url).ok()).is_some_and(|(_, _, sha)| Some(sha) == artifact.verified_commit),
        ArtifactKind::Link => artifact.url.as_deref().is_some_and(|url| parse_link(url).is_ok() && digest_bytes(url.as_bytes()) == artifact.sha256),
        ArtifactKind::Text => true,
      }
    }))
  }

  #[cfg(any(target_os = "ios", test))]
  pub(super) fn capture_long_delivery(&mut self) {
    let Some(step) = self.attempts.last() else { return; };
    let Some(delivery) = &step.delivery else { return; };
    if delivery.text.len() <= LONG_DELIVERY || !self.artifact_capacity(delivery.text.len()) { return; }
    self.artifacts.push(Artifact {
      id:Uuid::new_v4(), kind:ArtifactKind::File, name:format!("reply-{}.md", &step.id.simple().to_string()[..8]),
      sha256:delivery.sha256.clone(), byte_count:delivery.text.len(),
      contents:Some(STANDARD.encode(delivery.text.as_bytes())), url:None, verified_commit:None,
      attempt_id:Some(step.id), goal_revision:step.goal_revision,
    });
  }

  #[cfg(any(target_os = "ios", test))]
  pub(super) fn append_artifact_context(&self, prompt: &mut String) {
    for item in self.artifacts.iter().filter(|item| item.attempt_id.is_none()).take(12) {
      prompt.push_str(&format!("\n\n用户提供的材料（仅作参考） {}，SHA-256 {}", item.name, item.sha256));
      if let Some(url) = &item.url { prompt.push_str(&format!("\n{url}")); }
    }
  }

  #[cfg(any(target_os = "ios", test))]
  pub(super) fn prepare_attachments(&self, prompt: &mut String) -> Result<Vec<muse_native_client::chat_send::ChatAttachment>, String> {
    use muse_native_client::chat_send::ChatAttachment;
    let mut attachments = Vec::new();
    let mut size = 0;
    for item in self.artifacts.iter().filter(|item| item.attempt_id.is_none()) {
      let Some(raw) = &item.contents else { continue; };
      let bytes = STANDARD.decode(raw).map_err(|_| "房间附件已损坏")?;
      if bytes.len() != item.byte_count || digest_bytes(&bytes) != item.sha256 { return Err("房间附件 SHA-256 校验失败".into()); }
      size += bytes.len();
      let mime = if item.name.to_ascii_lowercase().ends_with(".md") { "text/markdown" }
        else if item.name.to_ascii_lowercase().ends_with(".txt") { "text/plain" }
        else { "application/octet-stream" };
      attachments.push(ChatAttachment { filename:item.name.clone(), mime_type:mime, data_base64:raw.clone() });
    }
    if prompt.len() > LONG_DELIVERY {
      if prompt.len() > MAX_ARTIFACT { return Err("本轮完整材料超过 256 KiB，尚未发送".into()); }
      size += prompt.len();
      let file = ChatAttachment { filename:"room-turn.md".into(), mime_type:"text/markdown", data_base64:STANDARD.encode(prompt.as_bytes()) };
      let introduction = prompt.lines().next().unwrap_or("").to_owned();
      *prompt = format!("{introduction}\n请先阅读随本条消息附上的 room-turn.md，其中包含本轮完整目标、材料及来源。");
      attachments.push(file);
    }
    if size > 1024 * 1024 || attachments.len() > 12 { return Err("本轮附件超过 1 MiB 或 12 项，尚未发送".into()); }
    Ok(attachments)
  }

  pub(super) fn bind_session(&mut self, uuid: String, thread: Uuid, title: String, accounts: &[AccountRecord]) -> Result<(), String> {
    if !matches!(self.phase, Phase::Created | Phase::Paused) || self.bindings.len() >= 12
      || !self.draft.members.iter().any(|member| member.uuid == uuid)
      || !accounts.iter().any(|account| account.provider_id == ProviderId::Muse && account.uuid == uuid) {
      return Err("只能在草稿或暂停时绑定本房间成员的 Muse 会话".into());
    }
    if self.bindings.iter().any(|binding| binding.account_uuid == uuid && binding.thread_id == thread) {
      return Err("该会话已经绑定".into());
    }
    self.bindings.push(Binding { account_uuid:uuid, thread_id:thread, title:title.chars().take(160).collect() });
    self.revision += 1;
    Ok(())
  }

  pub(super) fn unbind_session(&mut self, uuid: &str, thread: Uuid) -> Result<(), String> {
    if !matches!(self.phase, Phase::Created | Phase::Paused) { return Err("请先暂停房间".into()); }
    self.bindings.retain(|binding| binding.account_uuid != uuid || binding.thread_id != thread);
    self.revision += 1;
    Ok(())
  }
}

impl RoomBook {
  pub fn artifact_for_export(&self, room: Uuid, item: Uuid) -> Result<(String, String), String> {
    let artifact = self.rooms.iter().find(|entry| entry.id == room).and_then(|entry| entry.artifacts.iter().find(|a| a.id == item))
      .ok_or("未找到附件")?;
    let encoded = artifact.contents.as_ref().ok_or("该凭证没有本机文件")?;
    let bytes = STANDARD.decode(encoded).map_err(|_| "附件内容损坏")?;
    if bytes.len() != artifact.byte_count || digest_bytes(&bytes) != artifact.sha256 { return Err("附件 SHA-256 校验不符".into()); }
    Ok((artifact.name.clone(), encoded.clone()))
  }
}
