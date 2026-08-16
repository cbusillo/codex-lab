use std::fs;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use codex_protocol::ThreadId;
use serde::Deserialize;
use serde::Serialize;
use tracing::warn;

pub(super) const LEGACY_LEASE_VERSION: u32 = 1;
pub(super) const LEASE_VERSION: u32 = 2;
const LEASE_SUBDIR: &str = "execution-account-leases";

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct PersistedExecutionAccountLease {
    pub(super) version: u32,
    pub(super) account_id: Option<String>,
    pub(super) base_cache_identity: Option<String>,
}

pub(super) fn lease_path(codex_home: &Path, thread_id: ThreadId) -> PathBuf {
    codex_home
        .join(LEASE_SUBDIR)
        .join(format!("{thread_id}.json"))
}

pub(super) fn read_persisted_lease_record(
    codex_home: &Path,
    thread_id: ThreadId,
) -> Option<PersistedExecutionAccountLease> {
    let path = lease_path(codex_home, thread_id);
    let contents = match fs::read(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => {
            warn!(path = %path.display(), "failed to read execution account lease: {error}");
            return None;
        }
    };
    let persisted: PersistedExecutionAccountLease = match serde_json::from_slice(&contents) {
        Ok(persisted) => persisted,
        Err(error) => {
            warn!(path = %path.display(), "ignoring corrupt execution account lease: {error}");
            return None;
        }
    };
    if !matches!(persisted.version, LEGACY_LEASE_VERSION | LEASE_VERSION)
        || persisted
            .account_id
            .as_deref()
            .is_some_and(|account_id| account_id.trim().is_empty())
        || persisted
            .base_cache_identity
            .as_deref()
            .is_some_and(|identity| identity.trim().is_empty())
        || (persisted.version == LEGACY_LEASE_VERSION && persisted.account_id.is_none())
        || (persisted.version == LEASE_VERSION && persisted.base_cache_identity.is_none())
    {
        warn!(path = %path.display(), "ignoring invalid execution account lease");
        return None;
    }
    Some(persisted)
}

pub(super) fn persist_lease_record(
    codex_home: &Path,
    thread_id: ThreadId,
    account_id: Option<&str>,
    base_cache_identity: &str,
) -> io::Result<()> {
    let path = lease_path(codex_home, thread_id);
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("lease path {} has no parent", path.display()),
        )
    })?;
    fs::create_dir_all(parent)?;
    let persisted = PersistedExecutionAccountLease {
        version: LEASE_VERSION,
        account_id: account_id.map(str::to_string),
        base_cache_identity: Some(base_cache_identity.to_string()),
    };
    let mut contents = serde_json::to_vec_pretty(&persisted).map_err(io::Error::other)?;
    contents.push(b'\n');
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(&contents)?;
    file.as_file().sync_all()?;
    file.persist(&path).map_err(|error| error.error)?;
    Ok(())
}
