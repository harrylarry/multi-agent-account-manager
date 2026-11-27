// 会话 cookie 的内存持有者（Spike #2 Cookie 桥）。
//
// 来源：iOS 上 Swift 从各账号 WKWebsiteDataStore（identifier = 账号 UUID）
// 取会话 cookie，经 Tauri 插件传给 Rust。约束（Issue #12）：
// - 只在内存里传递：不写盘、不进日志、不进图谱、不进诊断结果；
// - 用后 zeroize：GatewayCreds 建好 WS URL 后立即清零；
// - 这里只存 name/value，不存 domain/path/过期时间（控制面只需要 Cookie 头）。

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Zeroize)]
pub struct SessionCookie {
  pub name: String,
  pub value: String,
}

/// 一组会话 cookie（内存持有，用后 zeroize）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct SessionCookies {
  #[serde(default)]
  pub cookies: Vec<SessionCookie>,
}

impl SessionCookies {
  /// 从 `a=1; b=2` 形式的 Cookie 头解析（用于只拿到 header 字符串的调用点）。
  pub fn from_header(header: &str) -> Self {
    let cookies = header
      .split(';')
      .filter_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        let name = name.trim();
        if name.is_empty() {
          return None;
        }
        Some(SessionCookie {
          name: name.to_string(),
          value: value.trim().to_string(),
        })
      })
      .collect();
    Self { cookies }
  }

  pub fn is_empty(&self) -> bool {
    self.cookies.is_empty()
  }

  pub fn len(&self) -> usize {
    self.cookies.len()
  }

  /// Cookie 请求头值（`name=value; name=value`）。调用方只把它放进
  /// HTTPS 请求头，不记录、不持久化。
  pub fn cookie_header(&self) -> String {
    self
      .cookies
      .iter()
      .map(|c| format!("{}={}", c.name, c.value))
      .collect::<Vec<_>>()
      .join("; ")
  }

  /// 诊断用：只返回 cookie 名（绝不返回 value）。
  pub fn names(&self) -> Vec<&str> {
    self.cookies.iter().map(|c| c.name.as_str()).collect()
  }

}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn from_header_parses_pairs() {
    let c = SessionCookies::from_header("a=1; b=x=y; ;c=3");
    assert_eq!(c.len(), 3);
    assert_eq!(c.names(), vec!["a", "b", "c"]);
    assert_eq!(c.cookie_header(), "a=1; b=x=y; c=3");
    assert!(SessionCookies::from_header("").is_empty());
  }
}
