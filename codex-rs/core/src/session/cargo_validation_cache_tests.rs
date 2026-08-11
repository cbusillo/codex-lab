use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::time::Duration;

use tempfile::TempDir;
use tempfile::tempdir;
use tokio::time::timeout;

use super::super::cargo_validation_cache_key::cargo_cache_environment;
use super::super::cargo_validation_cache_key::cargo_toolchain_allows_success_suppression;
use super::super::cargo_validation_cache_key::cargo_toolchain_identity;
use super::super::cargo_validation_cache_key::cargo_validation_environment;
use super::*;

fn absolute_root(root: &TempDir) -> AbsolutePathBuf {
    AbsolutePathBuf::try_from(root.path().to_path_buf()).expect("temporary root should be absolute")
}

fn cache_key(
    repository: &TempDir,
    toolchain: &str,
    target: &str,
    command: &[&str],
) -> CargoValidationCacheKey {
    CargoValidationCacheKey::new(
        repository.path(),
        repository.path(),
        toolchain,
        target,
        &command.iter().map(ToString::to_string).collect::<Vec<_>>(),
        &BTreeMap::new(),
    )
}

fn test_limits() -> CargoValidationCacheLimits {
    CargoValidationCacheLimits {
        max_entries: 4,
        max_entry_bytes: 1024,
        max_total_bytes: 2048,
        max_files_per_entry: 16,
    }
}

#[test]
fn cache_root_rejects_codex_home_inside_concrete_checkout_before_creating_cache() {
    let repository = tempdir().expect("create shared repository root");
    let checkout = tempdir().expect("create linked checkout");
    let codex_home = checkout.path().join(".codex-lab");
    fs::create_dir(&codex_home).expect("create nested codex home");
    let codex_home = AbsolutePathBuf::try_from(codex_home).expect("codex home should be absolute");

    let error = cache_root(&codex_home, repository.path(), checkout.path())
        .expect_err("cache root inside linked checkout should be rejected");

    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(!codex_home.join("cache").as_path().exists());
}

async fn acquire(
    home: &TempDir,
    repository: &TempDir,
    key: CargoValidationCacheKey,
    cancellation: &CancellationToken,
    limits: CargoValidationCacheLimits,
) -> CargoValidationCacheLease {
    CargoValidationCacheLease::acquire_with_limits(
        &absolute_root(home),
        repository.path(),
        repository.path(),
        key,
        cancellation,
        limits,
    )
    .await
    .expect("cache acquisition should succeed")
    .expect("cache acquisition should not be cancelled")
}

#[test]
fn key_changes_with_repository_checkout_toolchain_target_command_and_environment() {
    let first_repository = tempdir().expect("create first repository");
    let second_repository = tempdir().expect("create second repository");
    let second_checkout = tempdir().expect("create second checkout");
    let command = vec!["cargo".to_string(), "check".to_string()];
    let mut environment = BTreeMap::new();
    environment.insert("RUSTFLAGS".to_string(), "-Cdebuginfo=0".to_string());
    let baseline = CargoValidationCacheKey::new(
        first_repository.path(),
        first_repository.path(),
        "1.95.0",
        "aarch64-apple-darwin",
        &command,
        &environment,
    );
    let keys = [
        baseline,
        CargoValidationCacheKey::new(
            second_repository.path(),
            first_repository.path(),
            "1.95.0",
            "aarch64-apple-darwin",
            &command,
            &environment,
        ),
        CargoValidationCacheKey::new(
            first_repository.path(),
            second_checkout.path(),
            "1.95.0",
            "aarch64-apple-darwin",
            &command,
            &environment,
        ),
        CargoValidationCacheKey::new(
            first_repository.path(),
            first_repository.path(),
            "stable",
            "aarch64-apple-darwin",
            &command,
            &environment,
        ),
        CargoValidationCacheKey::new(
            first_repository.path(),
            first_repository.path(),
            "1.95.0",
            "x86_64-unknown-linux-gnu",
            &command,
            &environment,
        ),
        CargoValidationCacheKey::new(
            first_repository.path(),
            first_repository.path(),
            "1.95.0",
            "aarch64-apple-darwin",
            &[
                "cargo".to_string(),
                "check".to_string(),
                "--tests".to_string(),
            ],
            &environment,
        ),
        CargoValidationCacheKey::new(
            first_repository.path(),
            first_repository.path(),
            "1.95.0",
            "aarch64-apple-darwin",
            &command,
            &BTreeMap::from([("RUSTFLAGS".to_string(), "-Cdebuginfo=1".to_string())]),
        ),
    ];

    assert_eq!(keys.into_iter().collect::<BTreeSet<_>>().len(), 7);
}

#[test]
fn key_changes_when_resolved_toolchain_changes() {
    let repository = tempdir().expect("create repository");
    let command = vec!["cargo".to_string(), "check".to_string()];
    let first = CargoValidationCacheKey::new(
        repository.path(),
        repository.path(),
        "declared=stable\nresolved=cargo 1.95.0 (first)",
        "host",
        &command,
        &BTreeMap::new(),
    );
    let second = CargoValidationCacheKey::new(
        repository.path(),
        repository.path(),
        "declared=stable\nresolved=cargo 1.95.1 (second)",
        "host",
        &command,
        &BTreeMap::new(),
    );

    assert_ne!(first, second);
}

#[test]
fn toolchain_identity_tracks_the_selected_cargo_version() {
    let first = cargo_toolchain_identity(Some("rust-toolchain\n1.95.0\n"), "cargo 1.95.0 (first)")
        .expect("first probe should complete");
    let second =
        cargo_toolchain_identity(Some("rust-toolchain\n1.95.0\n"), "cargo 1.95.1 (second)")
            .expect("second probe should complete");

    assert_ne!(first, second);
    assert!(first.contains("cargo 1.95.0 (first)"));
    assert!(second.contains("cargo 1.95.1 (second)"));
}

#[test]
fn success_suppression_requires_an_immutable_toolchain_channel() {
    let environment = HashMap::new();
    for toolchain in [
        None,
        Some("ambient toolchain"),
        Some("rust-toolchain\nstable\n"),
        Some("rust-toolchain\n1.95\n"),
        Some("rust-toolchain.toml\n[toolchain]\nchannel = \"nightly\"\n"),
        Some("rust-toolchain.toml\nnot valid toml"),
    ] {
        assert!(!cargo_toolchain_allows_success_suppression(
            toolchain,
            &environment
        ));
    }

    for toolchain in [
        Some("rust-toolchain\n1.95.0\n"),
        Some("rust-toolchain\nnightly-2026-08-01-aarch64-apple-darwin\n"),
        Some("rust-toolchain.toml\n[toolchain]\nchannel = \"1.95.0\"\n"),
        Some("rust-toolchain.toml\n[toolchain]\nchannel = \"beta-2026-08-01\"\n"),
    ] {
        assert!(cargo_toolchain_allows_success_suppression(
            toolchain,
            &environment
        ));
    }
}

#[test]
fn rustup_toolchain_override_controls_success_suppression() {
    let pinned_file = Some("rust-toolchain.toml\n[toolchain]\nchannel = \"1.95.0\"\n");
    let mutable_override = HashMap::from([("RUSTUP_TOOLCHAIN".to_string(), "stable".to_string())]);
    assert!(!cargo_toolchain_allows_success_suppression(
        pinned_file,
        &mutable_override
    ));

    let pinned_override = HashMap::from([(
        "RUSTUP_TOOLCHAIN".to_string(),
        "nightly-2026-08-01".to_string(),
    )]);
    assert!(cargo_toolchain_allows_success_suppression(
        Some("ambient toolchain"),
        &pinned_override
    ));
}

#[cfg(not(windows))]
#[test]
fn lowercase_rustup_toolchain_does_not_enable_success_suppression_on_unix() {
    let environment = HashMap::from([(
        "rustup_toolchain".to_string(),
        "nightly-2026-08-01".to_string(),
    )]);

    assert!(!cargo_toolchain_allows_success_suppression(
        Some("ambient toolchain"),
        &environment
    ));
}

#[test]
fn curated_environment_keys_build_inputs_but_not_logging_controls() {
    let baseline = HashMap::from([
        ("CC".to_string(), "/opt/toolchain/bin/cc".to_string()),
        ("SDKROOT".to_string(), "/opt/sdk-a".to_string()),
        ("RUST_LOG".to_string(), "trace".to_string()),
    ]);
    let mut different_log = baseline.clone();
    different_log.insert("RUST_LOG".to_string(), "warn".to_string());
    assert_eq!(
        cargo_cache_environment(&baseline),
        cargo_cache_environment(&different_log)
    );

    let mut different_sdk = baseline.clone();
    different_sdk.insert("SDKROOT".to_string(), "/opt/sdk-b".to_string());
    assert_ne!(
        cargo_cache_environment(&baseline),
        cargo_cache_environment(&different_sdk)
    );

    let mut different_target_cc = baseline.clone();
    different_target_cc.insert(
        "CC_AARCH64_UNKNOWN_LINUX_GNU".to_string(),
        "/opt/cross/bin/cc".to_string(),
    );
    assert_ne!(
        cargo_cache_environment(&baseline),
        cargo_cache_environment(&different_target_cc)
    );

    for (name, value) in [
        ("RUSTC_BOOTSTRAP", "1"),
        ("TARGET_CC", "/opt/cross/bin/target-cc"),
        ("HOST_CFLAGS", "-march=native"),
    ] {
        let mut different_build_input = baseline.clone();
        different_build_input.insert(name.to_string(), value.to_string());
        assert_ne!(
            cargo_cache_environment(&baseline),
            cargo_cache_environment(&different_build_input),
            "{name} should affect the cache key"
        );
    }
}

#[test]
fn path_does_not_fragment_cache_identity_but_explicit_tool_inputs_do() {
    let repository = tempdir().expect("create repository");
    let command = ["cargo".to_string(), "check".to_string()];
    let baseline = HashMap::from([
        ("PATH".to_string(), "/session/one/bin".to_string()),
        ("CC".to_string(), "/opt/toolchain/bin/cc".to_string()),
        ("CMAKE".to_string(), "/opt/toolchain/bin/cmake".to_string()),
        (
            "PKG_CONFIG".to_string(),
            "/opt/toolchain/bin/pkg-config".to_string(),
        ),
        (
            "PROTOC".to_string(),
            "/opt/toolchain/bin/protoc".to_string(),
        ),
    ]);
    let mut different_path = baseline.clone();
    different_path.insert("PATH".to_string(), "/session/two/bin".to_string());
    let key = |environment: HashMap<String, String>| {
        CargoValidationCacheKey::for_command(
            repository.path(),
            repository.path(),
            "cargo 1.95.0",
            &command,
            &environment,
        )
        .expect("cache key should build")
    };

    assert_eq!(
        cargo_cache_environment(&baseline),
        cargo_cache_environment(&different_path)
    );
    assert_eq!(key(baseline.clone()), key(different_path));

    for (name, value) in [
        ("CC", "/other/toolchain/bin/cc"),
        ("CMAKE", "/other/toolchain/bin/cmake"),
        ("PKG_CONFIG", "/other/toolchain/bin/pkg-config"),
        ("PROTOC", "/other/toolchain/bin/protoc"),
    ] {
        let mut different_tool = baseline.clone();
        different_tool.insert(name.to_string(), value.to_string());
        assert_ne!(
            cargo_cache_environment(&baseline),
            cargo_cache_environment(&different_tool),
            "{name} should affect the cache environment"
        );
        assert_ne!(
            key(baseline.clone()),
            key(different_tool),
            "{name} should affect the cache key"
        );
    }
}

#[test]
fn compiler_wrapper_environment_is_disabled_without_changing_rustc_semantics() {
    let repository = tempdir().expect("create repository");
    let environment = HashMap::from([
        ("RUSTC".to_string(), "/opt/toolchain/bin/rustc".to_string()),
        (
            "RUSTC_WRAPPER".to_string(),
            "/opt/toolchain/bin/sccache".to_string(),
        ),
        (
            "RUSTC_WORKSPACE_WRAPPER".to_string(),
            "/opt/toolchain/bin/workspace-wrapper".to_string(),
        ),
    ]);
    let without_wrappers = cargo_validation_environment(environment.clone());

    assert_eq!(
        without_wrappers,
        HashMap::from([
            ("RUSTC".to_string(), "/opt/toolchain/bin/rustc".to_string()),
            ("RUSTC_WRAPPER".to_string(), String::new()),
            ("RUSTC_WORKSPACE_WRAPPER".to_string(), String::new()),
        ])
    );
    assert_eq!(
        cargo_cache_environment(&environment),
        cargo_cache_environment(&without_wrappers)
    );
    assert_eq!(
        CargoValidationCacheKey::for_command(
            repository.path(),
            repository.path(),
            "cargo 1.95.0",
            &["cargo".to_string(), "check".to_string()],
            &environment,
        )
        .expect("cache key with configured wrappers"),
        CargoValidationCacheKey::for_command(
            repository.path(),
            repository.path(),
            "cargo 1.95.0",
            &["cargo".to_string(), "check".to_string()],
            &without_wrappers,
        )
        .expect("cache key with disabled wrappers"),
    );
}

#[cfg(not(windows))]
#[test]
fn compiler_wrapper_environment_names_are_case_sensitive_on_unix() {
    let environment = HashMap::from([
        ("rustc_wrapper".to_string(), "lowercase-wrapper".to_string()),
        (
            "rustc_workspace_wrapper".to_string(),
            "lowercase-workspace-wrapper".to_string(),
        ),
    ]);

    assert_eq!(
        cargo_validation_environment(environment.clone()),
        environment
            .into_iter()
            .chain([
                ("RUSTC_WRAPPER".to_string(), String::new()),
                ("RUSTC_WORKSPACE_WRAPPER".to_string(), String::new()),
            ])
            .collect()
    );
}

#[cfg(windows)]
#[test]
fn compiler_wrapper_environment_names_are_case_insensitive_on_windows() {
    let environment = HashMap::from([
        ("rustc_wrapper".to_string(), "lowercase-wrapper".to_string()),
        (
            "rustc_workspace_wrapper".to_string(),
            "lowercase-workspace-wrapper".to_string(),
        ),
        ("rustc".to_string(), "configured-rustc".to_string()),
    ]);

    assert_eq!(
        cargo_validation_environment(environment),
        HashMap::from([
            ("rustc".to_string(), "configured-rustc".to_string()),
            ("RUSTC_WRAPPER".to_string(), String::new()),
            ("RUSTC_WORKSPACE_WRAPPER".to_string(), String::new()),
        ])
    );
}

#[tokio::test]
async fn cold_partial_artifacts_are_reused_by_warm_acquire() {
    let home = tempdir().expect("create cache home");
    let repository = tempdir().expect("create repository");
    let key = cache_key(&repository, "1.95.0", "host", &["cargo", "check"]);
    let cancellation = CancellationToken::new();
    let first = acquire(
        &home,
        &repository,
        key.clone(),
        &cancellation,
        test_limits(),
    )
    .await;
    let target = first.target_dir().clone();
    fs::write(target.join("partial-artifact"), "compiled").expect("write partial artifact");
    maintain_cache(first).expect("finish cold cache use");

    let second = acquire(&home, &repository, key, &cancellation, test_limits()).await;
    assert_eq!(second.target_dir(), &target);
    assert_eq!(
        fs::read_to_string(second.target_dir().join("partial-artifact"))
            .expect("read reused artifact"),
        "compiled"
    );
    maintain_cache(second).expect("finish warm cache use");
}

#[tokio::test]
async fn concurrent_acquire_waits_and_cancellation_releases_the_waiter() {
    let home = tempdir().expect("create cache home");
    let repository = tempdir().expect("create repository");
    let key = cache_key(&repository, "1.95.0", "host", &["cargo", "check"]);
    let owner = acquire(
        &home,
        &repository,
        key.clone(),
        &CancellationToken::new(),
        test_limits(),
    )
    .await;
    let waiter_cancellation = CancellationToken::new();
    let home_path = absolute_root(&home);
    let repository_path = repository.path().to_path_buf();
    let waiter_cancellation_for_task = waiter_cancellation.clone();
    let mut waiter = tokio::spawn(async move {
        CargoValidationCacheLease::acquire_with_limits(
            &home_path,
            &repository_path,
            &repository_path,
            key,
            &waiter_cancellation_for_task,
            test_limits(),
        )
        .await
    });

    assert!(
        timeout(Duration::from_millis(75), &mut waiter)
            .await
            .is_err()
    );
    waiter_cancellation.cancel();
    assert!(
        waiter
            .await
            .expect("waiter task should complete")
            .expect("waiter acquisition should not fail")
            .is_none()
    );
    drop(owner);
}

#[tokio::test]
async fn contended_entry_lock_falls_back_without_infrastructure_failure() {
    let home = tempdir().expect("create cache home");
    let repository = tempdir().expect("create repository");
    let key = cache_key(&repository, "1.95.0", "host", &["cargo", "check"]);
    let owner = acquire(
        &home,
        &repository,
        key.clone(),
        &CancellationToken::new(),
        test_limits(),
    )
    .await;
    let started = tokio::time::Instant::now();

    let fallback = CargoValidationCacheLease::acquire_with_limits(
        &absolute_root(&home),
        repository.path(),
        repository.path(),
        key,
        &CancellationToken::new(),
        test_limits(),
    )
    .await
    .expect("contention should not fail cache preparation");

    assert!(fallback.is_none());
    assert!(started.elapsed() < Duration::from_secs(1));
    drop(owner);
}

#[tokio::test]
async fn disk_bound_evicts_oldest_unlocked_entry() {
    let home = tempdir().expect("create cache home");
    let repository = tempdir().expect("create repository");
    let limits = CargoValidationCacheLimits {
        max_entries: 1,
        max_entry_bytes: 8,
        max_total_bytes: 8,
        max_files_per_entry: 4,
    };
    let first_key = cache_key(&repository, "first", "host", &["cargo", "check"]);
    let first = acquire(
        &home,
        &repository,
        first_key,
        &CancellationToken::new(),
        limits,
    )
    .await;
    let first_artifact = first.target_dir().join("artifact");
    fs::write(&first_artifact, "12345678").expect("write first artifact");
    write_last_used(
        first_artifact
            .parent()
            .expect("first target")
            .parent()
            .expect("first entry")
            .as_ref(),
        /*timestamp*/ 1,
    )
    .expect("age first entry");
    drop(first);

    let second_key = cache_key(&repository, "second", "host", &["cargo", "check"]);
    let second = acquire(
        &home,
        &repository,
        second_key,
        &CancellationToken::new(),
        limits,
    )
    .await;
    fs::write(second.target_dir().join("artifact"), "abcdefgh").expect("write second artifact");
    maintain_cache(second).expect("finish second entry");

    assert!(!first_artifact.exists());
}

#[tokio::test]
async fn oversized_entry_is_cleaned_instead_of_reused() {
    let home = tempdir().expect("create cache home");
    let repository = tempdir().expect("create repository");
    let limits = CargoValidationCacheLimits {
        max_entries: 2,
        max_entry_bytes: 4,
        max_total_bytes: 8,
        max_files_per_entry: 4,
    };
    let key = cache_key(&repository, "1.95.0", "host", &["cargo", "check"]);
    let lease = acquire(&home, &repository, key, &CancellationToken::new(), limits).await;
    let artifact = lease.target_dir().join("oversized");
    fs::write(&artifact, "12345").expect("write oversized artifact");
    maintain_cache(lease).expect("finish oversized entry");

    assert!(!artifact.exists());
}

#[tokio::test]
async fn abandoned_oversized_entry_is_pruned_by_later_maintenance() {
    let home = tempdir().expect("create cache home");
    let repository = tempdir().expect("create repository");
    let limits = CargoValidationCacheLimits {
        max_entries: 4,
        max_entry_bytes: 4,
        max_total_bytes: 64,
        max_files_per_entry: 4,
    };
    let abandoned = acquire(
        &home,
        &repository,
        cache_key(&repository, "abandoned", "host", &["cargo", "check"]),
        &CancellationToken::new(),
        limits,
    )
    .await;
    let artifact = abandoned.target_dir().join("oversized");
    fs::write(&artifact, "12345").expect("write abandoned oversized artifact");
    drop(abandoned);

    let later = acquire(
        &home,
        &repository,
        cache_key(&repository, "later", "host", &["cargo", "check"]),
        &CancellationToken::new(),
        limits,
    )
    .await;

    maintain_cache(later).expect("finish later entry");
    assert!(!artifact.exists());
}

#[tokio::test]
async fn maintenance_waits_in_background_when_cleanup_lock_is_contended() {
    let home = tempdir().expect("create cache home");
    let repository = tempdir().expect("create repository");
    let lease = acquire(
        &home,
        &repository,
        cache_key(&repository, "1.95.0", "host", &["cargo", "check"]),
        &CancellationToken::new(),
        test_limits(),
    )
    .await;
    let entry = lease
        .target_dir()
        .parent()
        .expect("target should have an entry parent")
        .to_path_buf();
    let root = entry
        .parent()
        .and_then(Path::parent)
        .expect("entry should be below the cache root");
    let cleanup_lock = open_lock(&root.join("cleanup.lock")).expect("open cleanup lock");
    cleanup_lock.lock_exclusive().expect("hold cleanup lock");
    let (result_tx, mut result_rx) = tokio::sync::oneshot::channel();
    spawn_maintenance(lease, move |result| {
        let _ = result_tx.send(result);
    });

    assert!(
        timeout(Duration::from_millis(75), &mut result_rx)
            .await
            .is_err()
    );
    FileExt::unlock(&cleanup_lock).expect("unlock cleanup lock");
    timeout(Duration::from_secs(1), result_rx)
        .await
        .expect("maintenance should finish after the cleanup lock is released")
        .expect("maintenance result should be reported")
        .expect("contended maintenance should eventually enforce bounds");

    assert!(entry.join("last-used").exists());
}

#[tokio::test]
async fn blocking_cache_preparation_is_explicitly_bounded() {
    let cancellation = CancellationToken::new();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let started = tokio::time::Instant::now();

    let result = bounded_blocking(&cancellation, "test preparation", move || {
        release_rx.recv().map_err(io::Error::other)?;
        Ok(())
    })
    .await
    .expect("preparation timeout should be a graceful fallback");

    assert!(result.is_none());
    assert!(started.elapsed() < Duration::from_secs(1));
    release_tx.send(()).expect("release preparation worker");
}

#[tokio::test]
async fn metadata_failure_is_reported_without_skipping_cache_enforcement() {
    let home = tempdir().expect("create cache home");
    let repository = tempdir().expect("create repository");
    let limits = CargoValidationCacheLimits {
        max_entries: 4,
        max_entry_bytes: 4,
        max_total_bytes: 64,
        max_files_per_entry: 4,
    };
    let lease = acquire(
        &home,
        &repository,
        cache_key(&repository, "1.95.0", "host", &["cargo", "check"]),
        &CancellationToken::new(),
        limits,
    )
    .await;
    let entry = lease
        .target_dir()
        .parent()
        .expect("target should have an entry parent")
        .to_path_buf();
    let artifact = lease.target_dir().join("oversized");
    fs::write(&artifact, "12345").expect("write oversized artifact");
    fs::create_dir(entry.join("last-used")).expect("block last-used metadata write");
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();

    spawn_maintenance(lease, move |result| {
        let _ = result_tx.send(result);
    });

    let result = timeout(Duration::from_secs(1), result_rx)
        .await
        .expect("maintenance should finish")
        .expect("maintenance result should be reported");
    assert!(result.is_err());
    assert!(!artifact.exists());
}

#[tokio::test]
async fn cleanup_does_not_traverse_another_active_shard() {
    let home = tempdir().expect("create cache home");
    let repository = tempdir().expect("create repository");
    let root = cache_root(&absolute_root(&home), repository.path(), repository.path())
        .expect("create cache root");
    let active_key = cache_key(&repository, "active", "host", &["cargo", "check"]);
    let later_key = (0..u32::MAX)
        .map(|index| {
            cache_key(
                &repository,
                &format!("later-{index}"),
                "host",
                &["cargo", "check"],
            )
        })
        .find(|key| lock_path(&root, key) != lock_path(&root, &active_key))
        .expect("find a key in another shard");
    let active = acquire(
        &home,
        &repository,
        active_key,
        &CancellationToken::new(),
        test_limits(),
    )
    .await;
    let active_target = active.target_dir().to_path_buf();
    fs::remove_dir(&active_target).expect("remove empty active target");
    fs::write(&active_target, "concurrently mutating").expect("replace active target with a file");

    let later = acquire(
        &home,
        &repository,
        later_key,
        &CancellationToken::new(),
        test_limits(),
    )
    .await;

    maintain_cache(later).expect("maintain later entry");
    fs::remove_file(&active_target).expect("remove active target stand-in");
    fs::create_dir(&active_target).expect("restore active target directory");
    drop(active);
}
