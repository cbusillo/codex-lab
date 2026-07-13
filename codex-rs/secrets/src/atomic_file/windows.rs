use std::ffi::c_void;
use std::fs;
use std::path::Path;

use anyhow::Context;
use anyhow::Result;
use tracing::warn;

use super::RepairOutcome;
use super::TransactionKind;
use super::TransactionPaths;
use super::next_transaction_id;
use super::readable_path;
use super::recover_failed_replace;
use super::recover_interrupted_write;
use super::transaction_paths;
use super::write_new_file;
use super::write_transaction_marker;

pub(super) fn write_file_atomically(path: &Path, contents: &[u8]) -> Result<()> {
    recover_interrupted_write(path)?;
    let kind = if path.try_exists()? {
        TransactionKind::ReplaceExisting
    } else {
        TransactionKind::FirstPublish
    };
    let transaction = create_transaction(path, contents, kind)?;
    match kind {
        TransactionKind::FirstPublish => match move_file(&transaction.temp, path) {
            Ok(()) => {
                cleanup_committed_transaction(path);
                Ok(())
            }
            Err(error) => match recover_interrupted_write(path) {
                Ok(RepairOutcome::Committed) => Ok(()),
                Ok(outcome) => Err(error).with_context(|| {
                    format!(
                        "failed to publish first secrets file at {}; recovery left {outcome:?}",
                        path.display()
                    )
                }),
                Err(recovery_error) => anyhow::bail!(
                    "{error}; first-publish recovery for {} also failed: {recovery_error:#}",
                    path.display()
                ),
            },
        },
        TransactionKind::ReplaceExisting => {
            match replace_file(path, &transaction.temp, &transaction.backup) {
                Ok(()) => {
                    cleanup_committed_transaction(path);
                    Ok(())
                }
                Err(error) => match recover_failed_replace(path, &error) {
                    Ok(()) => Err(error).with_context(|| {
                        format!("failed to replace secrets file at {}", path.display())
                    }),
                    Err(recovery_error) => anyhow::bail!(
                        "{error}; replacement recovery for {} also failed: {recovery_error:#}",
                        path.display()
                    ),
                },
            }
        }
    }
}

fn create_transaction(
    path: &Path,
    contents: &[u8],
    kind: TransactionKind,
) -> Result<TransactionPaths> {
    let source = match kind {
        TransactionKind::FirstPublish => None,
        TransactionKind::ReplaceExisting => Some(
            fs::read(path)
                .with_context(|| format!("failed to read source file at {}", path.display()))?,
        ),
    };
    let transaction = transaction_paths(path, kind, &next_transaction_id())?;
    anyhow::ensure!(
        !transaction.temp.try_exists()?
            && !transaction.backup.try_exists()?
            && !transaction.marker.try_exists()?,
        "secrets transaction paths already exist for {}",
        path.display()
    );
    write_new_file(&transaction.temp, contents)?;
    if let Err(error) = write_transaction_marker(&transaction, source.as_deref(), contents) {
        match transaction.marker.try_exists() {
            Ok(true) => return Err(error),
            Err(inspect_error) => anyhow::bail!(
                "{error:#}; preserving {} because {} could not be inspected: {inspect_error}",
                transaction.temp.display(),
                transaction.marker.display()
            ),
            Ok(false) => {}
        }
        if let Err(cleanup_error) = fs::remove_file(&transaction.temp)
            && cleanup_error.kind() != std::io::ErrorKind::NotFound
        {
            anyhow::bail!(
                "{error:#}; failed to remove staged file {}: {cleanup_error}",
                transaction.temp.display()
            );
        }
        return Err(error);
    }
    if let Err(error) = readable_path(path) {
        if let Err(cleanup_error) = abort_staged_transaction(&transaction) {
            anyhow::bail!(
                "{error:#}; failed to abort staged transaction for {}: {cleanup_error:#}",
                path.display()
            );
        }
        return Err(error);
    }
    Ok(transaction)
}

fn abort_staged_transaction(transaction: &TransactionPaths) -> Result<()> {
    remove_staged_file(&transaction.marker)?;
    remove_staged_file(&transaction.temp)
}

fn remove_staged_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("failed to remove staged file {}", path.display()))
        }
    }
}

fn cleanup_committed_transaction(path: &Path) {
    if let Err(error) = recover_interrupted_write(path) {
        warn!(
            "secrets file was committed at {} but transaction cleanup failed: {error:#}",
            path.display()
        );
    }
}

fn replace_file(destination: &Path, temp: &Path, backup: &Path) -> std::io::Result<()> {
    unsafe extern "system" {
        fn ReplaceFileW(
            replaced_file_name: *const u16,
            replacement_file_name: *const u16,
            backup_file_name: *const u16,
            replace_flags: u32,
            exclude: *mut c_void,
            reserved: *mut c_void,
        ) -> i32;
    }

    let destination = wide_absolute_path(destination)?;
    let temp = wide_absolute_path(temp)?;
    let backup = wide_absolute_path(backup)?;
    let replaced = unsafe {
        ReplaceFileW(
            destination.as_ptr(),
            temp.as_ptr(),
            backup.as_ptr(),
            /*replace_flags*/ 0,
            /*exclude*/ std::ptr::null_mut(),
            /*reserved*/ std::ptr::null_mut(),
        )
    };
    if replaced == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn move_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    unsafe extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;

    let source = wide_absolute_path(source)?;
    let destination = wide_absolute_path(destination)?;
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            /*flags*/ MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn wide_absolute_path(path: &Path) -> std::io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;

    const SEPARATOR: u16 = b'\\' as u16;
    const VERBATIM_PREFIX: [u16; 4] = [SEPARATOR, SEPARATOR, b'?' as u16, SEPARATOR];
    const UNC_PREFIX: [u16; 8] = [
        SEPARATOR,
        SEPARATOR,
        b'?' as u16,
        SEPARATOR,
        b'U' as u16,
        b'N' as u16,
        b'C' as u16,
        SEPARATOR,
    ];

    let path = std::path::absolute(path)?;
    let wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if wide.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Windows paths cannot contain NUL",
        ));
    }
    let mut result = if wide.starts_with(&VERBATIM_PREFIX) {
        wide
    } else if wide.starts_with(&[SEPARATOR, SEPARATOR]) {
        UNC_PREFIX
            .into_iter()
            .chain(wide.into_iter().skip(2))
            .collect()
    } else {
        VERBATIM_PREFIX.into_iter().chain(wide).collect()
    };
    result.push(/*value*/ 0);
    Ok(result)
}
