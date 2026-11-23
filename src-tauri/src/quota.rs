//! B3：额度读取（Issue #13 / i005 #7）。
//!
//! `fetchSubscriptionAction` 不在 Noise 网关上，而是 muse.ai 的 Next.js Server Action：
//! `POST https://muse.ai/`，头 `Next-Action: <40–44 hex>`、`Accept: text/x-component`，
//! body `[{"includeAgreement":false}]`；响应是 `N:{"subscription":...}` 形式的多行体。
//! hash 会随前端发版轮换，需要从页面/JS 里发现；失败再走 REST 兜底。
//!
//! 只读：本模块不写任何账号数据，也不记录 cookie。

use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

const ORIGIN: &str = "https://muse.ai";
/// BotArk `docs/museai/QUOTA.md` 记录的已知 action id（可能随发版轮换，仅作首个候选）。
const FALLBACK_ACTION_ID: &str = "409b972cbb33d3e372f4ca9d3c8332677217064f62";
/// 与网关一致的 UA：部分 Server Action 会校验来源浏览器。
const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Safari/537.36";

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaSnapshot {
  pub tier: String,
  pub percent_used: i64,
  pub state: String,
  pub resets_at_ms: i64,
  pub source: String,
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

/// 从 `{subscription:{tier,usage,...}}` 或裸对象里取额度；取不到返回 None。
pub fn parse_subscription(v: &Value) -> Option<QuotaSnapshot> {
  let root = v.get("result").unwrap_or(v);
  let sub = match root.get("subscription") {
    Some(x) => x,
    None => root,
  };
  let tier = sub.get("tier").cloned().unwrap_or(Value::Null);
  let usage = sub.get("usage").cloned().unwrap_or(Value::Null);
  let tier_name = if tier.is_object() {
    s(&tier, &["name", "tierId", "tier_id", "tierCode", "tier_code"])
  } else {
    String::new()
  };
  let pct = i(&usage, &["percentUsed", "percent_used"]).unwrap_or(-1);
  let state = s(&usage, &["state", "quotaStatus", "quota_status"]);
  let resets = i(&usage, &["resetsAt", "resets_at"]).unwrap_or(0);
  if tier_name.is_empty() && pct < 0 && state.is_empty() {
    return None;
  }
  Some(QuotaSnapshot {
    tier: tier_name,
    percent_used: if pct < 0 { 0 } else { pct.clamp(0, 100) },
    state,
    resets_at_ms: resets,
    source: "action".to_string(),
  })
}

/// 解析 Server Action 的 `text/x-component` 多行体（可带 `N:` 前缀）。
pub fn parse_action_body(text: &str) -> Option<QuotaSnapshot> {
  for line in text.lines() {
    let l = line.trim();
    if l.is_empty() {
      continue;
    }
    let json_part = match l.find(':') {
      Some(idx) if l[..idx].chars().all(|c| c.is_ascii_digit()) => &l[idx + 1..],
      _ => l,
    };
    if let Ok(v) = serde_json::from_str::<Value>(json_part) {
      if let Some(q) = parse_subscription(&v) {
        return Some(q);
      }
    }
  }
  None
}

/// 在 JS/HTML 文本里找 `createServerReference("<hex>")` 附近出现
/// `fetchSubscriptionAction` 的 action id。
pub fn find_action_id(text: &str) -> Option<String> {
  const NEEDLE: &str = "createServerReference(";
  let mut from = 0usize;
  while let Some(p) = text[from..].find(NEEDLE) {
    let start = from + p + NEEDLE.len();
    let rest = text[start..].trim_start().trim_start_matches(['"', '\'']);
    let hex: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    if (40..=44).contains(&hex.len()) {
      let lo = start.saturating_sub(200);
      let hi = (start + hex.len() + 200).min(text.len());
      if text[lo..hi].contains("fetchSubscriptionAction") {
        return Some(hex);
      }
    }
    from = start;
  }
  // 兜底：任意位置出现 fetchSubscriptionAction，在其前后 120 字符里找 40–44 位 hex。
  if let Some(p) = text.find("fetchSubscriptionAction") {
    let lo = p.saturating_sub(120);
    let hi = (p + 120).min(text.len());
    let window = &text[lo..hi];
    let bytes = window.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
      if bytes[i].is_ascii_hexdigit() {
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
          i += 1;
        }
        let run = &window[start..i];
        let all_digits_and_hex_ok = run.chars().all(|c| c.is_ascii_hexdigit());
        if all_digits_and_hex_ok && (40..=44).contains(&run.len()) {
          return Some(run.to_string());
        }
      } else {
        i += 1;
      }
    }
  }
  None
}

async fn post_action(
  client: &reqwest::Client,
  cookie: &str,
  id: &str,
) -> Result<Option<QuotaSnapshot>, String> {
  let resp = client
    .post(format!("{ORIGIN}/"))
    .header("Accept", "text/x-component")
    .header("Content-Type", "text/plain;charset=UTF-8")
    .header("Next-Action", id)
    .header("Origin", ORIGIN)
    .header("Referer", format!("{ORIGIN}/"))
    .header("Cookie", cookie)
    .header("User-Agent", UA)
    .body(r#"[{"includeAgreement":false}]"#)
    .send()
    .await
    .map_err(|e| e.to_string())?;
  let status = resp.status().as_u16();
  let text = resp.text().await.map_err(|e| e.to_string())?;
  if status >= 400 {
    return Ok(None);
  }
  Ok(parse_action_body(&text))
}

async fn get_text(
  client: &reqwest::Client,
  cookie: &str,
  url: &str,
  accept: &str,
) -> Result<String, String> {
  let resp = client
    .get(url)
    .header("Accept", accept)
    .header("Accept-Language", "en-US")
    .header("Cookie", cookie)
    .header("User-Agent", UA)
    .header("Origin", ORIGIN)
    .header("Referer", format!("{ORIGIN}/"))
    .send()
    .await
    .map_err(|e| e.to_string())?;
  if !resp.status().is_success() {
    return Err(format!("HTTP {}", resp.status().as_u16()));
  }
  resp.text().await.map_err(|e| e.to_string())
}

fn script_urls(html: &str) -> Vec<String> {
  let mut out = Vec::new();
  let mut from = 0usize;
  while let Some(p) = html[from..].find("<script") {
    let start = from + p;
    let end = (start + 600).min(html.len());
    let tag = &html[start..end];
    if let Some(sp) = tag.find("src=") {
      let after = tag[sp + 4..].trim_start();
      if let Some(q) = after.chars().next() {
        if q == '"' || q == '\'' {
          if let Some(close) = after[1..].find(q) {
            let src = &after[1..1 + close];
            if src.contains(".js") {
              out.push(if src.starts_with("http") {
                src.to_string()
              } else {
                format!("{ORIGIN}{}", if src.starts_with('/') { src.to_string() } else { format!("/{src}") })
              });
            }
          }
        }
      }
    }
    from = start + 8;
  }
  out.dedup();
  out
}

async fn discover_action_id(client: &reqwest::Client, cookie: &str) -> Option<String> {
  let mut scripts: Vec<String> = Vec::new();
  for path in ["/", "/chat", "/settings"] {
    if let Ok(html) = get_text(client, cookie, &format!("{ORIGIN}{path}"), "text/html").await {
      if let Some(id) = find_action_id(&html) {
        return Some(id);
      }
      scripts.extend(script_urls(&html));
    }
  }
  for url in scripts.into_iter().take(24) {
    if let Ok(js) = get_text(client, cookie, &url, "*/*").await {
      if let Some(id) = find_action_id(&js) {
        return Some(id);
      }
    }
  }
  None
}

/// 取一次额度：先试缓存/发现的 Server Action，再试 REST 兜底。
pub async fn fetch_quota(cookie: &str, cached_action: Option<&str>) -> Result<QuotaSnapshot, String> {
  let client = reqwest::Client::builder()
    .timeout(Duration::from_secs(20))
    .build()
    .map_err(|e| e.to_string())?;
  let mut tried: Vec<String> = Vec::new();

  let mut candidates: Vec<String> = Vec::new();
  if let Some(id) = cached_action {
    candidates.push(id.to_string());
  }
  candidates.push(FALLBACK_ACTION_ID.to_string());

  for id in &candidates {
    match post_action(&client, cookie, id).await {
      Ok(Some(q)) => return Ok(q),
      Ok(None) => tried.push(format!("action {}:no-json", &id[..8.min(id.len())])),
      Err(e) => tried.push(format!("action {}:{}", &id[..8.min(id.len())], e)),
    }
  }

  match discover_action_id(&client, cookie).await {
    Some(id) => {
      if !candidates.contains(&id) {
        match post_action(&client, cookie, &id).await {
          Ok(Some(q)) => return Ok(q),
          Ok(None) => tried.push("discovered:no-json".to_string()),
          Err(e) => tried.push(format!("discovered:{e}")),
        }
      }
    }
    None => tried.push("discover:none".to_string()),
  }

  for ep in ["/api/subscription", "/api/hatch/subscription", "/api/billing/subscription"] {
    match get_text(&client, cookie, &format!("{ORIGIN}{ep}"), "application/json").await {
      Ok(body) => {
        if let Ok(v) = serde_json::from_str::<Value>(&body) {
          if let Some(mut q) = parse_subscription(&v) {
            q.source = "rest".to_string();
            return Ok(q);
          }
          tried.push(format!("rest {ep}:no-quota"));
        } else {
          tried.push(format!("rest {ep}:not-json"));
        }
      }
      Err(e) => tried.push(format!("rest {ep}:{e}")),
    }
  }

  Err(format!("quota unavailable（{}）", tried.join("；")))
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  #[test]
  fn parses_action_body_with_prefix() {
    let text = r#"2:{"success":true,"subscription":{"tier":{"name":"Pro"},"usage":{"percentUsed":42,"state":"OK","resetsAt":1762100300000}}}"#;
    let q = parse_action_body(text).expect("quota");
    assert_eq!(q.tier, "Pro");
    assert_eq!(q.percent_used, 42);
    assert_eq!(q.resets_at_ms, 1762100300000);
  }

  #[test]
  fn parses_nested_result_and_snake_case() {
    let v = json!({"result":{"subscription":{"tier":{"tier_code":"free"},"usage":{"percent_used":7,"quota_status":"LOW"}}}});
    let q = parse_subscription(&v).expect("quota");
    assert_eq!(q.tier, "free");
    assert_eq!(q.percent_used, 7);
    assert_eq!(q.state, "LOW");
  }

  #[test]
  fn ignores_unrelated_json() {
    assert!(parse_action_body(r#"{"ok":true}"#).is_none());
    assert!(parse_action_body("not json").is_none());
  }

  #[test]
  fn finds_action_id_near_name() {
    let hex = "a".repeat(40);
    let js = format!(r#"x=createServerReference("{hex}","fetchSubscriptionAction");"#);
    assert_eq!(find_action_id(&js), Some(hex.clone()));
    let far = format!(r#"createServerReference("{hex}");{}{}"#, "x".repeat(400), "fetchSubscriptionAction");
    assert_eq!(find_action_id(&far), None);
  }

  #[test]
  fn finds_action_id_loose_pattern() {
    let hex = "b".repeat(42);
    let js = format!(r#"minified.fetchSubscriptionAction,{hex},other,"#);
    assert_eq!(find_action_id(&js), Some(hex));
    assert!(find_action_id("no hex here fetchSubscriptionAction").is_none());
  }

  #[test]
  fn extracts_script_urls() {
    let html = r#"<script src="/_next/a.js"></script><script src='https://muse.ai/b.js'></script>"#;
    let urls = script_urls(html);
    assert_eq!(urls.len(), 2);
    assert!(urls[0].starts_with("https://muse.ai/"));
  }
}
