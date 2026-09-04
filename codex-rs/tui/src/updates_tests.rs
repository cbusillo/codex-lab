use super::*;
use crate::legacy_core::config::ConfigBuilder;
use chrono::Duration;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

#[tokio::test]
async fn codex_lab_ignores_cached_upstream_version() {
    let codex_home = tempdir().expect("temp codex home");
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await
        .expect("load config");
    let version_file = version_filepath(&config);
    let upstream_info = VersionInfo {
        latest_version: "0.149.0".to_string(),
        last_checked_at: Utc::now() + Duration::hours(1),
        dismissed_version: None,
    };
    std::fs::write(
        &version_file,
        format!(
            "{}\n",
            serde_json::to_string(&upstream_info).expect("serialize version info")
        ),
    )
    .expect("write version cache");

    assert_eq!(
        (
            get_upgrade_version_for_build(&config, /*is_lab_build*/ true, "0.1.0"),
            get_upgrade_version_for_popup_for_build(&config, /*is_lab_build*/ true, "0.1.0",),
            get_upgrade_version_for_build(&config, /*is_lab_build*/ false, "0.1.0"),
            get_upgrade_version_for_popup_for_build(&config, /*is_lab_build*/ false, "0.1.0",),
        ),
        (
            None,
            None,
            Some("0.149.0".to_string()),
            Some("0.149.0".to_string()),
        )
    );
}
