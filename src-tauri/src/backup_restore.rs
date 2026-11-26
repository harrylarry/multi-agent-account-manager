use super::RoomBook;
use crate::storage::{save_json_atomic, save_store_atomic, AccountsStoreFile};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use std::{fs, io::ErrorKind, path::Path};
use uuid::Uuid;

#[derive(Deserialize, Serialize)]
struct Journal {
  id: Uuid,
  previous_rooms: Option<Value>,
}

fn read_optional<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
  match fs::read(path) {
    Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|_| "恢复记录或本机资料损坏，已保留原文件".into()),
    Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
    Err(_) => Err("无法读取恢复记录或本机资料".into()),
  }
}

fn remove_optional(path: &Path) -> Result<(), String> {
  match fs::remove_file(path) {
    Ok(()) => Ok(()),
    Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
    Err(_) => Err("无法清理恢复记录，请检查本机存储后重试".into()),
  }
}

// Room reads run under RoomGate. The account file is the commit point, and
// regular account writes preserve its marker until the next bundled restore.
pub(super) fn recover(rooms: &Path) -> Result<(), String> {
  let journal_path = rooms.with_file_name("backup-restore-v1.json");
  let Some(journal) = read_optional::<Journal>(&journal_path)? else { return Ok(()); };
  let accounts = read_optional::<Value>(&rooms.with_file_name("accounts.json"))?;
  let committed = accounts.as_ref().and_then(|doc| doc.get("backupRestoreId"))
    .and_then(Value::as_str).and_then(|id| Uuid::parse_str(id).ok()) == Some(journal.id);
  if !committed {
    match journal.previous_rooms {
      Some(previous) => save_json_atomic(rooms, &previous)?,
      None => remove_optional(rooms)?,
    }
  }
  remove_optional(&journal_path)
}

pub(crate) fn commit(
  accounts: &Path, store: &mut AccountsStoreFile, rooms: &Path, book: &RoomBook,
) -> Result<(), String> {
  let journal_path = rooms.with_file_name("backup-restore-v1.json");
  let journal = Journal { id: Uuid::new_v4(), previous_rooms: read_optional(rooms)? };
  save_json_atomic(&journal_path, &journal)?;
  store.backup_restore_id = Some(journal.id);
  if let Err(error) = save_json_atomic(rooms, book).and_then(|_| save_store_atomic(accounts, store)) {
    recover(rooms).map_err(|_| format!("{error}。房间回滚待完成，检查本机存储后重新打开房间"))?;
    return Err(error);
  }
  // Both documents are already committed. A later room read retries cleanup.
  let _ = remove_optional(&journal_path);
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::rooms::{apply, Command};
  use serde_json::json;

  #[test]
  fn interrupted_restore_rolls_back_only_before_the_account_commit() {
    let dir = std::env::temp_dir().join(format!("maam-backup-{}", Uuid::new_v4()));
    let rooms = dir.join("rooms-v1.json");
    let accounts = dir.join("accounts.json");
    let journal_path = dir.join("backup-restore-v1.json");
    let old = json!({"version":1,"rooms":[],"customTemplates":[],"marker":"old"});
    let restored = json!({"version":1,"rooms":[],"customTemplates":[],"marker":"restored"});
    for (committed, had_previous) in [(false, true), (false, false), (true, true)] {
      let journal = Journal { id: Uuid::new_v4(), previous_rooms: had_previous.then(|| old.clone()) };
      let store = AccountsStoreFile { backup_restore_id: committed.then_some(journal.id), ..Default::default() };
      save_store_atomic(&accounts, &store).unwrap();
      save_json_atomic(&journal_path, &journal).unwrap();
      save_json_atomic(&rooms, &restored).unwrap();
      apply(&rooms, Command::List, &[], false).unwrap();
      let expected = if committed { Some(restored.clone()) } else { journal.previous_rooms };
      assert_eq!(read_optional::<Value>(&rooms).unwrap(), expected);
      assert!(!journal_path.exists());
    }
    fs::remove_dir_all(dir).unwrap();
  }

  #[test]
  fn successful_restore_persists_both_documents_and_clears_the_journal() {
    let dir = std::env::temp_dir().join(format!("maam-backup-{}", Uuid::new_v4()));
    let rooms = dir.join("rooms-v1.json");
    let accounts = dir.join("accounts.json");
    let book = apply(&rooms, Command::List, &[], false).unwrap();
    let mut store = AccountsStoreFile::default();
    commit(&accounts, &mut store, &rooms, &book).unwrap();
    assert!(store.backup_restore_id.is_some());
    assert_eq!(crate::storage::load_or_migrate_store(&accounts).store.backup_restore_id, store.backup_restore_id);
    assert!(rooms.is_file());
    assert!(!dir.join("backup-restore-v1.json").exists());
    fs::remove_dir_all(dir).unwrap();
  }
}
