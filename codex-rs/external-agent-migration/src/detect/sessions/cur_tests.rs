use super::*;
use codex_protocol::ThreadId;
use pretty_assertions::assert_eq;
use std::fs::FileTimes;
use std::fs::OpenOptions;
use std::time::Duration;
use std::time::SystemTime;
use tempfile::TempDir;

#[test]
fn detects_cur_transcript_with_project_cwd() {
    let root = TempDir::new().expect("tempdir");
    let project_root = root.path().join("workspace with.dots_and-dashes");
    fs::create_dir_all(&project_root).expect("project root");
    let external_agent_home = root.path().join(".external");
    let encoded_project = encode_project_path(&project_root);
    let transcript = write_transcript(
        &external_agent_home,
        &encoded_project,
        "a-session",
        "first request",
    );

    let sessions =
        detect_recent_cur_sessions(&external_agent_home, root.path()).expect("detect sessions");

    assert_eq!(
        sessions,
        vec![ExternalAgentSessionMigration {
            path: transcript,
            cwd: project_root,
            title: Some("first request".to_string()),
        }]
    );
}

#[test]
fn detects_projectless_cur_transcript_without_embedded_metadata() {
    let root = TempDir::new().expect("tempdir");
    let external_agent_home = root.path().join(".cursor");
    let transcript = write_transcript(
        &external_agent_home,
        "empty-window",
        "projectless-session",
        "first request",
    );

    let sessions =
        detect_recent_cur_sessions(&external_agent_home, root.path()).expect("detect sessions");

    assert_eq!(
        sessions,
        vec![ExternalAgentSessionMigration {
            path: transcript,
            cwd: root.path().to_path_buf(),
            title: Some("first request".to_string()),
        }]
    );
}

#[test]
fn resolves_projectless_cur_cwd_from_relative_home() {
    let current_dir = std::env::current_dir().expect("current dir");

    assert_eq!(
        cur_project_cwd(
            Path::new(".cursor/projects/empty-window"),
            Path::new(".cursor"),
        ),
        Some(current_dir)
    );
}

#[test]
fn detects_cur_transcript_with_embedded_unc_cwd() {
    let root = TempDir::new().expect("tempdir");
    let external_agent_home = root.path().join(".external");
    let encoded_project = "server-share-repo";
    let unc_cwd = PathBuf::from(r"\\server\share\repo");
    let transcript = external_agent_home
        .join("projects")
        .join(encoded_project)
        .join("agent-transcripts")
        .join("unc-session/unc-session.jsonl");
    fs::create_dir_all(transcript.parent().expect("transcript parent"))
        .expect("transcript directory");
    fs::write(
        &transcript,
        [
            serde_json::json!({
                "cwd": unc_cwd,
                "role": "user",
                "timestamp_ms": 1_800_000_000_000_i64,
                "message": {
                    "content": [{
                        "type": "text",
                        "text": "<user_query>first request</user_query>",
                    }],
                },
            })
            .to_string(),
            serde_json::json!({
                "role": "assistant",
                "message": {
                    "content": [{"type": "text", "text": "first answer"}],
                },
            })
            .to_string(),
        ]
        .join("\n"),
    )
    .expect("transcript");

    assert_eq!(
        detect_recent_cur_sessions(&external_agent_home, root.path()).expect("detect sessions"),
        vec![ExternalAgentSessionMigration {
            path: transcript,
            cwd: unc_cwd,
            title: Some("first request".to_string()),
        }]
    );
}

#[test]
fn skips_cur_subagent_transcripts() {
    let root = TempDir::new().expect("tempdir");
    let project_root = root.path().join("workspace");
    fs::create_dir_all(&project_root).expect("project root");
    let external_agent_home = root.path().join(".external");
    let encoded_project = encode_project_path(&project_root);
    let transcript = write_transcript(
        &external_agent_home,
        &encoded_project,
        "main-session",
        "first request",
    );
    let subagent_transcript = external_agent_home
        .join("projects")
        .join(&encoded_project)
        .join("agent-transcripts")
        .join("main-session/subagents/worker/worker.jsonl");
    fs::create_dir_all(
        subagent_transcript
            .parent()
            .expect("subagent transcript parent"),
    )
    .expect("subagent transcript directory");
    fs::write(&subagent_transcript, transcript_contents("first request"))
        .expect("subagent transcript");

    let sessions =
        detect_recent_cur_sessions(&external_agent_home, root.path()).expect("detect sessions");

    assert_eq!(
        sessions,
        vec![ExternalAgentSessionMigration {
            path: transcript,
            cwd: project_root,
            title: Some("first request".to_string()),
        }]
    );
}

#[test]
fn rejects_ambiguous_encoded_project_cwd() {
    let root = TempDir::new().expect("tempdir");
    let nested_project = root.path().join("workspace").join("nested");
    let hyphenated_project = root.path().join("workspace-nested");
    fs::create_dir_all(&nested_project).expect("nested project");
    fs::create_dir_all(&hyphenated_project).expect("hyphenated project");

    assert_eq!(
        decode_cur_project_path(&encode_project_path(&nested_project)),
        None
    );
}

#[test]
fn resolves_cur_project_names_with_common_separators() {
    for (project_name, encoded_name) in [
        ("project", "project"),
        ("my-project", "my-project"),
        ("my--project", "my-project"),
        ("my project", "my-project"),
        ("my.project", "my-project"),
        ("my..project", "my-project"),
        ("my_project", "my-project"),
        ("my+project", "my-project"),
        ("my@project", "my-project"),
        ("my&project", "my-project"),
        ("my-awesome-project", "my-awesome-project"),
        ("my-awesome-cool-project", "my-awesome-cool-project"),
    ] {
        let root = TempDir::new().expect("tempdir");
        let project = root.path().join(project_name);
        fs::create_dir_all(&project).expect("project root");
        let encoded = format!("{}-{encoded_name}", encode_project_path(root.path()));

        assert_eq!(decode_cur_project_path(&encoded), Some(project));
    }
}

#[test]
fn resolves_cur_project_below_hyphenated_ancestor_and_leaf() {
    let root = TempDir::new().expect("tempdir");
    let project = root
        .path()
        .join("developer-artifacts")
        .join("cursor-project");
    fs::create_dir_all(&project).expect("project root");

    assert_eq!(
        decode_cur_project_path(&encode_project_path(&project)),
        Some(project)
    );
}

#[test]
fn resolves_cur_project_with_raw_and_normalized_components() {
    let root = TempDir::new().expect("tempdir");
    let project = root.path().join("team.with space").join("repo_name");
    fs::create_dir_all(&project).expect("project root");
    let encoded = format!(
        "{}-team.with space-repo-name",
        encode_project_path(root.path())
    );

    assert_eq!(decode_cur_project_path(&encoded), Some(project));
}

#[test]
fn rejects_ambiguous_cur_project_without_a_direct_match() {
    let root = TempDir::new().expect("tempdir");
    for project_name in ["my-project", "my project", "my+project"] {
        fs::create_dir_all(root.path().join(project_name)).expect("project root");
    }
    let encoded = format!("{}-my-project", encode_project_path(root.path()));

    assert_eq!(decode_cur_project_path(&encoded), None);
}

#[test]
fn rejects_ambiguous_cur_project_with_punctuated_ancestor() {
    let root = TempDir::new().expect("tempdir");
    let punctuated_ancestor = root.path().join("a-b").join("c");
    let punctuated_leaf = root.path().join("a").join("b-c");
    fs::create_dir_all(&punctuated_ancestor).expect("punctuated ancestor project");
    fs::create_dir_all(&punctuated_leaf).expect("punctuated leaf project");
    let encoded = encode_project_path(&punctuated_ancestor);

    assert_eq!(encoded, encode_project_path(&punctuated_leaf));
    assert_eq!(decode_cur_project_path(&encoded), None);
}

#[test]
fn rejects_ambiguous_cur_project_below_hyphenated_ancestor() {
    let root = TempDir::new().expect("tempdir");
    let hyphenated_ancestor = root
        .path()
        .join("developer-artifacts")
        .join("cursor-project");
    let split_ancestor = root
        .path()
        .join("developer")
        .join("artifacts-cursor-project");
    fs::create_dir_all(&hyphenated_ancestor).expect("hyphenated ancestor project");
    fs::create_dir_all(&split_ancestor).expect("split ancestor project");

    assert_eq!(
        encode_project_path(&hyphenated_ancestor),
        encode_project_path(&split_ancestor)
    );
    assert_eq!(
        decode_cur_project_path(&encode_project_path(&hyphenated_ancestor)),
        None
    );
}

#[test]
fn rejects_ambiguous_cur_project_with_multiple_punctuated_ancestors() {
    for (first, second) in [
        (&["a-b", "c-d", "e"][..], &["a", "b", "c", "d-e"][..]),
        (&["a-b", "c-d"][..], &["a", "b", "c", "d"][..]),
    ] {
        let root = TempDir::new().expect("tempdir");
        let first = first
            .iter()
            .fold(root.path().to_path_buf(), |path, component| {
                path.join(component)
            });
        let second = second
            .iter()
            .fold(root.path().to_path_buf(), |path, component| {
                path.join(component)
            });
        fs::create_dir_all(&first).expect("first project");
        fs::create_dir_all(&second).expect("second project");
        let encoded = encode_project_path(&first);

        assert_eq!(encoded, encode_project_path(&second));
        assert_eq!(decode_cur_project_path(&encoded), None);
    }
}

#[test]
fn returns_none_when_cur_project_directory_probe_budget_is_exhausted() {
    let root = TempDir::new().expect("tempdir");
    fs::create_dir_all(root.path().join("ancestor/project")).expect("project root");
    let limits = CurProjectPathSearchLimits {
        max_directory_probes: 1,
        ..CUR_PROJECT_PATH_SEARCH_LIMITS
    };

    assert_eq!(
        resolve_cur_project_path(root.path().to_path_buf(), "ancestor-project", limits),
        None
    );
}

#[test]
fn returns_none_when_cur_project_entry_scan_budget_is_exhausted() {
    let root = TempDir::new().expect("tempdir");
    fs::create_dir_all(root.path().join("project")).expect("project root");
    fs::create_dir_all(root.path().join("unrelated")).expect("unrelated directory");
    let limits = CurProjectPathSearchLimits {
        max_entries_scanned: 1,
        ..CUR_PROJECT_PATH_SEARCH_LIMITS
    };

    assert_eq!(
        resolve_cur_project_path(root.path().to_path_buf(), "project", limits),
        None
    );
}

#[test]
fn matches_raw_ascii_case_according_to_native_filesystem_semantics() {
    let root = TempDir::new().expect("tempdir");
    let project = root.path().join("Project");
    fs::create_dir_all(&project).expect("project root");
    let alternate_spelling = root.path().join("project");
    let expected = fs::canonicalize(&alternate_spelling)
        .ok()
        .filter(|resolved| *resolved == fs::canonicalize(&project).expect("canonical project"))
        .map(|_| project);

    assert_eq!(
        resolve_cur_project_path(
            root.path().to_path_buf(),
            "project",
            CUR_PROJECT_PATH_SEARCH_LIMITS,
        ),
        expected
    );
}

#[test]
fn matches_normalized_ascii_case_according_to_native_filesystem_semantics() {
    let root = TempDir::new().expect("tempdir");
    let project = root.path().join("Repo_Name");
    fs::create_dir_all(&project).expect("project root");
    let alternate_spelling = root.path().join("repo_name");
    let alternate_matches = fs::canonicalize(&alternate_spelling)
        .ok()
        .is_some_and(|resolved| resolved == fs::canonicalize(&project).expect("canonical project"));

    assert_eq!(
        resolve_cur_project_path(
            root.path().to_path_buf(),
            "repo-name",
            CUR_PROJECT_PATH_SEARCH_LIMITS,
        ),
        alternate_matches.then(|| project.clone())
    );

    if !alternate_matches {
        fs::create_dir_all(&alternate_spelling).expect("case-distinct project");
        assert_eq!(
            resolve_cur_project_path(
                root.path().to_path_buf(),
                "repo-name",
                CUR_PROJECT_PATH_SEARCH_LIMITS,
            ),
            Some(alternate_spelling)
        );
    }
}

#[cfg(unix)]
#[test]
fn resolves_symlinked_cur_project_ancestor_and_leaf_as_alias_path() {
    use std::os::unix::fs::symlink;

    let root = TempDir::new().expect("tempdir");
    let actual_ancestor = root.path().join("actual-parent");
    let actual_project = actual_ancestor.join("actual-project");
    fs::create_dir_all(&actual_project).expect("actual project");
    let linked_ancestor = root.path().join("linked-parent");
    symlink(&actual_ancestor, &linked_ancestor).expect("linked ancestor");
    let linked_project = actual_ancestor.join("linked-project");
    symlink(&actual_project, &linked_project).expect("linked project");
    let expected = linked_ancestor.join("linked-project");

    assert_eq!(
        resolve_cur_project_path(
            root.path().to_path_buf(),
            "linked-parent-linked-project",
            CUR_PROJECT_PATH_SEARCH_LIMITS,
        ),
        Some(expected)
    );
}

#[test]
fn honors_cur_project_encoded_input_budget_boundary() {
    let root = TempDir::new().expect("tempdir");
    let project = root.path().join("project");
    fs::create_dir_all(&project).expect("project root");
    let encoded = encode_project_path(&project);
    let exact_limits = CurProjectPathSearchLimits {
        max_input_bytes: encoded.len(),
        ..CUR_PROJECT_PATH_SEARCH_LIMITS
    };
    let over_limit = CurProjectPathSearchLimits {
        max_input_bytes: encoded.len() - 1,
        ..CUR_PROJECT_PATH_SEARCH_LIMITS
    };

    assert_eq!(
        decode_cur_project_path_with_limits(&encoded, exact_limits),
        Some(project)
    );
    assert_eq!(
        decode_cur_project_path_with_limits(&encoded, over_limit),
        None
    );
}

#[test]
fn returns_none_when_cur_project_frontier_budget_is_exhausted() {
    let root = TempDir::new().expect("tempdir");
    let project = root.path().join("a/b/c");
    fs::create_dir_all(&project).expect("project root");
    fs::create_dir_all(root.path().join("a-b")).expect("second prefix");
    let limits = CurProjectPathSearchLimits {
        max_frontier_states: 1,
        ..CUR_PROJECT_PATH_SEARCH_LIMITS
    };

    assert_eq!(
        resolve_cur_project_path(
            root.path().to_path_buf(),
            "a-b-c",
            CUR_PROJECT_PATH_SEARCH_LIMITS,
        ),
        Some(project)
    );
    assert_eq!(
        resolve_cur_project_path(root.path().to_path_buf(), "a-b-c", limits),
        None
    );
}

#[test]
fn parses_windows_cursor_fixture_project_directory() {
    assert_eq!(
        decode_cur_windows_project_drive("C--Users-fixture-Cursor"),
        Some(('C', "-Users-fixture-Cursor"))
    );
    assert_eq!(
        decode_cur_windows_project_drive("C-Users-fixture-Cursor"),
        Some(('C', "Users-fixture-Cursor"))
    );
    assert_eq!(decode_cur_windows_project_drive("1-Users-fixture"), None);
}

#[test]
fn ignores_cur_sessions_older_than_import_window() {
    let root = TempDir::new().expect("tempdir");
    let project_root = root.path().join("workspace");
    fs::create_dir_all(&project_root).expect("project root");
    let external_agent_home = root.path().join(".external");
    let transcript = write_transcript(
        &external_agent_home,
        &encode_project_path(&project_root),
        "old-session",
        "old request",
    );
    set_modified_at(
        &transcript,
        SystemTime::UNIX_EPOCH + Duration::from_secs(/*secs*/ 1),
    );

    assert!(
        detect_recent_cur_sessions(&external_agent_home, root.path())
            .expect("detect sessions")
            .is_empty()
    );
}

#[test]
fn detects_cur_sessions_in_batches_and_redetects_modified_imports() {
    let root = TempDir::new().expect("tempdir");
    let project_root = root.path().join("workspace");
    fs::create_dir_all(&project_root).expect("project root");
    let external_agent_home = root.path().join(".external");
    let encoded_project = encode_project_path(&project_root);
    let modified_at = SystemTime::now();
    let mut expected = Vec::new();
    let default_limits = ExternalAgentSessionImportLimits::default();
    for index in 0..=default_limits.max_sessions {
        let session_id = format!("session-{index:02}");
        let title = format!("request {index}");
        let path = write_transcript(&external_agent_home, &encoded_project, &session_id, &title);
        set_modified_at(
            &path,
            modified_at - Duration::from_secs(/*secs*/ index as u64),
        );
        expected.push(ExternalAgentSessionMigration {
            path,
            cwd: project_root.clone(),
            title: Some(title),
        });
    }
    let oldest_session = expected.pop().expect("oldest session");

    let sessions =
        detect_recent_cur_sessions(&external_agent_home, root.path()).expect("detect sessions");

    assert_eq!(sessions, expected);
    for session in &sessions {
        crate::sessions::ledger::record_imported_session(
            root.path(),
            &session.path,
            ThreadId::new(),
        )
        .expect("record import");
    }

    assert_eq!(
        detect_recent_cur_sessions(&external_agent_home, root.path()).expect("detect sessions"),
        vec![oldest_session.clone()]
    );
    crate::sessions::ledger::record_imported_session(
        root.path(),
        &oldest_session.path,
        ThreadId::new(),
    )
    .expect("record oldest import");
    assert!(
        detect_recent_cur_sessions(&external_agent_home, root.path())
            .expect("detect sessions")
            .is_empty()
    );

    let modified_session = &expected[0];
    let updated_record = serde_json::json!({
        "role": "assistant",
        "message": {
            "content": [{"type": "text", "text": "updated answer"}],
        },
    })
    .to_string();
    fs::write(
        &modified_session.path,
        format!(
            "{}\n{updated_record}",
            transcript_contents(modified_session.title.as_deref().expect("session title"))
        ),
    )
    .expect("update transcript");
    set_modified_at(
        &modified_session.path,
        SystemTime::now() + Duration::from_secs(/*secs*/ 1),
    );

    assert_eq!(
        detect_recent_cur_sessions(&external_agent_home, root.path()).expect("detect sessions"),
        vec![modified_session.clone()]
    );
}

fn write_transcript(
    external_agent_home: &Path,
    encoded_project: &str,
    session_id: &str,
    first_request: &str,
) -> PathBuf {
    let transcript = external_agent_home
        .join("projects")
        .join(encoded_project)
        .join("agent-transcripts")
        .join(session_id)
        .join(format!("{session_id}.jsonl"));
    fs::create_dir_all(transcript.parent().expect("transcript parent"))
        .expect("transcript directory");
    fs::write(&transcript, transcript_contents(first_request)).expect("transcript");
    transcript
}

fn transcript_contents(first_request: &str) -> String {
    [
        serde_json::json!({
            "role": "user",
            "message": {
                "content": [{
                    "type": "text",
                    "text": format!("<user_query>{first_request}</user_query>"),
                }],
            },
        })
        .to_string(),
        serde_json::json!({
            "role": "assistant",
            "message": {
                "content": [{"type": "text", "text": "first answer"}],
            },
        })
        .to_string(),
    ]
    .join("\n")
}

fn set_modified_at(path: &Path, modified_at: SystemTime) {
    OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open transcript")
        .set_times(FileTimes::new().set_modified(modified_at))
        .expect("set transcript modified time");
}

#[cfg(windows)]
fn encode_project_path(path: &Path) -> String {
    path.to_string_lossy().replace([':', '\\', '/'], "-")
}

#[cfg(not(windows))]
fn encode_project_path(path: &Path) -> String {
    path.to_string_lossy()
        .trim_start_matches('/')
        .replace('/', "-")
}
