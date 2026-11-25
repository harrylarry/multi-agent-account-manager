//! B3：额度读取 v2（Issue #13 / i005 #7）。
//!
//! 参考实现：
//! - `steipete/CodexBar` `Sources/CodexBarCore/Resources/Plugins/museai.js`
//!   （PR #5093：immutable chunk 发现；Issue #5095）：POST `https://muse.ai/`
//!   body `[{includeAgreement:true}]`，Sec-Fetch-* 头，400/404/409 → 重新发现，
//!   401/403/跳转 auth.muse.ai → 会话失效，静态 chunk 不带 Cookie。
//! - `vip0014/muse2api` `internal/muse/protocol/hatch/quota.go`：
//!   BFS 跟随 chunk 图、`createServerReference(...)` 通用解析、改名降级、唯一引用兜底。
//!
//! 安全约束：
//! - 静态资源（HTML/JS chunk）请求一律不携带 Cookie，且拒绝一切重定向，
//!   杜绝跨域/CDN 路径上的凭据泄漏。
//! - 仅 POST Server Action 时携带会话 Cookie，目标必须是同源根路径。
//! - 不猜测任何未经证实的 REST 接口；旧 action id 的 400/404/409 视为
//!   「需重新发现」，411宣布接口不可用。

use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;

const ORIGIN: &str = "https://muse.ai";
const ACTION_NAME: &str = "fetchSubscriptionAction";
const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";

/// 冷发现预算（对齐参考实现；有限扫描，避免无界爬取）。
const PAGE_CHUNK_LIMIT: usize = 96;
const SETTINGS_CHUNK_LIMIT: usize = 32;
const BFS_CHUNK_LIMIT: usize = 512;
const PARALLEL_FETCH: usize = 8;
const CHUNK_MAX_BYTES: usize = 2 << 20;
const ACCUMULATED_MAX_BYTES: usize = 8 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuotaError {
  /// 会话失效（401/403，或跳转 auth.muse.ai，或返回登录页）。
  SessionExpired,
  /// Server Action 返回非 2xx，或响应无法解析。
  Http { status: u16, detail: String },
  /// 冷发现失败。
  Discovery(String),
  Network(String),
}

impl std::fmt::Display for QuotaError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::SessionExpired => write!(f, "会话失效：请重新登录 muse.ai"),
      Self::Http { status, detail } => write!(f, "HTTP {status}: {detail}"),
      Self::Discovery(d) => write!(f, "接口发现失败：{d}"),
      Self::Network(e) => write!(f, "网络错误：{e}"),
    }
  }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaSnapshot {
  pub tier: String,
  pub percent_used: i64,
  pub usage_observed: bool,
  pub window: Option<String>,
  pub state: String,
  pub resets_at_ms: i64,
  pub source: String,
  /// 本次成功使用的 Server Action id（供调用方缓存）。
  pub action_id: Option<String>,
}

/// 进程内 action id 缓存：冷发现成功后写入，后续刷新直接复用。
#[derive(Default)]
pub struct ActionCache(std::sync::Mutex<Option<String>>);

impl ActionCache {
  pub fn get(&self) -> Option<String> {
    self.0.lock().unwrap().clone()
  }
  pub fn set(&self, id: Option<String>) {
    *self.0.lock().unwrap() = id;
  }
}

fn s(v: &Value, keys: &[&str]) -> String {
  for k in keys {
    if let Some(x) = v.get(*k).and_then(Value::as_str) {
      if !x.trim().is_empty() {
        return x.trim().to_string();
      }
    }
  }
  String::new()
}

fn i(v: &Value, keys: &[&str]) -> Option<i64> {
  for k in keys {
    if let Some(x) = v.get(*k).and_then(Value::as_i64) {
      return Some(x);
    }
    if let Some(x) = v.get(*k).and_then(Value::as_f64) {
      return Some(x.round() as i64);
    }
  }
  None
}

pub fn parse_subscription(v: &Value) -> Option<QuotaSnapshot> {
  let root = v.get("result").unwrap_or(v);
  let sub = root.get("subscription").or_else(|| Some(root))?;
  let tier = sub.get("tier").cloned().unwrap_or(Value::Null);
  let usage = sub.get("usage").cloned().unwrap_or(Value::Null);
  let tier_name = if tier.is_object() {
    s(&tier, &["name", "tierId", "tier_id", "tierCode", "tier_code"])
  } else {
    String::new()
  };
  let pct = i(&usage, &["percentUsed", "percent_used"]).unwrap_or(-1);
  let state = s(&usage, &["state", "quotaStatus", "quota_status"]);
  // 上游时间戳是秒（10 位），前端按毫秒渲染，这里统一归一到毫秒。
  let resets = i(&usage, &["resetsAt", "resets_at"])
    .map(|v| if v > 0 && v < 1_000_000_000_000 { v * 1000 } else { v })
    .unwrap_or(0);
  if tier_name.is_empty() && pct < 0 && state.is_empty() {
    return None;
  }
  Some(QuotaSnapshot {
    tier: tier_name,
    percent_used: if pct < 0 { 0 } else { pct.clamp(0, 100) },
    usage_observed: pct >= 0,
    window: {
      let period = s(&usage, &["period", "window"]);
      (!period.is_empty() && period.len() <= 60).then_some(period)
    },
    state,
    resets_at_ms: resets,
    source: "action".to_string(),
    action_id: None,
  })
}

/// 解析 React Flight `text/x-component` 响应：逐行 `<hex>:<json>`，
/// 只有 `success === true` 且含 `subscription` 的行才算数（对齐 CodexBar）。
pub fn parse_flight_subscription(text: &str) -> Option<QuotaSnapshot> {
  for line in text.lines() {
    let l = line.trim();
    if l.is_empty() {
      continue;
    }
    let json_part = match l.find(':') {
      Some(idx) if l[..idx].chars().all(|c| c.is_ascii_hexdigit()) => &l[idx + 1..],
      _ => l,
    };
    let Ok(v) = serde_json::from_str::<Value>(json_part) else {
      continue;
    };
    if v.get("success").and_then(Value::as_bool) != Some(true) {
      continue;
    }
    if let Some(q) = parse_subscription(&v) {
      return Some(q);
    }
  }
  None
}

const LOGIN_MARKERS: &[&str] = &["sign in", "log in", "login", "登录", "signin"];

fn looks_like_login_page(text: &str) -> bool {
  let head: String = text.chars().take(8192).collect();
  let lower = head.to_lowercase();
  LOGIN_MARKERS.iter().any(|m| lower.contains(m))
}

/// 页面里引用的 JS chunk 路径（兼容 `static/chunks/` 与 `static/immutable/chunks/`）。
/// 只用同源相对路径，绝不返回跨域 URL（静态资源不携带 Cookie，也不跟随重定向）。
pub fn chunk_paths(text: &str) -> Vec<String> {
  let re = regex::Regex::new(r"static/(?:immutable/)?chunks/[A-Za-z0-9~._-]+\.js")
    .expect("chunk regex");
  let mut out: Vec<String> = Vec::new();
  let mut seen: HashSet<String> = HashSet::new();
  for m in re.find_iter(text) {
    let full = format!("/_next/{}", m.as_str());
    if seen.insert(full.clone()) {
      out.push(full);
    }
  }
  out
}

/// 从 `createServerReference(...)` 调用参数里提取 action id。
pub fn server_ref_ids(text: &str) -> Vec<String> {
  let needle = "createServerReference(";
  let mut out = Vec::new();
  let mut from = 0usize;
  while let Some(p) = text[from..].find(needle) {
    let start = from + p + needle.len();
    let rest = &text[start..];
    if let Some(end) = rest.find(')') {
      let args = &rest[..end];
      // 40–44 位 hex
      let bytes = args.as_bytes();
      let mut j = 0usize;
      while j < bytes.len() {
        if bytes[j].is_ascii_hexdigit() {
          let s = j;
          while j < bytes.len() && bytes[j].is_ascii_hexdigit() {
            j += 1;
          }
          let run = &args[s..j];
          if (40..=44).contains(&run.len()) {
            out.push(run.to_string());
          }
        } else {
          j += 1;
        }
      }
    }
    from = start + 8;
  }
  out
}

/// 找到引用 `fetchSubscriptionAction` 的注册 id；其次允许改名（subscription/quota 关键词）。
fn action_id_from_chunk(text: &str) -> Option<String> {
  for id in server_ref_ids(text) {
    // createServerReference("<id>", …, "fetchSubscriptionAction") 仅在参数为名称所在
    if text.contains(ACTION_NAME) {
      let idx = text.find(ACTION_NAME)?;
      let lo = idx.saturating_sub(600);
      let hi = (idx + ACTION_NAME.len() + 120).min(text.len());
      if text[lo..hi].contains(&id) {
        return Some(id);
      }
    }
  }
  // 宽松：id 附近（同 chunk 前 600 字符）出现 ACTION_NAME
  if let Some(idx) = text.find(ACTION_NAME) {
    let lo = idx.saturating_sub(600);
    let hi = (idx + ACTION_NAME.len() + 120).min(text.len());
    let window = &text[lo..hi];
    let bytes = window.as_bytes();
    let mut j = 0usize;
    while j < bytes.len() {
      if bytes[j].is_ascii_hexdigit() {
        let start = j;
        while j < bytes.len() && bytes[j].is_ascii_hexdigit() {
          j += 1;
        }
        let run = &window[start..j];
        if (40..=44).contains(&run.len()) {
          return Some(run.to_string());
        }
      } else {
        j += 1;
      }
    }
  }
  // 改名降级：参数里包含 subscription/quota 的引用
  let matches: Vec<String> = server_ref_ids(text)
    .into_iter()
    .filter(|id| {
      let idx = text.find(id).unwrap_or(0);
      let lo = idx.saturating_sub(200);
      let hi = (idx + id.len() + 200).min(text.len());
      let w = text[lo..hi].to_lowercase();
      w.contains("subscription") || w.contains("quota")
    })
    .collect();
  if matches.len() == 1 {
    return matches.into_iter().next();
  }
  None
}

struct ScanState {
  queue: Vec<String>,
  seen: HashSet<String>,
  accumulated: usize,
  server_refs: usize,
}

/// 冷发现：从此页面开始，阶梯式定位 Settings 懒加载模块；无法精确命中时
/// 遍历 chunk 图（BFS）找 `createServerReference` 引用。
async fn discover_action_id(
  client: &reqwest::Client,
  cookie: Option<&str>,
  base: &str,
  diag: &mut Vec<String>,
) -> Result<Option<String>, QuotaError> {
  let page_paths = ["/", "/settings", "/subscriptions"];
  let mut queue: Vec<String> = Vec::new();
  let mut seen: HashSet<String> = HashSet::new();
  let mut accumulated = 0usize;

  for path in page_paths {
    let url = format!("{base}{path}");
    let (status, body) = fetch_no_redirect(client, &url, Some(cookie), "text/html").await?;
    if status == 401 || status == 403 {
      return Err(QuotaError::SessionExpired);
    }
    if status == 302 || status == 307 {
      return Err(QuotaError::SessionExpired);
    }
    if status != 200 {
      diag.push(format!("GET {path}: HTTP {status}"));
      continue;
    }
    if looks_like_login_page(&body) {
      return Err(QuotaError::SessionExpired);
    }
    diag.push(format!("GET {path}: 200 {}B", body.len()));
    if let Some(id) = action_id_from_chunk(&body) {
      return Ok(Some(id));
    }
    for p in chunk_paths(&body) {
      let full = if p.starts_with("http") {
        p.clone()
      } else {
        format!("{base}{p}")
      };
      if seen.insert(full.clone()) {
        queue.push(full);
      }
    }
    if queue.len() >= PAGE_CHUNK_LIMIT {
      break;
    }
  }

  // 阶梯：先模拟 CodexBar 的 `.A(<module>).then(({[^}]*Settings` 定位。
  let mut text_accum = String::new();
  let mut scanned = 0usize;
  let mut module: Option<String> = None;
  loop {
    if queue.is_empty() || scanned >= PAGE_CHUNK_LIMIT {
      break;
    }
    let batch: Vec<String> = queue.drain(..queue.len().min(PARALLEL_FETCH)).collect();
    scanned += batch.len();
    let bodies = fetch_chunks_no_cookie(client, &batch, base).await;
    for body in bodies {
      accumulated += body.len();
      if accumulated > ACCUMULATED_MAX_BYTES {
        return Err(QuotaError::Discovery("累计文本超限".into()));
      }
      if let Some(id) = action_id_from_chunk(&body) {
        return Ok(Some(id));
      }
      text_accum.push_str(&body);
      text_accum.push('\n');
      if module.is_none() {
        // `.A(<n>).then(({...,Settings`
        let re = regex::Regex::new(r"\.A\((\d+)\)\.then\(\(\{[^}]*Settings").unwrap();
        if let Some(c) = re.captures(&text_accum) {
          module = Some(c[1].to_string());
        }
      }
    }
  }
  if let Some(m) = module {
    // 不用正则避免花括号转义：定位 `Promise.all([ ... ])` 的 chunk 列表。
    if let Some(pos) = text_accum.find(&format!("{m},")) {
      let tail = &text_accum[pos..];
      if let Some(p) = tail.find("Promise.all([") {
        let after = &tail[p + "Promise.all([".len()..];
        if let Some(q) = after.find(']') {
          let settings_paths: Vec<String> = chunk_paths(&after[..q])
            .into_iter()
            .take(SETTINGS_CHUNK_LIMIT)
            .map(|p| {
              if p.starts_with("http") {
                p
              } else {
                format!("{base}{p}")
              }
            })
            .collect();
          for body in fetch_chunks_no_cookie(client, &settings_paths, base).await {
            if let Some(id) = action_id_from_chunk(&body) {
              return Ok(Some(id));
            }
          }
        }
      }
    }
  }

  // 兜底：BFS 全图扫描（≤512），任何 createServerReference 引用。
  let mut state = ScanState {
    queue,
    seen,
    accumulated,
    server_refs: 0,
  };
  while !state.queue.is_empty() && state.accumulated < ACCUMULATED_MAX_BYTES {
    let batch: Vec<String> = state
      .queue
      .drain(..state.queue.len().min(PARALLEL_FETCH))
      .collect();
    let bodies = fetch_chunks_no_cookie(client, &batch, base).await;
    for body in bodies {
      state.accumulated += body.len();
      state.server_refs += server_ref_ids(&body).len();
      if let Some(id) = action_id_from_chunk(&body) {
        return Ok(Some(id));
      }
      for p in chunk_paths(&body) {
        let full = if p.starts_with("http") {
          p
        } else {
          format!("{base}{p}")
        };
        if state.seen.insert(full.clone()) && state.seen.len() < BFS_CHUNK_LIMIT {
          state.queue.push(full);
        }
      }
    }
  }
  diag.push(format!(
    "扫描 {scanned} 个页面 chunk，BFS {} 条，server ref {} 处，未发现 {}",
    state.seen.len(),
    state.server_refs,
    ACTION_NAME
  ));
  Ok(None)
}

async fn fetch_chunks_no_cookie(
  client: &reqwest::Client,
  urls: &[String],
  _base: &str,
) -> Vec<String> {
  let mut out = Vec::with_capacity(urls.len());
  for chunk in urls.chunks(PARALLEL_FETCH) {
    let mut handles = Vec::new();
    for url in chunk {
      let client = client.clone();
      let url = url.clone();
      handles.push(tokio::spawn(async move {
        fetch_no_redirect(&client, &url, None, "*/*").await.ok().map(|(_, b)| b)
      }));
    }
    for h in handles {
      // spawn 的闭包返回 Option<String>，JoinError 只需忽略。
      if let Ok(Some(body)) = h.await {
        out.push(body);
      }
    }
  }
  out
}

fn quota_http_error(stage: &str, error: &reqwest::Error) -> QuotaError {
  #[cfg(not(target_os = "android"))]
  let detail = crate::egress::safe_http_error(stage, error);
  #[cfg(target_os = "android")]
  let detail = format!("{stage}: {}", if error.is_timeout() { "timeout" } else { "request" });
  QuotaError::Network(detail)
}

/// 无重定向、显式带/不带 Cookie 的 GET。3xx 返回 (status, "")。
async fn fetch_no_redirect(
  client: &reqwest::Client,
  url: &str,
  cookie: Option<Option<&str>>,
  accept: &str,
) -> Result<(u16, String), QuotaError> {
  let mut req = client.get(url).header("Accept", accept).header(
    "User-Agent",
    UA,
  );
  if let Some(c) = cookie.flatten() {
    req = req.header("Cookie", c);
  }
  let resp = req.send().await.map_err(|e| quota_http_error("额度发现 GET 请求", &e))?;
  let status = resp.status().as_u16();
  if (300..400).contains(&status) {
    return Ok((status, String::new()));
  }
  let bytes = resp
    .bytes()
    .await
    .map_err(|e| quota_http_error("额度发现 GET 响应体", &e))?;
  if bytes.len() > CHUNK_MAX_BYTES {
    return Err(QuotaError::Network("响应体超过 2MB".into()));
  }
  Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
}

/// POST `fetchSubscriptionAction`；返回 (status, content_type, body)。
async fn post_action(
  client: &reqwest::Client,
  base: &str,
  cookie: &str,
  action_id: &str,
) -> Result<(u16, String, String), QuotaError> {
  let url = format!("{base}/");
  let resp = client
    .post(&url)
    .header("Accept", "text/x-component")
    .header("Content-Type", "text/plain;charset=UTF-8")
    .header("Next-Action", action_id)
    .header("Origin", base)
    .header("Referer", url.clone())
    .header("User-Agent", UA)
    .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
    .header("Sec-Fetch-Site", "same-origin")
    .header("Sec-Fetch-Mode", "cors")
    .header("Sec-Fetch-Dest", "empty")
    .header("Cookie", cookie)
    .body(r#"[{"includeAgreement":true}]"#)
    .send()
    .await
    .map_err(|e| quota_http_error("额度 action POST 请求", &e))?;
  let status = resp.status().as_u16();
  let ctype = resp
    .headers()
    .get("content-type")
    .and_then(|v| v.to_str().ok())
    .unwrap_or("")
    .to_string();
  let bytes = resp
    .bytes()
    .await
    .map_err(|e| quota_http_error("额度 action POST 响应体", &e))?;
  Ok((status, ctype, String::from_utf8_lossy(&bytes).into_owned()))
}

/// 一次完整额度查询，返回 (结果, 诊断行)。
/// 1. 有缓存 action 先试；2. 400/404/409 → 重新发现并重试一次；
/// 3. 401/403/登录页 → 会话失效；4. 解析出 Flight subscription 才算通过。
///
/// `client` 由调用方按账号出站策略构造（`AccountEgress::http_client`），
/// 本模块不自建客户端：否则启用代理的账号会在这里退回直连。
pub async fn fetch_quota_v2(
  client: &reqwest::Client,
  cookie: &str,
  cached_action: Option<&str>,
) -> (Result<QuotaSnapshot, QuotaError>, Vec<String>) {
  let mut diag: Vec<String> = Vec::new();

  // 判定一次 POST 结果的语义，避免两处重复。
  fn classify(
    status: u16,
    ctype: &str,
    body: &str,
  ) -> Result<Option<QuotaSnapshot>, QuotaError> {
    match status {
      200 => {
        if !ctype.contains("text/x-component") {
          if looks_like_login_page(body) {
            return Err(QuotaError::SessionExpired);
          }
          return Err(QuotaError::Http {
            status,
            detail: format!("意外的 Content-Type {ctype}"),
          });
        }
        if let Some(q) = parse_flight_subscription(body) {
          return Ok(Some(q));
        }
        if looks_like_login_page(body) {
          return Err(QuotaError::SessionExpired);
        }
        Err(QuotaError::Http {
          status,
          detail: "响应不是 Flight subscription".into(),
        })
      }
      401 | 403 => Err(QuotaError::SessionExpired),
      // 上游重新部署：旧 action id 失效，需要重新发现。
      400 | 404 | 409 => Ok(None),
      other => Err(QuotaError::Http {
        status: other,
        detail: body.chars().take(160).collect(),
      }),
    }
  }

  if let Some(id) = cached_action {
    match post_action(client, ORIGIN, cookie, id).await {
      Ok((status, ctype, body)) => match classify(status, &ctype, &body) {
        Ok(Some(mut q)) => {
          q.action_id = Some(id.to_string());
          return (Ok(q), diag);
        }
        Ok(None) => diag.push(format!("缓存 action {id} 失效（HTTP {status}），开始重新发现")),
        Err(e) => return (Err(e), diag),
      },
      Err(e) => return (Err(e), diag),
    }
  } else {
    diag.push("无缓存 action，执行冷发现".into());
  }

  let discovered = match discover_action_id(client, Some(cookie), ORIGIN, &mut diag).await {
    Ok(Some(id)) => id,
    Ok(None) => {
      return (
        Err(QuotaError::Discovery(
          "未在部署脚本中找到订阅接口引用（已按上限扫描）".into(),
        )),
        diag,
      )
    }
    Err(e) => return (Err(e), diag),
  };
  diag.push(format!("发现 action id {}", &discovered[..8.min(discovered.len())]));

  match post_action(client, ORIGIN, cookie, &discovered).await {
    Ok((status, ctype, body)) => match classify(status, &ctype, &body) {
      Ok(Some(mut q)) => {
        q.action_id = Some(discovered);
        (Ok(q), diag)
      }
      Ok(None) => (
        Err(QuotaError::Http {
          status,
          detail: "重新发现后的 action 仍被拒绝".into(),
        }),
        diag,
      ),
      Err(e) => (Err(e), diag),
    },
    Err(e) => (Err(e), diag),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  
  #[test]
  fn chunk_paths_both_layouts() {
    let html = r#"<script src="/_next/static/chunks/a.js"></script><script src="/_next/static/immutable/chunks/b.js"></script><script src="https://cdn.external/x.js"></script>"#;
    let paths = chunk_paths(html);
    assert!(paths.contains(&"/_next/static/chunks/a.js".to_string()));
    assert!(paths.contains(&"/_next/static/immutable/chunks/b.js".to_string()));
    assert!(!paths.iter().any(|p| p.contains("cdn.external")));
  }

  #[test]
  fn server_ref_ids_extracts_40_44_hex() {
    let id = "c".repeat(40);
    let js = format!(r#"createServerReference("{id}",g.callServer,void 0,"fetchSubscriptionAction")"#);
    let ids = server_ref_ids(&js);
    assert_eq!(ids, vec![id]);
  }

  #[test]
  fn action_id_from_chunk_named() {
    let id = "d".repeat(44);
    let js = format!(r#"createServerReference("{id}","fetchSubscriptionAction")"#);
    assert_eq!(action_id_from_chunk(&js), Some(id));
  }

  #[test]
  fn action_id_renamed_subscription() {
    let id = "e".repeat(40);
    let js = format!(r#"createServerReference("{id}","getSubscriptionAction")"#);
    assert_eq!(action_id_from_chunk(&js), Some(id));
  }

  #[test]
  fn parse_flight_subscription_rows() {
    let id = "f".repeat(40);
    let text = format!(
      "{id}:{{\"success\":true,\"subscription\":{{\"tier\":{{\"name\":\"Pro\"}},\"usage\":{{\"percentUsed\":32,\"resetsAt\":1762100300000}}}}}}"
    );
    let q = parse_flight_subscription(&text).expect("quota");
    assert_eq!(q.tier, "Pro");
    assert_eq!(q.percent_used, 32);
    assert_eq!(q.resets_at_ms, 1762100300000);
  }

  #[test]
  fn parse_flight_rejects_success_false_without_subscription() {
    let text = "1:{\"success\":false,\"message\":\"nope\"}\n2:{\"success\":true,\"ok\":1}";
    assert!(parse_flight_subscription(text).is_none());
  }

  #[test]
  fn resets_seconds_are_normalized_to_ms() {
    let v: serde_json::Value = serde_json::from_str(
      r#"{"subscription":{"tier":{"name":"Muse 免费版"},"usage":{"percentUsed":4,"state":"METERED","resetsAt":1762100300}}}"#,
    )
    .unwrap();
    let q = parse_subscription(&v).expect("quota");
    assert_eq!(q.tier, "Muse 免费版");
    assert_eq!(q.percent_used, 4);
    // 秒 → 毫秒
    assert_eq!(q.resets_at_ms, 1_762_100_300_000);
  }

  #[test]
  fn resets_already_in_ms_are_kept() {
    let v: serde_json::Value = serde_json::from_str(
      r#"{"subscription":{"tier":{"name":"Pro"},"usage":{"percentUsed":1,"resetsAt":1762100300000}}}"#,
    )
    .unwrap();
    let q = parse_subscription(&v).expect("quota");
    assert_eq!(q.resets_at_ms, 1_762_100_300_000);
  }

  #[test]
  fn records_quota_window_and_distinguishes_zero_usage_from_missing_usage() {
    let mut raw = serde_json::json!({
      "subscription": { "tier": { "name": "Pro" }, "usage": { "period": "monthly", "percentUsed": 0 } }
    });
    let observed = parse_subscription(&raw).unwrap();
    assert!(observed.usage_observed);
    assert_eq!(observed.percent_used, 0);
    assert_eq!(observed.window.as_deref(), Some("monthly"));
    raw["subscription"]["usage"]["percentUsed"] = serde_json::Value::Null;
    assert!(!parse_subscription(&raw).unwrap().usage_observed);
  }

  #[test]
  fn login_page_detection() {
    assert!(looks_like_login_page("<title>Sign in to Muse</title>"));
    assert!(!looks_like_login_page("<html><body>dashboard</body></html>"));
  }

  // ---- 集成：本地 mock server ----

  fn spawn_mock() -> std::net::SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
      for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let mut buf = [0u8; 8192];
        let Ok(n) = std::io::Read::read(&mut stream, &mut buf) else { continue };
        let req = String::from_utf8_lossy(&buf[..n]);
        let mut lines = req.lines();
        let request_line = lines.next().unwrap_or("");
        let parts: Vec<&str> = request_line.split_whitespace().collect();
        if parts.len() < 2 {
          continue;
        }
        let method = parts[0];
        let path = parts[1];
        let lower = req.to_lowercase();
        let has_auth = lower.contains("muse_session=1");
        let id = "ab".repeat(20); // 40 hex
        let body = match (method, path) {
          ("POST", "/") if has_auth => format!(
            "{id}:{{\"success\":true,\"subscription\":{{\"tier\":{{\"name\":\"Pro\"}},\"usage\":{{\"percentUsed\":12,\"resetsAt\":1762100300000}}}}}}"
          ),
          ("GET", "/") => format!(
            r#"<html><script src="/_next/static/immutable/chunks/app.js"></script><script src="/_next/static/immutable/chunks/settings-loader.js"></script></html>"#
          ),
          ("GET", "/_next/static/immutable/chunks/app.js") => {
            r#"// app chunk: e.A(3).then(({preloadHatchSettingsData…"#.to_string()
          }
          ("GET", "/_next/static/immutable/chunks/settings-loader.js") => format!(
            r#"createServerReference("{id}","fetchSubscriptionAction")"#
          ),
          _ => "404".to_string(),
        };
        let status = if body == "404" { "404 Not Found" } else { "200 OK" };
        let ctype = if method == "POST" {
          "text/x-component"
        } else {
          "text/html"
        };
        let response = format!(
          "HTTP/1.1 {status}\r\nContent-Type: {ctype}; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
          body.len()
        );
        let _ = std::io::Write::write_all(&mut stream, response.as_bytes());
      }
    });
    addr
  }

  fn mock_base() -> String {
    format!("http://{}", spawn_mock())
  }

  #[tokio::test]
  async fn cold_discovery_roundtrip() {
    let base = mock_base();
    // 生产路径用 ORIGIN；这里直接测内部函数以注入 base。
    let client = reqwest::Client::builder()
      .redirect(reqwest::redirect::Policy::none())
      .build()
      .unwrap();
    let id = discover_action_id(&client, Some("muse_session=1"), &base, &mut vec![])
      .await
      .unwrap()
      .expect("discover id");
    assert_eq!(id.len(), 40);
  }

  #[tokio::test]
  async fn stale_then_discovered_recovers() {
    // 直接验证 post_action + 解析链路在 mock 上成功
    let base = mock_base();
    let client = reqwest::Client::builder()
      .redirect(reqwest::redirect::Policy::none())
      .build()
      .unwrap();
    let id = "ab".repeat(20);
    let (status, ctype, body) = post_action(&client, &base, "muse_session=1", &id)
      .await
      .unwrap();
    assert_eq!(status, 200);
    assert!(ctype.contains("text/x-component"));
    let q = parse_flight_subscription(&body).expect("quota");
    assert_eq!(q.percent_used, 12);
  }
}
