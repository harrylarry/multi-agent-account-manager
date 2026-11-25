use serde::{Deserialize, Serialize};

const MAX_INPUT: usize = 256 * 1024;
const MAX_COOKIES: usize = 256;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountCookie {
  pub name: String,
  pub value: String,
  pub domain: String,
  pub path: String,
  #[serde(default)]
  pub secure: bool,
  #[serde(default)]
  pub http_only: bool,
  pub expiration_date: Option<f64>,
  pub same_site: Option<String>,
}

impl Drop for AccountCookie {
  fn drop(&mut self) {
    use zeroize::Zeroize;
    self.value.zeroize();
  }
}

fn cookie_domains(provider: &str) -> Option<(&'static str, &'static [&'static str])> {
  match provider {
    "muse" => Some(("muse.ai", &["muse.ai", "meta.com"])),
    "hark" => Some(("app.hark.com", &["hark.com"])),
    _ => None,
  }
}

pub fn parse(raw: &str, provider: &str, now: f64) -> Result<Vec<AccountCookie>, String> {
  let invalid = || "Cookie 格式或属性无效".to_string();
  let (default_host, domains) = cookie_domains(provider).ok_or_else(invalid)?;
  if raw.len() > MAX_INPUT {
    return Err("Cookie 输入过大".into());
  }
  let raw = raw.trim_matches([' ', '\r', '\n']);
  let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
  if raw.trim().is_empty() {
    return Err("请先粘贴要预览的 Cookie。下方预览只检查输入框里的内容。".into());
  }
  let cookies = if raw.starts_with('[') || raw.starts_with('{') {
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|_| invalid())?;
    let list = if value.is_array() {
      value
    } else {
      value.get("cookies").cloned().ok_or_else(invalid)?
    };
    serde_json::from_value(list).map_err(|_| invalid())?
  } else if raw.contains('\t') {
    raw.lines().filter(|line| !line.trim().is_empty() && (!line.starts_with('#') || line.starts_with("#HttpOnly_")))
      .map(|line| {
        let (http_only, line) = line.strip_prefix("#HttpOnly_").map_or((false, line), |line| (true, line));
        let fields: Vec<_> = line.split('\t').collect();
        let [domain, include_subdomains, path, secure, expiry, name, value] = fields.as_slice() else {
          return Err(invalid());
        };
        let subdomains = flag(include_subdomains).ok_or_else(invalid)?;
        let expiry = expiry.parse::<i64>().map_err(|_| invalid())?;
        if expiry < 0 { return Err(invalid()); }
        Ok(AccountCookie {
          domain: if subdomains { format!(".{}", domain.trim_start_matches('.')) } else { domain.trim_start_matches('.').into() },
          path: (*path).into(), name: (*name).into(), value: (*value).into(),
          secure: flag(secure).ok_or_else(invalid)?, http_only, same_site: None,
          expiration_date: (expiry != 0).then_some(expiry as f64),
        })
      }).collect::<Result<Vec<_>, _>>()?
  } else {
    if raw.contains(['\r', '\n']) { return Err(invalid()); }
    let header = raw.get(..7).filter(|prefix| prefix.eq_ignore_ascii_case("cookie:"))
      .map_or(raw, |_| &raw[7..]);
    header.trim().trim_end_matches(';').split(';').map(|pair| {
      let (name, value) = pair.trim().split_once('=').ok_or_else(invalid)?;
      Ok(AccountCookie {
        name: name.into(), value: value.into(), domain: default_host.into(), path: "/".into(),
        secure: true, http_only: true, expiration_date: None, same_site: None,
      })
    }).collect::<Result<Vec<_>, String>>()?
  };
  if cookies.is_empty() || cookies.len() > MAX_COOKIES { return Err(invalid()); }

  let mut dedup = std::collections::HashSet::new();
  let mut cookies = cookies;
  for cookie in &mut cookies {
    cookie.domain.make_ascii_lowercase();
    let host = cookie.domain.strip_prefix('.').unwrap_or(&cookie.domain);
    if !domains.iter().any(|root| host == *root || host.strip_suffix(root).is_some_and(|prefix| prefix.ends_with('.')))
      || !host.split('.').all(|label| !label.is_empty() && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
    {
      return Err("Cookie 包含当前服务范围之外的域名".into());
    }
    if (cookie.name.starts_with("__Secure-") && !cookie.secure)
      || (cookie.name.starts_with("__Host-") && (!cookie.secure || cookie.domain.starts_with('.') || cookie.path != "/"))
      || cookie.name.is_empty() || cookie.name.len() + cookie.value.len() > 4096
      || !cookie.name.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
      || !cookie.value.bytes().all(|b| matches!(b, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e))
      || !cookie.path.starts_with('/') || cookie.path.chars().any(|c| c.is_control() || c == ';')
      || !dedup.insert((cookie.name.clone(), host.to_string(), cookie.path.clone()))
    {
      return Err(invalid());
    }
    if cookie.expiration_date.is_some_and(|expires| expires <= now) {
      return Err("输入含已过期的 Cookie，请重新导出后再试".into());
    }
    if cookie.expiration_date.is_some_and(|expires| !expires.is_finite() || expires > 253402300799.0) {
      return Err("Cookie 到期时间无效，JSON 和 Netscape 应使用 Unix 秒".into());
    }
    if let Some(value) = &mut cookie.same_site {
      *value = match value.to_ascii_lowercase().as_str() {
        "strict" => "Strict",
        "lax" => "Lax",
        "none" | "no_restriction" if cookie.secure => "None",
        "unspecified" => { cookie.same_site = None; continue; }
        _ => return Err(invalid()),
      }.into();
    }
  }
  Ok(cookies)
}

fn flag(value: &str) -> Option<bool> {
  match value.to_ascii_lowercase().as_str() {
    "true" => Some(true),
    "false" => Some(false),
    _ => None,
  }
}

#[cfg(any(target_os = "ios", test))]
pub fn prepare_export(
  cookies: Vec<AccountCookie>, provider: &str, now: f64,
) -> Result<(zeroize::Zeroizing<String>, usize, usize), String> {
  let mut supported = Vec::new();
  let mut seen = std::collections::HashSet::new();
  let count = cookies.len();
  for mut cookie in cookies {
    // WebKit may explicitly report an unset policy.
    if cookie.same_site.as_deref().is_some_and(|site| site.is_empty() || site.eq_ignore_ascii_case("default")) {
      cookie.same_site = None;
    }
    let entry = zeroize::Zeroizing::new(serde_json::to_string(&[cookie]).map_err(|_| "无法编码本机 Cookie")?);
    let Ok(mut validated) = parse(&entry, provider, now) else { continue; };
    let Some(cookie) = validated.pop() else { continue; };
    if !seen.insert((cookie.name.clone(), cookie.domain.trim_start_matches('.').to_owned(), cookie.path.clone())) {
      continue;
    }
    supported.push(cookie);
  }
  let written = supported.len();
  if written == 0 {
    return Err("没有可安全导出并重新导入的 Cookie，请打开账号检查登录".into());
  }
  let text = zeroize::Zeroizing::new(serde_json::to_string(&supported).map_err(|_| "无法编码本机 Cookie")?);
  parse(&text, provider, now).map_err(|error| format!("Cookie 导出未复制：{error}"))?;
  Ok((text, written, count - written))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn supports_the_three_formats_and_rejects_invalid_scope() {
    let header = parse("Cookie: session=sample; other=a=b", "muse", 100.0).unwrap();
    assert_eq!(header.len(), 2);
    assert_eq!(header[1].value, "a=b");
    let file = "# Netscape\n#HttpOnly_.muse.ai\tTRUE\t/\tTRUE\t200\tsession\tsample";
    assert!(parse(file, "muse", 100.0).unwrap()[0].http_only);
    assert!(parse(file, "muse", 300.0).is_err());
    assert!(parse(file, "hark", 100.0).is_err());
    let json = r#"[{"domain":"app.hark.com","name":"session","value":"abc","path":"/","secure":true,"sameSite":"lax"}]"#;
    assert_eq!(parse(json, "hark", 100.0).unwrap()[0].same_site.as_deref(), Some("Lax"));
    assert!(parse("bad=x; bad=y", "hark", 100.0).is_err());
    assert!(parse("bad=x\nSet-Cookie: injected=y", "hark", 100.0).is_err());
  }

  #[test]
  fn distinguishes_empty_and_expired_input_from_invalid_attributes() {
    let error = |raw| parse(raw, "muse", 300.0).err().unwrap();
    assert!(error("   ").contains("先粘贴"));
    assert!(error(".muse.ai\tTRUE\t/\tTRUE\t200\tsession\tsample").contains("已过期"));
    assert!(error("broken").contains("格式"));
    assert!(parse("\u{feff}Cookie: session=sample", "muse", 300.0).is_ok());
  }

  #[test]
  fn exported_webkit_rows_pass_the_same_import_parser() {
    let rows = serde_json::from_str(r#"[
      {"domain":"muse.ai","name":"session","value":"abc","path":"/","secure":true,"sameSite":"Default"},
      {"domain":".muse.ai","name":"session","value":"duplicate","path":"/"},
      {"domain":"muse.ai","name":"invalid","value":"a;b","path":"/"},
      {"domain":"muse.ai","name":"expired","value":"abc","path":"/","expirationDate":20},
      {"domain":"muse.ai","name":"other","value":"def","path":"/","secure":true,"sameSite":"none"}
    ]"#).unwrap();
    let (export, written, skipped) = prepare_export(rows, "muse", 100.0).unwrap();
    assert_eq!((written, skipped), (2, 3));
    assert_eq!(parse(&export, "muse", 100.0).unwrap().len(), 2);
  }
}
