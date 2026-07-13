use std::fs;

use pretty_assertions::assert_eq;

use super::write_file_atomically;

#[test]
fn atomic_write_replaces_contents_without_artifacts() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("local.age");

    write_file_atomically(&destination, b"one")?;
    write_file_atomically(&destination, b"two")?;

    assert_eq!(fs::read(&destination)?, b"two".to_vec());
    let entries = fs::read_dir(dir.path())?.collect::<std::io::Result<Vec<_>>>()?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path(), destination);
    Ok(())
}

#[cfg(not(windows))]
#[test]
fn failed_replace_preserves_destination_and_cleans_temp() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("destination");
    fs::create_dir(&destination)?;

    write_file_atomically(&destination, b"secret").expect_err("replacing a directory must fail");

    assert!(destination.is_dir());
    let entries = fs::read_dir(dir.path())?.collect::<std::io::Result<Vec<_>>>()?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path(), destination);
    Ok(())
}

#[cfg(unix)]
#[test]
fn atomic_write_hardens_permissions_on_create_and_replace() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("local.age");

    write_file_atomically(&destination, b"one")?;
    assert_eq!(
        fs::metadata(&destination)?.permissions().mode() & 0o777,
        0o600
    );
    fs::set_permissions(&destination, fs::Permissions::from_mode(/*mode*/ 0o644))?;
    write_file_atomically(&destination, b"two")?;

    assert_eq!(fs::read(&destination)?, b"two".to_vec());
    assert_eq!(
        fs::metadata(destination)?.permissions().mode() & 0o777,
        0o600
    );
    Ok(())
}
