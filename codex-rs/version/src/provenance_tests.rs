use std::path::PathBuf;

use pretty_assertions::assert_eq;
use serde_json::json;

use super::BUILD_PROVENANCE_SCHEMA_VERSION;
use super::BuildProvenance;
use super::BuildProvenanceMetadata;
use super::DirtyState;
use super::provenance_from_metadata;

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

#[test]
fn clean_metadata_records_versioned_json() {
    let provenance = provenance_from_metadata(
        metadata(Some(COMMIT), Some("clean"), Some("debug"), Some("dev")),
        Some(PathBuf::from("/tmp/codex")),
    );

    assert_eq!(
        provenance.as_json(),
        json!({
            "schema_version": BUILD_PROVENANCE_SCHEMA_VERSION,
            "version": "1.2.3",
            "source_commit": COMMIT,
            "dirty_state": "clean",
            "build_profile": "debug",
            "build_channel": "dev",
            "executable_path": "/tmp/codex",
        })
    );
}

#[test]
fn missing_metadata_is_explicitly_unavailable() {
    assert_eq!(
        provenance(
            /*source_commit*/ None, /*dirty_state*/ None, /*build_profile*/ None,
            /*build_channel*/ None,
        ),
        unavailable("unavailable")
    );
}

#[test]
fn dirty_metadata_normalizes_commit_but_stays_observational() {
    let mut expected = unavailable("release");
    expected.source_commit = "abcdef0123456789abcdef0123456789abcdef01".to_string();
    expected.dirty_state = DirtyState::Dirty;
    expected.build_channel = "release".to_string();
    assert_eq!(
        provenance(
            Some("ABCDEF0123456789ABCDEF0123456789ABCDEF01"),
            Some("dirty"),
            Some("release"),
            Some("release"),
        ),
        expected
    );
}

#[test]
fn malformed_or_partial_source_metadata_fails_as_an_atomic_tuple() {
    for (commit, dirty, channel) in [
        (Some(COMMIT), None, Some("dev")),
        (Some("not-a-commit"), Some("clean"), Some("dev")),
    ] {
        assert_eq!(
            provenance(commit, dirty, Some("debug"), channel),
            unavailable("debug")
        );
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_executable_path_is_unavailable() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let provenance = provenance_from_metadata(
        metadata(Some(COMMIT), Some("clean"), Some("debug"), Some("dev")),
        Some(PathBuf::from(OsString::from_vec(vec![b'/', 0xff]))),
    );

    assert_eq!(provenance.executable_path, "unavailable");
}

fn provenance(
    source_commit: Option<&str>,
    dirty_state: Option<&str>,
    build_profile: Option<&str>,
    build_channel: Option<&str>,
) -> BuildProvenance {
    provenance_from_metadata(
        metadata(source_commit, dirty_state, build_profile, build_channel),
        /*executable_path*/ None,
    )
}

fn metadata<'a>(
    source_commit: Option<&'a str>,
    dirty_state: Option<&'a str>,
    build_profile: Option<&'a str>,
    build_channel: Option<&'a str>,
) -> BuildProvenanceMetadata<'a> {
    BuildProvenanceMetadata {
        version: "1.2.3",
        source_commit,
        dirty_state,
        build_profile,
        build_channel,
    }
}

fn unavailable(build_profile: &str) -> BuildProvenance {
    BuildProvenance {
        schema_version: BUILD_PROVENANCE_SCHEMA_VERSION,
        version: "1.2.3".to_string(),
        source_commit: "unavailable".to_string(),
        dirty_state: DirtyState::Unavailable,
        build_profile: build_profile.to_string(),
        build_channel: "unavailable".to_string(),
        executable_path: "unavailable".to_string(),
    }
}
