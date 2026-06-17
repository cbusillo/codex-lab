use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

const PROFILE_METADATA_FILE: &str = "auth-profiles.json";
const PROFILE_DIR: &str = "auth-profiles";

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct AuthProfilesFile {
    pub version: u32,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_profile: Option<String>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, AuthProfileMetadata>,
}

impl AuthProfilesFile {
    fn new() -> Self {
        Self {
            version: 1,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct AuthProfileMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<DateTime<Utc>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_login_at: Option<DateTime<Utc>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priming_enabled: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthProfileEntry {
    pub name: String,
    pub home: PathBuf,
    pub metadata: AuthProfileMetadata,
}

pub fn profile_metadata_path(codex_home: &Path) -> PathBuf {
    codex_home.join(PROFILE_METADATA_FILE)
}

pub fn profile_home(codex_home: &Path, profile_name: &str) -> io::Result<PathBuf> {
    let safe_name = validate_profile_name(profile_name)?;
    Ok(codex_home.join(PROFILE_DIR).join(safe_name))
}

pub fn validate_profile_name(profile_name: &str) -> io::Result<&str> {
    let trimmed = profile_name.trim();
    if trimmed.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "auth profile name cannot be empty",
        ));
    }
    if trimmed != profile_name {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "auth profile name cannot start or end with whitespace",
        ));
    }
    if trimmed == "." || trimmed == ".." {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "auth profile name cannot be . or ..",
        ));
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "auth profile names may contain only ASCII letters, numbers, '-' and '_'",
        ));
    }
    Ok(trimmed)
}

pub fn load_auth_profiles(codex_home: &Path) -> io::Result<AuthProfilesFile> {
    let path = profile_metadata_path(codex_home);
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(AuthProfilesFile::new()),
        Err(err) => return Err(err),
    };
    let mut profiles: AuthProfilesFile = serde_json::from_str(&raw).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to parse {}: {err}", path.display()),
        )
    })?;
    if profiles.version == 0 {
        profiles.version = 1;
    }
    Ok(profiles)
}

pub fn save_auth_profiles(codex_home: &Path, profiles: &AuthProfilesFile) -> io::Result<()> {
    fs::create_dir_all(codex_home)?;
    let path = profile_metadata_path(codex_home);
    let raw = serde_json::to_string_pretty(profiles).map_err(io::Error::other)?;
    let (tmp_path, mut file) = create_metadata_tmp_file(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(raw.as_bytes())?;
    file.flush()?;
    drop(file);
    if let Err(err) = replace_metadata_file(&tmp_path, &path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }
    Ok(())
}

fn create_metadata_tmp_file(path: &Path) -> io::Result<(PathBuf, fs::File)> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(PROFILE_METADATA_FILE);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    for attempt in 0..100 {
        let tmp_path = parent.join(format!(
            ".{file_name}.{}.{}.tmp",
            std::process::id(),
            attempt
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&tmp_path) {
            Ok(file) => return Ok((tmp_path, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "failed to allocate temporary metadata path for {}",
            path.display()
        ),
    ))
}

fn replace_metadata_file(src: &Path, dst: &Path) -> io::Result<()> {
    #[cfg(not(windows))]
    {
        fs::rename(src, dst)
    }
    #[cfg(windows)]
    {
        match replace_file_windows(src, dst) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => fs::rename(src, dst),
            Err(err) => Err(err),
        }
    }
}

#[cfg(windows)]
fn replace_file_windows(src: &Path, dst: &Path) -> io::Result<()> {
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

    let dst_wide: Vec<u16> = dst.as_os_str().encode_wide().chain(Some(0)).collect();
    let src_wide: Vec<u16> = src.as_os_str().encode_wide().chain(Some(0)).collect();
    let replaced = unsafe {
        ReplaceFileW(
            dst_wide.as_ptr(),
            src_wide.as_ptr(),
            std::ptr::null(),
            REPLACEFILE_IGNORE_MERGE_ERRORS,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if replaced == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn upsert_auth_profile(
    codex_home: &Path,
    profile_name: &str,
    update: impl FnOnce(&mut AuthProfileMetadata),
) -> io::Result<AuthProfileEntry> {
    let profile_name = validate_profile_name(profile_name)?.to_string();
    let mut profiles = load_auth_profiles(codex_home)?;
    profiles.version = 1;
    let metadata = profiles.profiles.entry(profile_name.clone()).or_default();
    update(metadata);
    if profiles.active_profile.is_none() {
        profiles.active_profile = Some(profile_name.clone());
    }
    save_auth_profiles(codex_home, &profiles)?;
    Ok(AuthProfileEntry {
        home: profile_home(codex_home, &profile_name)?,
        metadata: profiles
            .profiles
            .get(&profile_name)
            .cloned()
            .unwrap_or_default(),
        name: profile_name,
    })
}

pub fn record_auth_profile_login(
    codex_home: &Path,
    profile_name: &str,
    account_id: Option<String>,
    email: Option<String>,
) -> io::Result<AuthProfileEntry> {
    let now = Utc::now();
    upsert_auth_profile(codex_home, profile_name, |metadata| {
        metadata.last_login_at = Some(now);
        metadata.last_used_at = Some(now);
        metadata.account_id = account_id;
        metadata.email = email;
    })
}

pub fn list_auth_profiles(codex_home: &Path) -> io::Result<Vec<AuthProfileEntry>> {
    let profiles = load_auth_profiles(codex_home)?;
    profiles
        .profiles
        .into_iter()
        .map(|(name, metadata)| {
            Ok(AuthProfileEntry {
                home: profile_home(codex_home, &name)?,
                metadata,
                name,
            })
        })
        .collect()
}

pub fn remove_auth_profile_metadata(codex_home: &Path, profile_name: &str) -> io::Result<bool> {
    let profile_name = validate_profile_name(profile_name)?.to_string();
    let mut profiles = load_auth_profiles(codex_home)?;
    let removed = profiles.profiles.remove(&profile_name).is_some();
    if !removed {
        return Ok(false);
    }
    if profiles.active_profile.as_deref() == Some(&profile_name) {
        profiles.active_profile = profiles.profiles.keys().next().cloned();
    }
    save_auth_profiles(codex_home, &profiles)?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn profile_home_rejects_path_like_names() {
        let temp = TempDir::new().expect("tempdir");

        assert!(profile_home(temp.path(), "../secret").is_err());
        assert!(profile_home(temp.path(), "work/account").is_err());
        assert!(profile_home(temp.path(), " work").is_err());
    }

    #[test]
    fn upsert_and_list_profiles_round_trip_metadata() {
        let temp = TempDir::new().expect("tempdir");

        let entry = upsert_auth_profile(temp.path(), "work", |metadata| {
            metadata.email = Some("me@example.com".to_string());
            metadata.priming_enabled = Some(true);
        })
        .expect("upsert profile");

        assert_eq!(entry.name, "work");
        assert_eq!(entry.home, temp.path().join("auth-profiles").join("work"));

        let profiles = list_auth_profiles(temp.path()).expect("list profiles");
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "work");
        assert_eq!(
            profiles[0].metadata.email.as_deref(),
            Some("me@example.com")
        );
        assert_eq!(profiles[0].metadata.priming_enabled, Some(true));
    }

    #[test]
    fn save_profiles_replaces_existing_metadata() {
        let temp = TempDir::new().expect("tempdir");

        upsert_auth_profile(temp.path(), "work", |metadata| {
            metadata.email = Some("work@example.com".to_string());
        })
        .expect("first upsert");
        upsert_auth_profile(temp.path(), "work", |metadata| {
            metadata.email = Some("updated@example.com".to_string());
        })
        .expect("second upsert");

        let profiles = load_auth_profiles(temp.path()).expect("load profiles");
        assert_eq!(profiles.profiles.len(), 1);
        assert_eq!(
            profiles.profiles["work"].email.as_deref(),
            Some("updated@example.com")
        );
    }

    #[cfg(unix)]
    #[test]
    fn saved_profiles_metadata_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().expect("tempdir");

        upsert_auth_profile(temp.path(), "work", |_| {}).expect("upsert profile");

        let mode = fs::metadata(profile_metadata_path(temp.path()))
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn saved_profiles_metadata_restricts_stale_tmp_file() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().expect("tempdir");
        let tmp_path = temp
            .path()
            .join(format!(".auth-profiles.json.{}.0.tmp", std::process::id()));
        fs::write(&tmp_path, "{}").expect("write stale tmp");
        fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o644))
            .expect("make stale tmp permissive");

        upsert_auth_profile(temp.path(), "work", |_| {}).expect("upsert profile");

        let mode = fs::metadata(profile_metadata_path(temp.path()))
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        assert!(tmp_path.exists());
    }

    #[test]
    fn remove_profile_metadata_updates_active_profile() {
        let temp = TempDir::new().expect("tempdir");

        upsert_auth_profile(temp.path(), "work", |_| {}).expect("upsert work profile");
        upsert_auth_profile(temp.path(), "backup", |_| {}).expect("upsert backup profile");

        assert!(
            remove_auth_profile_metadata(temp.path(), "work").expect("remove profile metadata")
        );

        let profiles = load_auth_profiles(temp.path()).expect("load profiles");
        assert_eq!(profiles.active_profile.as_deref(), Some("backup"));
        assert!(!profiles.profiles.contains_key("work"));
        assert!(profiles.profiles.contains_key("backup"));
    }
}
