const APP_COMMANDS: &[&str] = &[
  "get_manager_state",
  "select_provider",
  "add_account",
  "rename_account",
  "remove_account",
  "reorder_accounts",
  "switch_account",
  "set_modal_active",
  "open_official_site",
  "resolve_uuid_conflict",
  "dismiss_notice",
  "trigger_quota_sync",
  "check_for_updates",
  "download_and_install_update",
  "open_external_url",
];

fn main() {
  let attributes = tauri_build::Attributes::new()
    .app_manifest(tauri_build::AppManifest::new().commands(APP_COMMANDS));
  tauri_build::try_build(attributes).expect("failed to run tauri-build");
}
