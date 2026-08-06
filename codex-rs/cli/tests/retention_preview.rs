use std::fs;
use std::path::Path;

use anyhow::Result;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

fn codex_command(codex_home: &Path) -> Result<assert_cmd::Command> {
    let mut command = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("codex")?);
    command.env("CODEX_LAB_HOME", codex_home);
    Ok(command)
}

#[test]
fn empty_json_preview_is_read_only() -> Result<()> {
    let codex_home = TempDir::new()?;
    let output = codex_command(codex_home.path())?
        .args(["retention", "preview", "--json"])
        .output()?;

    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(report["schemaVersion"], 1);
    assert_eq!(report["previewOnly"], true);
    assert_eq!(report["items"], serde_json::json!([]));
    assert_eq!(report["nextCursor"], serde_json::Value::Null);
    assert_eq!(report["pageTotals"]["candidateCount"], 0);
    assert_eq!(report["totals"], serde_json::Value::Null);
    assert_no_files(codex_home.path())?;

    Ok(())
}

#[test]
fn preview_rejects_out_of_range_limit() -> Result<()> {
    let codex_home = TempDir::new()?;
    let output = codex_command(codex_home.path())?
        .args(["retention", "preview", "--limit", "101"])
        .output()?;

    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)?.contains("limit must be between 1 and 100"),
        "unexpected clap error"
    );

    Ok(())
}

fn assert_no_files(directory: &Path) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        assert!(entry.file_type()?.is_dir(), "unexpected file: {entry:?}");
        assert_no_files(entry.path().as_path())?;
    }
    Ok(())
}
