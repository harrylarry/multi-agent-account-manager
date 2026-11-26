use std::{fs, io, path::Path};

// Upgrade cleanup for the removed feature. Every path is private to the app.
pub fn purge(app_data: &Path) -> io::Result<()> {
  for name in ["rooms-v1.json", "backup-restore-v1.json"] {
    remove_file(&app_data.join(name))?;
  }
  let files = match fs::read_dir(app_data) {
    Ok(files) => files,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
    Err(error) => return Err(error),
  };
  for file in files {
    let file = file?;
    let name = file.file_name();
    let Some(suffix) = name.to_str().and_then(|name| name.strip_prefix(".maam.tmp.")) else { continue; };
    if uuid::Uuid::parse_str(suffix).is_err() || !file.file_type()?.is_file() { continue; }
    let bytes = fs::read(file.path())?;
    // A truncated temporary file is never read by account storage. The
    // recognized archive and rollback schemas identify completed writes.
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else { continue; };
    let removed = (value.get("rooms").is_some_and(serde_json::Value::is_array) && value.get("customTemplates").is_some())
      || (value.get("previous_rooms").is_some() && value.get("id").is_some());
    if removed { remove_file(&file.path())?; }
  }
  Ok(())
}

fn remove_file(path: &Path) -> io::Result<()> {
  match fs::remove_file(path) {
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
    result => result,
  }
}

#[cfg(target_os = "ios")]
pub fn purge_temporary_shares() -> io::Result<()> {
  let root = fs::canonicalize(std::env::temp_dir())?;
  for file in fs::read_dir(&root)? {
    let file = file?;
    let name = file.file_name();
    let Some(suffix) = name.to_str().and_then(|name| name.strip_prefix("diehao-")) else { continue; };
    let kind = file.file_type()?;
    let path = file.path();
    if kind.is_dir() && uuid::Uuid::parse_str(suffix).is_ok() && path.parent() == Some(root.as_path()) {
      fs::remove_dir_all(path)?;
    } else if kind.is_file() && suffix.strip_suffix(".maambackup").is_some_and(|part| part.len() == 8 && part.bytes().all(|b| b.is_ascii_hexdigit())) {
      remove_file(&path)?;
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn cleanup_is_idempotent_and_preserves_account_data() {
    let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
    fs::create_dir(&dir).unwrap();
    for name in ["rooms-v1.json", "backup-restore-v1.json", "accounts.json", "send-ledger.json"] {
      fs::write(dir.join(name), b"local data").unwrap();
    }
    let stale = dir.join(format!(".maam.tmp.{}", uuid::Uuid::new_v4().simple()));
    fs::write(&stale, b"{\"rooms\":[],\"customTemplates\":[]}").unwrap();
    purge(&dir).unwrap();
    purge(&dir).unwrap();
    assert!(!stale.exists() && !dir.join("rooms-v1.json").exists() && !dir.join("backup-restore-v1.json").exists());
    assert_eq!(fs::read(dir.join("accounts.json")).unwrap(), b"local data");
    assert_eq!(fs::read(dir.join("send-ledger.json")).unwrap(), b"local data");
    fs::remove_dir_all(dir).unwrap();
  }
}
