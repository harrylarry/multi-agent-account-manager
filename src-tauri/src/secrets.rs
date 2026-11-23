//! 敏感凭据存储（#11/#16）。
//!
//! 正式平台（iOS / macOS）写入系统 Keychain：`ProxySetting` 只保存一条
//! `credential_ref`（Keychain account 名），真实口令绝不落到 `accounts.json`。
//!
//! 非 Apple 桌面平台（Linux / Windows）没有统一的系统凭据库绑定，这里降级为
//! 应用数据目录下的受限文件（0600）。这是**开发/非正式环境**的可运行兜底，
//! 不等同于 Keychain 的安全保证；正式发布前必须在目标平台接入系统凭据库。

/// Keychain 条目的 service 命名空间。
#[allow(dead_code)] // 仅 Apple 平台分支使用
pub const SERVICE: &str = "com.harrylarry.multi-agent-account-manager.proxy";

pub struct Secrets {
  #[cfg(not(any(target_os = "ios", target_os = "macos")))]
  dir: std::path::PathBuf,
}

impl Secrets {
  pub fn new(app_data_dir: &std::path::Path) -> Self {
    #[cfg(any(target_os = "ios", target_os = "macos"))]
    {
      let _ = app_data_dir;
      Self {}
    }
    #[cfg(not(any(target_os = "ios", target_os = "macos")))]
    {
      let dir = app_data_dir.join("secrets");
      let _ = std::fs::create_dir_all(&dir);
      Self { dir }
    }
  }

  pub fn set(&self, account: &str, secret: &str) -> Result<(), String> {
    #[cfg(any(target_os = "ios", target_os = "macos"))]
    {
      imp::set(account, secret)
    }
    #[cfg(not(any(target_os = "ios", target_os = "macos")))]
    {
      imp::set(&self.dir, account, secret)
    }
  }

  pub fn get(&self, account: &str) -> Result<Option<String>, String> {
    #[cfg(any(target_os = "ios", target_os = "macos"))]
    {
      imp::get(account)
    }
    #[cfg(not(any(target_os = "ios", target_os = "macos")))]
    {
      imp::get(&self.dir, account)
    }
  }

  pub fn delete(&self, account: &str) -> Result<(), String> {
    #[cfg(any(target_os = "ios", target_os = "macos"))]
    {
      imp::delete(account)
    }
    #[cfg(not(any(target_os = "ios", target_os = "macos")))]
    {
      imp::delete(&self.dir, account)
    }
  }
}

#[cfg(any(target_os = "ios", target_os = "macos"))]
mod imp {
  use super::SERVICE;
  use security_framework::passwords::{
    delete_generic_password, generic_password, set_generic_password,
  };

  /// `errSecItemNotFound`（Security.framework）。
  const ERR_SEC_ITEM_NOT_FOUND: isize = -25300;

  pub fn set(account: &str, secret: &str) -> Result<(), String> {
    set_generic_password(SERVICE, account, secret.as_bytes())
      .map_err(|e| format!("写入 Keychain 失败: {e}"))
  }

  pub fn get(account: &str) -> Result<Option<String>, String> {
    match generic_password(SERVICE, account) {
      Ok(bytes) => String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "Keychain 返回值不是合法 UTF-8".to_string()),
      Err(e) if e.code() as isize == ERR_SEC_ITEM_NOT_FOUND => Ok(None),
      Err(e) => Err(format!("读取 Keychain 失败: {e}")),
    }
  }

  pub fn delete(account: &str) -> Result<(), String> {
    match delete_generic_password(SERVICE, account) {
      Ok(()) => Ok(()),
      Err(e) if e.code() as isize == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
      Err(e) => Err(format!("删除 Keychain 条目失败: {e}")),
    }
  }
}

#[cfg(not(any(target_os = "ios", target_os = "macos")))]
mod imp {
  use std::io::Write;
  use std::path::{Path, PathBuf};

  /// 账号名转文件名：逐字节 hex，避免路径穿越与特殊字符。
  fn file_name(account: &str) -> String {
    let mut out = String::with_capacity(account.len() * 2);
    for b in account.as_bytes() {
      out.push_str(&format!("{b:02x}"));
    }
    out
  }

  fn path(dir: &Path, account: &str) -> PathBuf {
    dir.join(format!("{}.bin", file_name(account)))
  }

  pub fn set(dir: &Path, account: &str, secret: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建凭据目录失败: {e}"))?;
    let p = path(dir, account);
    let mut f = std::fs::File::create(&p).map_err(|e| format!("写入凭据失败: {e}"))?;
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt;
      let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
    }
    f.write_all(secret.as_bytes())
      .map_err(|e| format!("写入凭据失败: {e}"))
  }

  pub fn get(dir: &Path, account: &str) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path(dir, account)) {
      Ok(s) => Ok(Some(s)),
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
      Err(e) => Err(format!("读取凭据失败: {e}")),
    }
  }

  pub fn delete(dir: &Path, account: &str) -> Result<(), String> {
    match std::fs::remove_file(path(dir, account)) {
      Ok(()) => Ok(()),
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
      Err(e) => Err(format!("删除凭据失败: {e}")),
    }
  }
}

#[cfg(all(test, not(any(target_os = "ios", target_os = "macos"))))]
mod tests {
  use super::*;

  #[test]
  fn file_backend_roundtrip_and_delete() {
    let dir = std::env::temp_dir().join(format!("secrets-test-{}", uuid::Uuid::new_v4()));
    let s = Secrets::new(&dir);
    assert_eq!(s.get("acct/../x").unwrap(), None);
    s.set("acct/../x", "p@ss:w0rd").unwrap();
    assert_eq!(s.get("acct/../x").unwrap().as_deref(), Some("p@ss:w0rd"));
    s.delete("acct/../x").unwrap();
    assert_eq!(s.get("acct/../x").unwrap(), None);
    // 幂等删除
    s.delete("acct/../x").unwrap();
    let _ = std::fs::remove_dir_all(&dir);
  }
}
