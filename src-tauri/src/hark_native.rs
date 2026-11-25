//! Hark Phase 0 read-only probe. The sync routes and frame shapes follow
//! BotArk (MIT), src/main/hark/harkClient.ts.

use serde::Serialize;
#[cfg(any(target_os = "ios", test))]
use std::collections::HashMap;

#[cfg(any(target_os = "ios", test))]
#[derive(Clone, Copy, PartialEq, Eq)]
enum JobState {
  Running,
  Completed,
  Failed,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSummary {
  frames: usize,
  malformed_frames: usize,
  known_messages: usize,
  running_now: usize,
  running: usize,
  completed: usize,
  failed: usize,
  duplicate_states: usize,
}

#[cfg(any(target_os = "ios", test))]
#[derive(Default)]
struct SyncTracker {
  states: HashMap<String, Option<JobState>>,
  summary: SyncSummary,
  saw_snapshot: bool,
  completed_ids: Vec<String>,
}

#[cfg(any(target_os = "ios", test))]
impl SyncTracker {
  fn observe(&mut self, id: &str, fields: &serde_json::Value, patch: bool, baseline: bool) {
    let role = fields["role"]
      .as_str()
      .or_else(|| fields["author"].as_str());
    let assistant = role.is_some_and(|role| matches!(role, "assistant" | "bot" | "hark"));
    if id.is_empty() || (!assistant && !(patch && role.is_none() && self.states.contains_key(id))) {
      return;
    }
    let previous = self.states.entry(id.to_string()).or_default();
    let state = match fields["jobStatus"].as_str() {
      Some("completed" | "complete" | "done" | "finished" | "succeeded") => JobState::Completed,
      Some("failed" | "error" | "cancelled" | "canceled" | "interrupted") => JobState::Failed,
      Some("running" | "pending" | "queued" | "active" | "starting") => JobState::Running,
      _ if fields["isStreaming"] == true => JobState::Running,
      _ => return,
    };
    if *previous == Some(state) || matches!(previous, Some(JobState::Completed | JobState::Failed))
    {
      self.summary.duplicate_states += 1;
      return;
    }
    *previous = Some(state);
    if baseline {
      return;
    }
    match state {
      JobState::Running => self.summary.running += 1,
      JobState::Completed => {
        self.summary.completed += 1;
        self.completed_ids.push(id.to_string());
      }
      JobState::Failed => self.summary.failed += 1,
    }
  }

  fn apply_frame(&mut self, raw: &str) {
    use serde_json::Value;
    let Ok(frame) = serde_json::from_str::<Value>(raw) else {
      self.summary.malformed_frames += 1;
      return;
    };
    match frame["type"].as_str() {
      Some("snapshot") if frame["data"].is_object() => {
        self.summary.frames += 1;
        let baseline = !self.saw_snapshot;
        self.saw_snapshot = true;
        let data = &frame["data"];
        if let Some(records) = data["messages"].as_object() {
          for (id, message) in records {
            self.observe(id, message, false, baseline);
          }
        } else if let Some(messages) = data["messages"]
          .as_array()
          .or_else(|| data["items"].as_array())
        {
          for message in messages {
            if let Some(id) = message["id"]
              .as_str()
              .or_else(|| message["messageId"].as_str())
            {
              self.observe(id, message, false, baseline);
            }
          }
        }
      }
      Some("patches") if frame["data"].is_array() => {
        self.summary.frames += 1;
        for patch in frame["data"].as_array().into_iter().flatten() {
          let Some(id) = patch["id"].as_str().or_else(|| patch["messageId"].as_str()) else {
            continue;
          };
          match patch["op"].as_str() {
            Some("entry_add" | "message_add")
              if patch["entry"]
                .as_str()
                .is_none_or(|entry| entry == "message") =>
            {
              self.observe(id, &patch["value"], false, false);
            }
            Some("message_update") => self.observe(id, &patch["fields"], true, false),
            _ => {}
          }
        }
      }
      Some("snapshot" | "patches") => self.summary.malformed_frames += 1,
      _ => {}
    }
  }
}

#[cfg(any(target_os = "ios", test))]
#[derive(Default)]
struct SseDecoder {
  buffer: Vec<u8>,
  data: Vec<u8>,
  skip: bool,
}

#[cfg(any(target_os = "ios", test))]
impl SseDecoder {
  fn feed(&mut self, bytes: &[u8], tracker: &mut SyncTracker) -> Result<(), &'static str> {
    const LIMIT: usize = 2 * 1024 * 1024;
    if self.buffer.len().saturating_add(bytes.len()) > LIMIT {
      return Err("sse_frame_limit");
    }
    self.buffer.extend_from_slice(bytes);
    let mut parsed = 0;
    while let Some(offset) = self.buffer[parsed..].iter().position(|byte| *byte == b'\n') {
      let end = parsed + offset;
      let line = &self.buffer[parsed..end];
      let line = line.strip_suffix(b"\r").unwrap_or(line);
      parsed = end + 1;
      if line.is_empty() {
        if !self.skip && !self.data.is_empty() {
          let frame = std::str::from_utf8(&self.data).map_err(|_| "invalid_sse_utf8")?;
          tracker.apply_frame(frame);
        }
        self.data.clear();
        self.skip = false;
      } else if let Some(kind) = line.strip_prefix(b"event:") {
        self.skip = !matches!(kind.strip_prefix(b" ").unwrap_or(kind), b"" | b"message");
      } else if let Some(chunk) = line.strip_prefix(b"data:") {
        let chunk = chunk.strip_prefix(b" ").unwrap_or(chunk);
        if self.data.len().saturating_add(chunk.len()) >= LIMIT {
          return Err("sse_frame_limit");
        }
        self.data.extend_from_slice(chunk);
        self.data.push(b'\n');
      }
    }
    self.buffer.drain(..parsed);
    Ok(())
  }
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarkProbe {
  rest_status: Option<u16>,
  sse_status: Option<u16>,
  sse_end: Option<String>,
  sse_frames: usize,
  poll_status: Option<u16>,
  poll_frames: usize,
  summary: SyncSummary,
  error: Option<String>,
  #[serde(skip)]
  pub completed_ids: Vec<String>,
}

#[cfg(target_os = "ios")]
impl HarkProbe {
  pub fn task_status(&self) -> Option<&'static str> {
    if !self.rest_status.is_some_and(|status| (200..300).contains(&status))
      || self.summary.malformed_frames > 0 || self.summary.known_messages == 0
      || (self.error.is_some() && self.summary.running_now == 0)
    {
      return None;
    }
    Some(if self.summary.running_now > 0 { "working" } else { "idle" })
  }
}
#[cfg(target_os = "ios")]
pub async fn diagnose(
  app: &tauri::AppHandle,
  uuid: &str,
  setting: Option<&crate::storage::ProxySetting>,
  timezone: &str,
) -> HarkProbe {
  use muse_native_client::cookie::{SessionCookie, SessionCookies};
  use reqwest::header::HeaderValue;
  use std::time::Duration;
  use zeroize::Zeroizing;

  let ready = async {
    let egress = crate::egress::resolve_for_app(app, setting)?;
    let client = egress
      .http_client(Duration::from_secs(18), false)
      .map_err(|e| e.to_string())?;
    let cookies = tokio::time::timeout(
      Duration::from_secs(10),
      tauri_plugin_account_session::get_hark_api_cookies(app, uuid),
    )
    .await
    .map_err(|_| "Hark 登录态读取超时".to_string())?
    .map_err(|_| "无法读取 Hark 登录态".to_string())?;
    let cookies = SessionCookies {
      cookies: cookies
        .into_iter()
        .map(|c| SessionCookie {
          name: c.name,
          value: c.value,
        })
        .collect(),
    };
    if cookies.is_empty() {
      return Err("请在 Spike 中登录非主力 Hark 账号".to_string());
    }
    let cookie = Zeroizing::new(cookies.cookie_header());
    let mut header =
      HeaderValue::from_str(&cookie).map_err(|_| "Hark 登录态格式无效".to_string())?;
    header.set_sensitive(true);
    Ok::<_, String>((client, header))
  }
  .await;
  match ready {
    Ok((client, header)) => run_probe(client, header, timezone, "https://app.hark.com").await,
    Err(e) => HarkProbe {
      error: Some(e),
      ..HarkProbe::default()
    },
  }
}

#[cfg(any(target_os = "ios", test))]
async fn run_probe(
  client: reqwest::Client,
  cookie: reqwest::header::HeaderValue,
  timezone: &str,
  origin: &str,
) -> HarkProbe {
  use reqwest::header::{ACCEPT, COOKIE};
  use serde_json::Value;
  use std::time::Duration;

  let mut result = HarkProbe::default();
  let mut tracker = SyncTracker::default();
  let checked = async {
    let request = |path: &str, query: &[(&str, &str)]| -> Result<reqwest::RequestBuilder, String> {
      let mut url = url::Url::parse(&format!("{origin}/api{path}"))
        .map_err(|_| "Hark 诊断端点无效".to_string())?;
      url.query_pairs_mut().extend_pairs(query);
      Ok(
        client
          .get(url)
          .header(COOKIE, cookie.clone())
          .header("X-Hark-Timezone", timezone)
          .header("Cache-Control", "no-cache"),
      )
    };
    let response = request("/conversations/main", &[("timezone", timezone)])?
      .send()
      .await
      .map_err(|e| crate::egress::safe_http_error("Hark REST", &e))?;
    result.rest_status = Some(response.status().as_u16());
    let main = checked_json(response).await?;
    let conversation = ["conversationId", "conversation_id", "id"]
      .iter()
      .find_map(|key| {
        main
          .get(key)
          .and_then(Value::as_str)
          .filter(|id| !id.is_empty())
      })
      .or_else(|| main["conversation"]["id"].as_str())
      .or_else(|| main["main"]["id"].as_str())
      .filter(|id| !id.is_empty() && id.len() <= 128)
      .ok_or_else(|| "Hark 主会话标识不可观测".to_string())?;
    let manager = uuid::Uuid::new_v4().to_string();
    let base = [
      ("conversationId", conversation),
      ("manager", manager.as_str()),
      ("v", "2"),
      ("mode", "in_tab"),
      ("dial", "initial"),
    ];
    let sse = async {
      let mut params = base.to_vec();
      params.extend([("ids", "1"), ("beat", "5")]);
      let response = request("/sync/conversation", &params)?
        .header(ACCEPT, "text/event-stream")
        .send()
        .await
        .map_err(|e| crate::egress::safe_http_error("Hark SSE", &e))?;
      result.sse_status = Some(response.status().as_u16());
      read_sse(response, &mut tracker).await
    };
    result.sse_end = Some(
      match tokio::time::timeout(Duration::from_secs(12), sse).await {
        Ok(Ok(())) => "closed".to_string(),
        Ok(Err(e)) => e,
        Err(_) => "observation_window".to_string(),
      },
    );
    result.sse_frames = tracker.summary.frames;
    if matches!(result.sse_status, Some(401 | 403)) {
      return Err("Hark SSE 登录已失效".to_string());
    }
    let mut params = base.to_vec();
    let poll_manager = uuid::Uuid::new_v4().to_string();
    params[1].1 = &poll_manager;
    params.push(("first", "1"));
    let response = request("/sync/poll", &params)?
      .send()
      .await
      .map_err(|e| crate::egress::safe_http_error("Hark poll", &e))?;
    result.poll_status = Some(response.status().as_u16());
    let payload = checked_json(response).await?;
    let frames = payload["frames"]
      .as_array()
      .ok_or_else(|| "Hark poll 未返回 frames".to_string())?;
    for frame in frames {
      if let Some(raw) = frame.as_str() {
        tracker.apply_frame(raw);
      } else {
        tracker.summary.malformed_frames += 1;
      }
    }
    result.poll_frames = tracker.summary.frames - result.sse_frames;
    Ok::<_, String>(())
  }
  .await;
  result.error = checked.err();
  tracker.summary.known_messages = tracker.states.values().filter(|state| state.is_some()).count();
  tracker.summary.running_now = tracker.states.values().filter(|state| **state == Some(JobState::Running)).count();
  result.summary = tracker.summary;
  result.completed_ids = tracker.completed_ids;
  result
}

#[cfg(any(target_os = "ios", test))]
async fn checked_json(mut response: reqwest::Response) -> Result<serde_json::Value, String> {
  if !response.status().is_success() {
    return Err(format!("Hark HTTP {}", response.status().as_u16()));
  }
  let mut body = Vec::new();
  while let Some(bytes) = response
    .chunk()
    .await
    .map_err(|e| crate::egress::safe_http_error("读取 Hark 响应", &e))?
  {
    if body.len().saturating_add(bytes.len()) > 4 * 1024 * 1024 {
      return Err("Hark 响应超出诊断容量".to_string());
    }
    body.extend(bytes);
  }
  serde_json::from_slice(&body).map_err(|_| "Hark JSON 响应无法解析".to_string())
}

#[cfg(any(target_os = "ios", test))]
async fn read_sse(
  mut response: reqwest::Response,
  tracker: &mut SyncTracker,
) -> Result<(), String> {
  if !response.status().is_success() {
    return Err(format!("Hark SSE HTTP {}", response.status().as_u16()));
  }
  if !response
    .headers()
    .get(reqwest::header::CONTENT_TYPE)
    .and_then(|value| value.to_str().ok())
    .is_some_and(|value| value.starts_with("text/event-stream"))
  {
    return Err("Hark SSE 响应类型无效".to_string());
  }
  let mut decoder = SseDecoder::default();
  while let Some(bytes) = response
    .chunk()
    .await
    .map_err(|e| crate::egress::safe_http_error("读取 Hark SSE", &e))?
  {
    decoder.feed(&bytes, tracker).map_err(str::to_string)?;
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn read_only_probe_polls_after_the_stream_closes_and_deduplicates() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let initial =
      r#"{"type":"snapshot","data":{"messages":{"a":{"role":"assistant","jobStatus":"running"}}}}"#;
    let complete = r#"{"type":"patches","data":[{"op":"message_update","id":"a","fields":{"jobStatus":"completed"}}]}"#;
    let snapshot = r#"{"type":"snapshot","data":{"messages":{"a":{"role":"assistant","jobStatus":"completed"}}}}"#;
    let server = tokio::spawn(async move {
      let responses = [
        (
          "/api/conversations/main?",
          "application/json",
          r#"{"conversationId":"thread"}"#.to_string(),
        ),
        (
          "/api/sync/conversation?",
          "text/event-stream",
          format!("data: {initial}\n\ndata: {complete}\n\n"),
        ),
        (
          "/api/sync/poll?",
          "application/json",
          serde_json::json!({"frames":[snapshot]}).to_string(),
        ),
      ];
      for (path, mime, body) in responses {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
          assert!(request.len() < 4096);
          request.push(socket.read_u8().await.unwrap());
        }
        let request = String::from_utf8(request).unwrap();
        assert!(request.starts_with(&format!("GET {path}")));
        assert!(request.contains("cookie: fixture=value"));
        if path.contains("sync") {
          assert!(request.contains("conversationId=thread"));
        }
        if path.contains("poll") {
          assert!(request.contains("first=1"));
        }
        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        socket.write_all(response.as_bytes()).await.unwrap();
      }
    });
    let client = reqwest::Client::builder()
      .no_proxy()
      .timeout(std::time::Duration::from_secs(2))
      .build()
      .unwrap();
    let result = run_probe(
      client,
      reqwest::header::HeaderValue::from_static("fixture=value"),
      "UTC",
      &origin,
    )
    .await;
    assert_eq!(result.rest_status, Some(200));
    assert_eq!(result.sse_end.as_deref(), Some("closed"));
    assert_eq!(result.poll_status, Some(200));
    assert_eq!(result.sse_frames, 2);
    assert_eq!(result.poll_frames, 1);
    assert_eq!(result.summary.completed, 1);
    assert_eq!(result.summary.duplicate_states, 1);
    assert!(result.error.is_none());
    server.await.unwrap();
  }

  #[test]
  fn sse_and_poll_share_message_status_deduplication() {
    let snapshot = r#"{"type":"snapshot","data":{"messages":{"a":{"role":"assistant","jobStatus":"running"},"u":{"role":"user","jobStatus":"running"}}}}"#;
    let complete = r#"{"type":"patches","data":[{"op":"message_update","id":"a","fields":{"jobStatus":"completed"}}]}"#;
    let stream =
      format!(": heartbeat\r\n\r\nevent: message\r\ndata: {snapshot}\r\n\r\ndata: {complete}\n\n");
    let mut tracker = SyncTracker::default();
    let mut decoder = SseDecoder::default();
    for chunk in stream.as_bytes().chunks(3) {
      decoder.feed(chunk, &mut tracker).unwrap();
    }
    tracker.apply_frame(snapshot);
    tracker.apply_frame(complete);
    assert_eq!(tracker.summary.frames, 4);
    assert_eq!(
      tracker.summary.running, 0,
      "historical snapshots are not live transitions"
    );
    assert_eq!(tracker.summary.completed, 1);
    assert_eq!(tracker.summary.duplicate_states, 2);
    assert_eq!(tracker.summary.malformed_frames, 0);
  }
}
