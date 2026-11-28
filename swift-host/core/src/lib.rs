//! Read-only adapter to the original account store. Tauri's migration writer is not called.
#[allow(dead_code)]
#[path = "../../../src-tauri/src/providers.rs"]
mod providers;
#[allow(dead_code)]
#[path = "../../../src-tauri/src/storage.rs"]
mod storage;

use serde_json::{json, Value};
use std::ffi::{c_char, CStr, CString};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::ptr;

pub fn read_manager(path: &Path) -> Result<Value, String> {
    let (store, _) = match fs::read(path) {
        Ok(bytes) => storage::migrate_from_value(serde_json::from_slice(&bytes).map_err(|e| e.to_string())?)?,
        Err(error) if error.kind() == ErrorKind::NotFound => (storage::AccountsStoreFile::default(), false),
        Err(error) => return Err(error.to_string()),
    };
    let conflicts = storage::detect_conflicts(&store.accounts);
    let last_committed = store.last_committed.filter(|_| conflicts.is_empty());
    let selected_provider = last_committed.as_ref().map(|key| key.provider_id).unwrap_or(providers::ProviderId::Muse);
    let mut groups = store.account_groups;
    let mut accounts = Vec::with_capacity(store.accounts.len());
    for account in store.accounts {
        if let Some(group) = &account.group {
            let provider_groups = groups.entry(account.provider_id.as_str().to_owned()).or_default();
            if !provider_groups.contains(group) {
                provider_groups.push(group.clone());
            }
        }
        let mut row = serde_json::to_value(&account).map_err(|e| e.to_string())?;
        row["proxy"] = account.proxy.as_ref()
            .map(|proxy| json!({ "enabled": proxy.enabled, "poolEntryId": proxy.pool_entry_id }))
            .unwrap_or(Value::Null);
        accounts.push(row);
    }
    for names in groups.values_mut() {
        names.sort();
    }
    Ok(json!({
        "platform": "ios", "providers": providers::all_provider_infos(), "accounts": accounts, "accountGroups": groups,
        "selectedProviderId": selected_provider, "activeAccount": null, "switchingAccount": null, "warmAccounts": [],
        "lastCommitted": last_committed, "pendingCleanup": store.pending_cleanup, "conflicts": conflicts, "notice": null,
        "modalActive": false,
    }))
}

/// # Safety
/// The path must be a valid NUL-terminated string for this call.
#[no_mangle]
pub unsafe extern "C" fn diehao_manager_snapshot(path: *const c_char) -> *mut c_char {
    if path.is_null() {
        return ptr::null_mut();
    }
    let Some(snapshot) = CStr::from_ptr(path).to_str().ok()
        .and_then(|path| read_manager(Path::new(path)).ok()) else { return ptr::null_mut() };
    CString::new(snapshot.to_string()).map(CString::into_raw).unwrap_or(ptr::null_mut())
}

/// # Safety
/// The pointer must be null or an unfreed result of diehao_manager_snapshot.
#[no_mangle]
pub unsafe extern "C" fn diehao_manager_snapshot_free(bytes: *mut c_char) {
    if !bytes.is_null() {
        drop(CString::from_raw(bytes));
    }
}
