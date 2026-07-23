use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

static NEXT_AUTH_TEMP_ID: AtomicU64 = AtomicU64::new(0);

pub(super) fn write_auth_file_atomically(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let (temp_path, mut file) = create_auth_temp_file(path)?;
    let write_result = file.write_all(contents).and_then(|()| file.flush());
    drop(file);
    if let Err(err) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(err);
    }
    if let Err(err) = replace_auth_file(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(err);
    }
    Ok(())
}

fn create_auth_temp_file(path: &Path) -> std::io::Result<(PathBuf, File)> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("auth path has no parent: {}", path.display()),
        )
    })?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("auth.json");

    for _ in 0..100 {
        let sequence = NEXT_AUTH_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let temp_path = parent.join(format!(
            ".{file_name}.{}.{}.tmp",
            std::process::id(),
            sequence
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
        }
        match options.open(&temp_path) {
            Ok(file) => return Ok((temp_path, file)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!(
            "failed to allocate temporary auth path for {}",
            path.display()
        ),
    ))
}

fn replace_auth_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(not(windows))]
    {
        fs::rename(source, destination)
    }
    #[cfg(windows)]
    {
        match replace_auth_file_windows(source, destination) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                fs::rename(source, destination)
            }
            Err(err) => Err(err),
        }
    }
}

#[cfg(windows)]
fn replace_auth_file_windows(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    const REPLACEFILE_IGNORE_MERGE_ERRORS: u32 = 0x0000_0002;

    unsafe extern "system" {
        fn ReplaceFileW(
            lp_replaced_file_name: *const u16,
            lp_replacement_file_name: *const u16,
            lp_backup_file_name: *const u16,
            dw_replace_flags: u32,
            lp_exclude: *mut c_void,
            lp_reserved: *mut c_void,
        ) -> i32;
    }

    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let source_wide: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let replaced = unsafe {
        ReplaceFileW(
            destination_wide.as_ptr(),
            source_wide.as_ptr(),
            std::ptr::null(),
            REPLACEFILE_IGNORE_MERGE_ERRORS,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if replaced == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
