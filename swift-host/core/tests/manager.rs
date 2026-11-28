use diehao_manager_core::read_manager;
use serde_json::json;
use std::fs;
use uuid::Uuid;

#[test]
fn reads_original_account_metadata_without_writes_or_proxy_credentials() {
    let directory = std::env::temp_dir().join(format!("diehao-manager-{}", Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    let file = directory.join("accounts.json");
    let account_id = "8915f479-4e7e-42b3-b313-83430010883d";
    let original = json!({
        "version": 2,
        "accounts": [{
            "providerId": "hark", "uuid": account_id, "alias": "测试 Hark",
            "createdAtMs": 1, "group": "测试组", "authState": "authenticated",
            "proxy": {
                "uri": "socks5://proxy.example:1080", "enabled": true,
                "username": "fixture-user", "credentialRef": "fixture-key"
            }
        }],
        "lastCommitted": { "providerId": "hark", "uuid": account_id },
        "pendingCleanup": [], "accountGroups": { "hark": ["原分组"] },
    }).to_string();
    fs::write(&file, &original).unwrap();
    let result = read_manager(&file).unwrap();
    let account = &result["accounts"][0];
    assert_eq!(account["uuid"], account_id);
    assert_eq!(account["alias"], "测试 Hark");
    assert_eq!(account["authState"], "authenticated");
    assert_eq!(account["proxy"], json!({ "enabled": true, "poolEntryId": null }));
    assert_eq!(result["selectedProviderId"], "hark");
    assert_eq!(result["accountGroups"]["hark"], json!(["原分组", "测试组"]));
    assert_eq!(fs::read_to_string(&file).unwrap(), original);
    fs::write(&file, "broken").unwrap();
    assert!(read_manager(&file).is_err());
    assert_eq!(fs::read_to_string(&file).unwrap(), "broken");
    fs::remove_file(&file).unwrap();
    assert_eq!(read_manager(&file).unwrap()["accounts"], json!([]));
    fs::remove_dir(directory).unwrap();
}
