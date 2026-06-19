use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use codex_utils_path::write_atomically;
use serde::Deserialize;
use serde::Serialize;

use crate::scoped_review_root;

const LOCK_FILENAME: &str = "review.lock";
const EPOCH_FILENAME: &str = "snapshot.epoch";
const EPOCH_LOCK_FILENAME: &str = "snapshot.epoch.lock";
const MALFORMED_LOCK_STALE_SECS: u64 = 10 * 60;
const EPOCH_LOCK_STALE_SECS: u64 = 10 * 60;
const CLEANUP_LOCK_STALE_SECS: u64 = 10 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewLockInfo {
    pub pid: u32,
    pub started_at_unix_secs: u64,
    pub intent: String,
    pub git_head: Option<String>,
    pub snapshot_epoch: u64,
    #[serde(default)]
    pub owner_id: String,
}

#[derive(Debug, Clone)]
pub struct ReviewCoordination {
    root: PathBuf,
    scope: PathBuf,
}

#[derive(Debug)]
pub struct ReviewLockGuard {
    lock_path: PathBuf,
    owner_id: String,
}

impl ReviewCoordination {
    pub fn for_scope(codex_home: impl AsRef<Path>, scope: impl AsRef<Path>) -> Self {
        let scope = scope.as_ref().to_path_buf();
        Self {
            root: scoped_review_root(codex_home.as_ref(), &scope),
            scope,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn current_snapshot_epoch(&self) -> Result<u64> {
        match fs::read_to_string(self.epoch_path()) {
            Ok(text) => text.trim().parse::<u64>().with_context(|| {
                format!(
                    "failed to parse snapshot epoch {}",
                    self.epoch_path().display()
                )
            }),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(err) => Err(err).with_context(|| {
                format!(
                    "failed to read snapshot epoch {}",
                    self.epoch_path().display()
                )
            }),
        }
    }

    pub fn bump_snapshot_epoch(&self) -> Result<u64> {
        let _guard = self.try_acquire_epoch_lock()?;
        let current = self.current_snapshot_epoch()?;
        let next = current.saturating_add(1);
        self.write_snapshot_epoch(next)?;
        Ok(next)
    }

    pub fn publish_next_snapshot_epoch_after<F>(&self, publish: F) -> Result<Option<u64>>
    where
        F: FnOnce(u64) -> bool,
    {
        let _guard = self.try_acquire_epoch_lock()?;
        let current = self.current_snapshot_epoch()?;
        let next = current.saturating_add(1);
        if !publish(next) {
            return Ok(None);
        }
        self.write_snapshot_epoch(next)?;
        Ok(Some(next))
    }

    pub fn try_acquire_lock(&self, intent: impl Into<String>) -> Result<Option<ReviewLockGuard>> {
        fs::create_dir_all(&self.root).with_context(|| {
            format!("failed to create review state dir {}", self.root.display())
        })?;

        let lock_path = self.lock_path();
        if lock_path.exists() {
            let _ = self.clear_stale_lock_if_dead()?;
        }
        let owner_id = new_owner_id();
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path);

        let mut file = match file {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => return Ok(None),
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("failed to create review lock {}", lock_path.display())
                });
            }
        };

        let info = match self.lock_info(intent.into(), owner_id.clone()) {
            Ok(info) => info,
            Err(err) => {
                let _ = fs::remove_file(&lock_path);
                return Err(err);
            }
        };
        let body = serde_json::to_string_pretty(&info)?;
        if let Err(err) = file.write_all(format!("{body}\n").as_bytes()) {
            let _ = fs::remove_file(&lock_path);
            return Err(err)
                .with_context(|| format!("failed to write review lock {}", lock_path.display()));
        }
        Ok(Some(ReviewLockGuard {
            lock_path,
            owner_id,
        }))
    }

    fn lock_info(&self, intent: String, owner_id: String) -> Result<ReviewLockInfo> {
        Ok(ReviewLockInfo {
            pid: std::process::id(),
            started_at_unix_secs: now_unix_secs().unwrap_or_default(),
            intent,
            git_head: git_head(&self.scope),
            snapshot_epoch: self.current_snapshot_epoch()?,
            owner_id,
        })
    }

    pub fn read_lock_info(&self) -> Result<Option<ReviewLockInfo>> {
        match fs::read_to_string(self.lock_path()) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| {
                    format!("failed to parse review lock {}", self.lock_path().display())
                })
                .map(Some),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err).with_context(|| {
                format!("failed to read review lock {}", self.lock_path().display())
            }),
        }
    }

    pub fn clear_stale_lock_if_dead(&self) -> Result<bool> {
        let lock_path = self.lock_path();
        let _cleanup_guard = try_acquire_cleanup_lock(&lock_path)?;
        let text = match fs::read_to_string(&lock_path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("failed to read review lock {}", lock_path.display())
                });
            }
        };
        let Ok(info) = serde_json::from_str::<ReviewLockInfo>(&text) else {
            return self.clear_malformed_lock_if_stale(&lock_path);
        };
        if pid_alive(info.pid) {
            return Ok(false);
        }
        match fs::remove_file(&lock_path) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err).with_context(|| {
                format!("failed to remove stale review lock {}", lock_path.display())
            }),
        }
    }

    pub(crate) fn lock_path(&self) -> PathBuf {
        self.root.join(LOCK_FILENAME)
    }

    pub(crate) fn epoch_path(&self) -> PathBuf {
        self.root.join(EPOCH_FILENAME)
    }

    fn write_snapshot_epoch(&self, next: u64) -> Result<()> {
        write_atomically(&self.epoch_path(), &format!("{next}\n")).with_context(|| {
            format!(
                "failed to write snapshot epoch {}",
                self.epoch_path().display()
            )
        })
    }

    fn try_acquire_epoch_lock(&self) -> Result<EpochLockGuard> {
        fs::create_dir_all(&self.root).with_context(|| {
            format!("failed to create review state dir {}", self.root.display())
        })?;
        let lock_path = self.root.join(EPOCH_LOCK_FILENAME);
        loop {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(_) => return Ok(EpochLockGuard { lock_path }),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    let _ = clear_stale_path_if_old(&lock_path, EPOCH_LOCK_STALE_SECS)?;
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(err) => {
                    return Err(err).with_context(|| {
                        format!(
                            "failed to create snapshot epoch lock {}",
                            lock_path.display()
                        )
                    });
                }
            }
        }
    }

    fn clear_malformed_lock_if_stale(&self, lock_path: &Path) -> Result<bool> {
        clear_stale_path_if_old_unlocked(lock_path, MALFORMED_LOCK_STALE_SECS)
    }
}

struct EpochLockGuard {
    lock_path: PathBuf,
}

struct CleanupLockGuard {
    lock_path: PathBuf,
}

impl Drop for EpochLockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lock_path);
    }
}

impl Drop for CleanupLockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lock_path);
    }
}

impl Drop for ReviewLockGuard {
    fn drop(&mut self) {
        let Ok(text) = fs::read_to_string(&self.lock_path) else {
            return;
        };
        let Ok(info) = serde_json::from_str::<ReviewLockInfo>(&text) else {
            return;
        };
        if info.owner_id == self.owner_id {
            let _ = fs::remove_file(&self.lock_path);
        }
    }
}

fn git_head(cwd: &Path) -> Option<String> {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn new_owner_id() -> String {
    let nanos = now_unix_nanos().unwrap_or_default();
    format!("{}-{nanos}", std::process::id())
}

fn now_unix_secs() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

fn now_unix_nanos() -> Option<u64> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(nanos.min(u128::from(u64::MAX)) as u64)
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    let res = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if res == 0 {
        return true;
    }
    let err = io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::ESRCH);
    err != libc::ESRCH
}

#[cfg(not(unix))]
fn pid_alive(pid: u32) -> bool {
    platform_pid_alive(pid)
}

#[cfg(windows)]
fn platform_pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Foundation::STILL_ACTIVE;
    use windows_sys::Win32::System::Threading::GetExitCodeProcess;
    use windows_sys::Win32::System::Threading::OpenProcess;
    use windows_sys::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION;

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle == 0 {
        return false;
    }
    let mut code = 0;
    let ok = unsafe { GetExitCodeProcess(handle, &mut code) };
    unsafe { CloseHandle(handle) };
    ok != 0 && code == STILL_ACTIVE
}

#[cfg(not(any(unix, windows)))]
fn platform_pid_alive(pid: u32) -> bool {
    pid == std::process::id()
}

fn clear_stale_path_if_old(path: &Path, stale_secs: u64) -> Result<bool> {
    let _cleanup_guard = try_acquire_cleanup_lock(path)?;
    clear_stale_path_if_old_unlocked(path, stale_secs)
}

fn clear_stale_path_if_old_unlocked(path: &Path, stale_secs: u64) -> Result<bool> {
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(false);
    };
    let Ok(modified) = metadata.modified() else {
        return Ok(false);
    };
    if !modified
        .elapsed()
        .is_ok_and(|elapsed| elapsed.as_secs() >= stale_secs)
    {
        return Ok(false);
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => {
            Err(err).with_context(|| format!("failed to remove stale path {}", path.display()))
        }
    }
}

fn try_acquire_cleanup_lock(target_path: &Path) -> Result<CleanupLockGuard> {
    let cleanup_path = cleanup_lock_path(target_path);
    loop {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&cleanup_path)
        {
            Ok(_) => {
                return Ok(CleanupLockGuard {
                    lock_path: cleanup_path,
                });
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                let _ = clear_stale_path_if_old_unlocked(&cleanup_path, CLEANUP_LOCK_STALE_SECS)?;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "failed to create review cleanup lock {}",
                        cleanup_path.display()
                    )
                });
            }
        }
    }
}

fn cleanup_lock_path(target_path: &Path) -> PathBuf {
    let file_name = target_path
        .file_name()
        .map(|file_name| file_name.to_string_lossy())
        .unwrap_or_default();
    target_path.with_file_name(format!("{file_name}.cleanup"))
}

#[cfg(test)]
#[path = "review_coord_tests.rs"]
mod tests;
