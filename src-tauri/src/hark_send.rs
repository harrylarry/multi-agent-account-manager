use crate::{hark_native::{checked_json, HarkSessionSummary}, probe_cancel::CancelToken, send_ledger::{MissionGuard, SendLedger}};
use reqwest::{header::{HeaderValue, ACCEPT, CONTENT_TYPE, COOKIE, ORIGIN, REFERER}, Client, Method, RequestBuilder};
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendReport {
  pub conversation_id: String,
  pub send_id: String,
  pub confirmed: bool,
  pub response_seen: bool,
  pub http_status: Option<u16>,
  #[serde(skip)]
  pub completed_text: Option<String>,
}

pub(crate) struct Transport<'a> {
  pub client: Client,
  pub cookie: HeaderValue,
  pub timezone: &'a str,
  pub origin: &'a str,
}

impl Transport<'_> {
  fn request(&self, method: Method, path: &str, query: &[(&str, &str)]) -> Result<RequestBuilder, String> {
    let mut url = url::Url::parse(&format!("{}/api{path}", self.origin)).map_err(|_| "Hark 端点无效")?;
    if !query.is_empty() { url.query_pairs_mut().extend_pairs(query); }
    Ok(self.client.request(method, url)
      .header(COOKIE, self.cookie.clone()).header(ACCEPT, "application/json")
      .header(ORIGIN, self.origin).header(REFERER, format!("{}/", self.origin))
      .header("X-Hark-Timezone", self.timezone))
  }

  async fn read(&self, req: RequestBuilder) -> Result<Value, String> {
    checked_json(req.send().await.map_err(|e| crate::egress::safe_http_error("Hark 隔离会话", &e))?).await
  }

  async fn projects(&self) -> Result<Value, String> {
    self.read(self.request(Method::GET, "/conversations", &[])?).await
  }
}

pub(crate) fn valid_project_id(id: &str) -> bool {
  !id.is_empty() && id.len() <= 128 && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
}

fn project_id(value: &Value) -> Option<&str> {
  value["conversationId"].as_str().or_else(|| value["conversation_id"].as_str())
    .or_else(|| value["conversation"]["id"].as_str()).or_else(|| value["id"].as_str())
    .filter(|id| valid_project_id(id))
}

fn projects(list: &Value) -> Result<&[Value], String> {
  list.as_array().or_else(|| list["conversations"].as_array()).map(Vec::as_slice)
    .ok_or("Hark 会话列表格式无法核验".into())
}

pub(crate) async fn list_projects(transport: &Transport<'_>) -> Result<Vec<HarkSessionSummary>, String> {
  let response = transport.projects().await?;
  let rows = projects(&response)?;
  let mut result = Vec::new();
  for row in rows {
    let Some(id) = project_id(row) else { return Err("Hark 会话列表含无法核验的标识".into()); };
    let conversation = row.get("conversation").unwrap_or(row);
    let title = conversation["title"].as_str().filter(|title| !title.trim().is_empty()).unwrap_or("未命名会话");
    let updated_at_ms = ["updatedAtMs", "updated_at_ms"].iter()
      .find_map(|key| conversation[*key].as_u64()).filter(|time| *time <= 8_640_000_000_000_000);
    result.push(HarkSessionSummary {
      id: id.to_owned(), title: title.chars().take(500).collect(), updated_at_ms,
      pinned: conversation["pinned"] == true, archived: conversation["archived"] == true,
    });
  }
  Ok(result)
}

pub(crate) async fn project_is_main(transport: &Transport<'_>, id: &str) -> Result<bool, String> {
  if !valid_project_id(id) { return Err("Hark 项目标识无效".into()); }
  let main = transport.read(transport.request(Method::GET, "/conversations/main", &[("timezone", transport.timezone)])?).await?;
  let main_id = project_id(&main).or_else(|| main["main"]["id"].as_str()).ok_or("Hark 主会话不可核验")?;
  if main_id == id { return Ok(true); }
  let list = transport.projects().await?;
  if projects(&list)?.iter().any(|row| project_id(row) == Some(id)) { Ok(false) }
  else { Err("该会话不在所选账号的项目列表中".into()) }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Purpose { IsolationProbe, Fingerprint, Product }

fn response_id(send_id: &str) -> Result<String, String> {
  Uuid::parse_str(send_id).map(|uuid| Uuid::from_u128(uuid.as_u128() ^ 1).to_string())
    .map_err(|_| "发送标识无效".into())
}

fn receipt(payload: &Value, conversation: &str, key: &str, reply: &str) -> bool {
  let scope = payload["conversationId"].as_str().or_else(|| payload["conversation_id"].as_str());
  if payload["ok"] == false || payload["success"] == false || !payload["error"].is_null()
    || scope.is_some_and(|id| id != conversation) { return false; }
  let exact = payload["responseMessageId"].as_str() == Some(reply)
    || payload["response_message_id"].as_str() == Some(reply) || payload["idempotencyKey"].as_str() == Some(key);
  exact || (scope == Some(conversation) && payload["userMessageId"].as_str().is_some_and(|id| !id.is_empty()))
}

#[cfg(target_os = "ios")]
pub(crate) async fn read_project_snapshot(transport: &Transport<'_>, conversation: &str) -> Result<Value, String> {
  if !valid_project_id(conversation) { return Err("Hark 项目标识无效".into()); }
  let manager = Uuid::new_v4().to_string();
  let request = transport.request(Method::GET, "/sync/poll", &[
    ("conversationId", conversation), ("manager", &manager), ("v", "2"), ("mode", "in_tab"), ("dial", "initial"), ("first", "1"),
  ])?;
  transport.read(request).await
}

#[cfg(target_os = "ios")]
pub(crate) async fn verify_product_reply(transport: &Transport<'_>, conversation: &str, send_id: &str) -> bool {
  let Ok(reply_id) = response_id(send_id) else { return false; };
  let Ok(value) = read_project_snapshot(transport, conversation).await else { return false; };
  crate::hark_native::bound_reply(&value, conversation, &reply_id).0
}

pub(crate) async fn send_isolated(
  transport: &Transport<'_>, ledger: &SendLedger, mission: &MissionGuard<'_>, text: &str, token: &mut CancelToken,
  collect_limit: std::time::Duration, purpose: Purpose,
) -> Result<SendReport, String> {
  let setup = async {
    let old = transport.projects().await?;
    let main = transport.read(transport.request(Method::GET, "/conversations/main", &[("timezone", transport.timezone)])?).await?;
    let main_id = project_id(&main).or_else(|| main["main"]["id"].as_str()).ok_or("Hark 主会话不可核验")?;
    let prior = projects(&old)?;
    let title = if purpose == Purpose::Product { "新对话" } else { "叠号隔离验证" };
    let title = format!("{title} {}", &Uuid::new_v4().simple().to_string()[..8]);
    let created = transport.read(transport.request(Method::POST, "/conversations", &[])?
      .header(CONTENT_TYPE,"application/json").body(json!({"title":title,"timezone":transport.timezone,"autoTitle":false}).to_string())).await?;
    let id = project_id(&created).filter(|id| *id != main_id
      && !prior.iter().any(|project| project_id(project) == Some(id))).ok_or("Hark 未证实新建的是独立会话")?;
    for _ in 0..3 {
      let refreshed = transport.projects().await?;
      if projects(&refreshed)?.iter().any(|project| project_id(project) == Some(id) && project["title"].as_str() == Some(&title)) {
        return Ok(id.to_owned());
      }
      tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    Err("无法在账号列表核验新建隔离会话，尚未发送".to_string())
  };
  let conversation = tokio::select! {
    biased;
    _ = token.cancelled() => return Err("会话准备取消，尚未发送".into()),
    result = tokio::time::timeout(std::time::Duration::from_secs(35), setup) => result.map_err(|_| "会话准备超时，尚未发送")??,
  };
  let digest: String = ring::digest::digest(&ring::digest::SHA256, text.as_bytes()).as_ref()
    .iter().map(|byte| format!("{byte:02x}")).collect();
  let prepared = if purpose == Purpose::Product {
    mission.begin_hark_message(&conversation, &digest)
  } else {
    mission.begin_hark_send(&conversation, &digest, purpose == Purpose::IsolationProbe)
  };
  let send_id = prepared.map_err(|_| "无法写入发送台账")?;
  let reply_id = response_id(&send_id)?;
  if token.is_cancelled() {
    ledger.fail(&send_id, "尚未发送".into());
    return Err("发送已取消".into());
  }
  ledger.mark_in_flight_in(&send_id, Some(&conversation)).map_err(|_| "无法持久化发送状态")?;
  let request = transport.request(Method::POST, "/messages/send", &[("cid", &conversation)])?
    .header(CONTENT_TYPE,"application/json").body(json!({"timezone":transport.timezone,"conversationId":conversation,"message":text,
      "idempotencyKey":send_id,"responseMessageId":reply_id}).to_string());
  let exchange = async {
    let response = request.send().await.ok()?;
    let status = response.status().as_u16();
    Some((status, checked_json(response).await.ok()))
  };
  let result = tokio::select! {
    biased;
    _ = token.cancelled() => None,
    response = exchange => response,
  };
  let mut report = SendReport { conversation_id:conversation.clone(), send_id:send_id.clone(), confirmed:false,
    response_seen:false, http_status:result.as_ref().map(|r|r.0), completed_text:None };
  if purpose != Purpose::IsolationProbe && result.and_then(|r| r.1).is_some_and(|payload| receipt(&payload, &conversation, &send_id, &reply_id)) {
    ledger.confirm(&send_id);
    report.confirmed = true;
  }
  if purpose == Purpose::Product && report.confirmed {
    if ledger.is_poisoned() { return Err("Hark 回执已收到，发送台账待核对".into()); }
    return Ok(report);
  }
  let _ = tokio::time::timeout(collect_limit, async {
    while !token.is_cancelled() {
      let manager = Uuid::new_v4().to_string();
      let Ok(req) = transport.request(Method::GET, "/sync/poll",
        &[("conversationId", conversation.as_str()), ("manager", manager.as_str()),
          ("v","2"), ("mode","in_tab"), ("dial","initial"), ("first","1")]) else { break; };
      let snapshot = tokio::select! {
        biased;
        _ = token.cancelled() => break,
        result = transport.read(req) => result.ok(),
      };
      if let Some(data) = snapshot {
        let (seen, completed_text) = crate::hark_native::bound_reply(&data, &conversation, &reply_id);
        report.response_seen |= seen;
        if !report.confirmed && seen && (purpose != Purpose::IsolationProbe || completed_text.is_some()) {
          ledger.confirm(&send_id);
          report.confirmed = true;
        }
        if completed_text.is_some() || (purpose == Purpose::Product && report.confirmed) { report.completed_text = completed_text; break; }
      }
      tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
  }).await;
  if !report.confirmed { ledger.mark_unknown(&send_id, "未取得可关联的回执，禁止自动重发".into()); }
  if ledger.is_poisoned() { return Err("Hark 请求结果已产生，但台账写入失败。请核对隔离会话。".into()); }
  Ok(report)
}

#[cfg(test)]
mod tests {
  use super::*;
  use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpListener};

  #[tokio::test]
  async fn sends_once_to_new_project_and_marks_an_unbound_ack_unknown() {
    for (created, completed, purpose) in [
      ("new", false, Purpose::IsolationProbe),
      ("main", false, Purpose::IsolationProbe),
      ("new", true, Purpose::IsolationProbe),
      ("new", false, Purpose::Product),
    ] {
      let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
      let origin = format!("http://{}", listener.local_addr().unwrap());
      let server = tokio::spawn(async move {
        let rounds = if purpose == Purpose::Product { 5 } else if created == "new" { 6 } else { 3 };
        let mut title = String::new();
        let mut reply_id = String::new();
        for index in 0..rounds {
          let (mut conn, _) = listener.accept().await.unwrap();
          let mut input = Vec::new();
          let (end, len) = loop {
            input.push(conn.read_u8().await.unwrap());
            if input.ends_with(b"\r\n\r\n") {
              let header = String::from_utf8_lossy(&input);
              let len = header.lines().find_map(|line| {
                let (name, raw) = line.split_once(':')?;
                name.eq_ignore_ascii_case("Content-Length").then(|| raw.trim().parse::<usize>().unwrap())
              }).unwrap_or(0);
              break (input.len(), len);
            }
          };
          let mut body = vec![0; len];
          conn.read_exact(&mut body).await.unwrap();
          let line = std::str::from_utf8(&input[..end]).unwrap().lines().next().unwrap();
          let response = match index {
            0 => { assert!(line.starts_with("GET /api/conversations ")); json!([{"id":"main"}]) }
            1 => { assert!(line.starts_with("GET /api/conversations/main?")); json!({"conversationId":"main"}) }
            2 => {
              assert!(line.starts_with("POST /api/conversations "));
              let payload: Value = serde_json::from_slice(&body).unwrap();
              assert_eq!(payload["autoTitle"], false);
              title = payload["title"].as_str().unwrap().into();
              json!({"conversationId":created})
            }
            3 => json!([{"id":"main"},{"id":"new","title":title}]),
            4 => {
              assert!(line.starts_with("POST /api/messages/send?cid=new "));
              let payload: Value = serde_json::from_slice(&body).unwrap();
              assert_eq!(payload["conversationId"], "new");
              assert_ne!(payload["idempotencyKey"], payload["responseMessageId"]);
              reply_id = payload["responseMessageId"].as_str().unwrap().into();
              json!({"conversationId":if purpose == Purpose::Product { "new" } else { "main" }, "responseMessageId":payload["responseMessageId"]})
            }
            5 => {
              assert!(line.starts_with("GET /api/sync/poll?conversationId=new"));
              if completed {
                let frame = json!({"type":"snapshot","conversationId":"new","data":{"messages":[{
                  "id":reply_id, "role":"assistant", "jobStatus":"completed", "content":"OK"
                }]}}).to_string();
                json!({"frames":[frame]})
              } else { json!({"frames":[]}) }
            }
            _ => unreachable!(),
          }.to_string();
          conn.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response).as_bytes()).await.unwrap();
        }
      });
      let registry = crate::probe_cancel::ProbeCancel::new();
      let (mut token, _guard) = registry.register("account", "hark");
      let ledger = SendLedger::new();
      let mission = ledger.mission("account").unwrap();
      let transport = Transport { client:Client::builder().no_proxy().timeout(std::time::Duration::from_secs(2)).build().unwrap(),
        cookie:HeaderValue::from_static("mock=1"), timezone:"UTC", origin:&origin };
      let result = send_isolated(&transport, &ledger, &mission, "Test", &mut token, std::time::Duration::from_millis(500), purpose).await;
      if created == "new" {
        let report = result.unwrap();
        assert_eq!(report.confirmed, completed || purpose == Purpose::Product);
        assert_eq!(crate::hark_isolation_proved(&ledger, "account"), completed);
        assert_eq!(ledger.attempts().len(), 1);
        assert_eq!(report.completed_text.as_deref(), if completed { Some("OK") } else { None });
        assert_eq!(ledger.attempts()[0].status,
          if completed || purpose == Purpose::Product { crate::send_ledger::SendStatus::Confirmed } else { crate::send_ledger::SendStatus::Unknown });
        if purpose == Purpose::Product {
          assert_eq!(ledger.attempts()[0].kind, crate::send_ledger::SendKind::HarkMessage);
          assert_eq!(ledger.attempts()[0].digest.len(), 64);
        }
      } else {
        assert!(result.is_err());
        assert!(ledger.attempts().is_empty());
      }
      server.await.unwrap();
    }
  }
}
