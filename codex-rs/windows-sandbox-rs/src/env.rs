use anyhow::Result;
use anyhow::anyhow;
use dirs_next::home_dir;
use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::fs::{self};
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

pub fn normalize_null_device_env(env_map: &mut HashMap<String, String>) {
    let keys: Vec<String> = env_map.keys().cloned().collect();
    for k in keys {
        if let Some(v) = env_map.get(&k).cloned() {
            let t = v.trim().to_ascii_lowercase();
            if t == "/dev/null" || t == "\\\\\\\\dev\\\\\\\\null" {
                env_map.insert(k, "NUL".to_string());
            }
        }
    }
}

pub fn ensure_non_interactive_pager(env_map: &mut HashMap<String, String>) {
    env_map
        .entry("GIT_PAGER".into())
        .or_insert_with(|| "more.com".into());
    env_map
        .entry("PAGER".into())
        .or_insert_with(|| "more.com".into());
    env_map.entry("LESS".into()).or_insert_with(|| "".into());
}

// Keep PATH and PATHEXT stable for callers that rely on inheriting the parent process env.
pub fn inherit_path_env(env_map: &mut HashMap<String, String>) {
    if !env_map.contains_key("PATH")
        && let Ok(path) = env::var("PATH")
    {
        env_map.insert("PATH".into(), path);
    }
    if !env_map.contains_key("PATHEXT")
        && let Ok(pathext) = env::var("PATHEXT")
    {
        env_map.insert("PATHEXT".into(), pathext);
    }
}

/// Variables Windows expects to already exist in a new process environment.
///
/// The legacy sandbox builds a child environment from scratch, so a caller that
/// passes an empty (or nearly empty) map leaves the native loader, the .NET
/// host, and PowerShell without the roots they probe during startup, which
/// surfaces as module-loading failures (most visibly on Windows ARM, where the
/// emulation/native probe order makes the missing roots fatal rather than slow).
///
/// Deliberately excluded:
/// - `PATH`/`PATHEXT`: owned by [`inherit_path_env`] and the `inherit_path`
///   contract, which callers use to *deny* inheritance.
/// - `PSModulePath`: PowerShell derives it from the roots below; synthesizing it
///   would pin module resolution to whatever this process happens to see.
/// - Anything carrying credentials or network configuration.
const WINDOWS_STARTUP_ENV_FLOOR: &[&str] = &[
    // Native loader and system roots.
    "SystemRoot",
    "windir",
    "SystemDrive",
    "ComSpec",
    // Machine-wide install roots probed by the .NET host.
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "ProgramData",
    // Per-user roots used for profiles and .NET/CLR per-user state.
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    // Temp roots used for assembly and JIT scratch files.
    "TEMP",
    "TMP",
];

/// `windir` is the only floor variable outside the protocol-level core set; the
/// core set spells the same directory as `SYSTEMROOT`, but some loaders still
/// read the legacy name.
#[cfg(test)]
const WINDOWS_STARTUP_ENV_FLOOR_EXTRAS: &[&str] = &["windir"];

/// Fill in [`WINDOWS_STARTUP_ENV_FLOOR`] entries the caller did not provide.
///
/// Lookups and insertions are case-insensitive on the key, so a caller that set
/// `systemroot` (or any other casing) keeps its own value untouched.
pub fn ensure_windows_startup_env_floor(env_map: &mut HashMap<String, String>) {
    ensure_windows_startup_env_floor_from(env_map, |name| env::var(name).ok());
}

fn ensure_windows_startup_env_floor_from(
    env_map: &mut HashMap<String, String>,
    lookup: impl Fn(&str) -> Option<String>,
) {
    for name in WINDOWS_STARTUP_ENV_FLOOR {
        if env_map.keys().any(|key| key.eq_ignore_ascii_case(name)) {
            continue;
        }
        let Some(value) = lookup(name) else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        env_map.insert((*name).to_string(), value);
    }
}

fn prepend_path(env_map: &mut HashMap<String, String>, prefix: &str) {
    let existing = env_map
        .get("PATH")
        .cloned()
        .or_else(|| env::var("PATH").ok())
        .unwrap_or_default();
    let parts: Vec<String> = existing.split(';').map(ToString::to_string).collect();
    if parts
        .first()
        .map(|p| p.eq_ignore_ascii_case(prefix))
        .unwrap_or(false)
    {
        return;
    }
    let mut new_path = String::new();
    new_path.push_str(prefix);
    if !existing.is_empty() {
        new_path.push(';');
        new_path.push_str(&existing);
    }
    env_map.insert("PATH".into(), new_path);
}

fn reorder_pathext_for_stubs(env_map: &mut HashMap<String, String>) {
    let default = env_map
        .get("PATHEXT")
        .cloned()
        .or_else(|| env::var("PATHEXT").ok())
        .unwrap_or(".COM;.EXE;.BAT;.CMD".to_string());
    let exts: Vec<String> = default
        .split(';')
        .filter(|e| !e.is_empty())
        .map(ToString::to_string)
        .collect();
    let exts_norm: Vec<String> = exts.iter().map(|e| e.to_ascii_uppercase()).collect();
    let want = [".BAT", ".CMD"];
    let mut front: Vec<String> = Vec::new();
    for w in want {
        if let Some(idx) = exts_norm.iter().position(|e| e == w) {
            front.push(exts[idx].clone());
        }
    }
    let rest: Vec<String> = exts
        .into_iter()
        .enumerate()
        .filter(|(i, _)| {
            let up = &exts_norm[*i];
            up != ".BAT" && up != ".CMD"
        })
        .map(|(_, e)| e)
        .collect();
    let mut combined = Vec::new();
    combined.extend(front);
    combined.extend(rest);
    env_map.insert("PATHEXT".into(), combined.join(";"));
}

fn ensure_denybin(tools: &[&str], denybin_dir: Option<&Path>) -> Result<PathBuf> {
    let base = match denybin_dir {
        Some(p) => p.to_path_buf(),
        None => {
            let home = home_dir().ok_or_else(|| anyhow!("no home dir"))?;
            home.join(".sbx-denybin")
        }
    };
    fs::create_dir_all(&base)?;
    for tool in tools {
        for ext in [".bat", ".cmd"] {
            let path = base.join(format!("{tool}{ext}"));
            if !path.exists() {
                let mut f = File::create(&path)?;
                f.write_all(b"@echo off\\r\\nexit /b 1\\r\\n")?;
            }
        }
    }
    Ok(base)
}

pub fn apply_no_network_to_env(env_map: &mut HashMap<String, String>) -> Result<()> {
    env_map.insert("SBX_NONET_ACTIVE".into(), "1".into());
    env_map
        .entry("HTTP_PROXY".into())
        .or_insert_with(|| "http://127.0.0.1:9".into());
    env_map
        .entry("HTTPS_PROXY".into())
        .or_insert_with(|| "http://127.0.0.1:9".into());
    env_map
        .entry("ALL_PROXY".into())
        .or_insert_with(|| "http://127.0.0.1:9".into());
    env_map
        .entry("NO_PROXY".into())
        .or_insert_with(|| "localhost,127.0.0.1,::1".into());
    env_map
        .entry("PIP_NO_INDEX".into())
        .or_insert_with(|| "1".into());
    env_map
        .entry("PIP_DISABLE_PIP_VERSION_CHECK".into())
        .or_insert_with(|| "1".into());
    env_map
        .entry("NPM_CONFIG_OFFLINE".into())
        .or_insert_with(|| "true".into());
    env_map
        .entry("CARGO_NET_OFFLINE".into())
        .or_insert_with(|| "true".into());
    env_map
        .entry("GIT_HTTP_PROXY".into())
        .or_insert_with(|| "http://127.0.0.1:9".into());
    env_map
        .entry("GIT_HTTPS_PROXY".into())
        .or_insert_with(|| "http://127.0.0.1:9".into());
    env_map
        .entry("GIT_SSH_COMMAND".into())
        .or_insert_with(|| "cmd /c exit 1".into());
    env_map
        .entry("GIT_ALLOW_PROTOCOLS".into())
        .or_insert_with(|| "".into());

    let base = ensure_denybin(&["ssh", "scp"], /*denybin_dir*/ None)?;
    for tool in ["curl", "wget"] {
        for ext in [".bat", ".cmd"] {
            let p = base.join(format!("{tool}{ext}"));
            if p.exists() {
                let _ = fs::remove_file(&p);
            }
        }
    }
    prepend_path(env_map, &base.to_string_lossy());
    reorder_pathext_for_stubs(env_map);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::WINDOWS_STARTUP_ENV_FLOOR;
    use super::WINDOWS_STARTUP_ENV_FLOOR_EXTRAS;
    use super::ensure_windows_startup_env_floor_from;
    use codex_protocol::shell_environment::WINDOWS_CORE_ENV_VARS;
    use pretty_assertions::assert_eq;
    use std::collections::HashMap;

    fn fake_process_env(name: &str) -> Option<String> {
        match name {
            "SystemRoot" | "windir" => Some("C:\\Windows".to_string()),
            "SystemDrive" => Some("C:".to_string()),
            "ComSpec" => Some("C:\\Windows\\system32\\cmd.exe".to_string()),
            "ProgramFiles" | "ProgramW6432" => Some("C:\\Program Files".to_string()),
            "ProgramFiles(x86)" => Some("C:\\Program Files (x86)".to_string()),
            "ProgramData" => Some("C:\\ProgramData".to_string()),
            "USERPROFILE" => Some("C:\\Users\\runner".to_string()),
            "HOMEDRIVE" => Some("C:".to_string()),
            "HOMEPATH" => Some("\\Users\\runner".to_string()),
            "APPDATA" => Some("C:\\Users\\runner\\AppData\\Roaming".to_string()),
            "LOCALAPPDATA" => Some("C:\\Users\\runner\\AppData\\Local".to_string()),
            "TEMP" | "TMP" => Some("C:\\Users\\runner\\AppData\\Local\\Temp".to_string()),
            // Present in the real process env, but never part of the floor.
            "PATH" => Some("C:\\Windows\\system32".to_string()),
            "PATHEXT" => Some(".COM;.EXE".to_string()),
            "PSModulePath" => Some("C:\\leaked\\modules".to_string()),
            _ => None,
        }
    }

    #[test]
    fn startup_env_floor_populates_empty_map() {
        let mut env_map = HashMap::new();

        ensure_windows_startup_env_floor_from(&mut env_map, fake_process_env);

        let mut keys = env_map.keys().cloned().collect::<Vec<_>>();
        keys.sort();
        let mut expected = WINDOWS_STARTUP_ENV_FLOOR
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>();
        expected.sort();
        assert_eq!(keys, expected);
        assert_eq!(env_map.get("SystemRoot"), Some(&"C:\\Windows".to_string()));
        assert_eq!(
            env_map.get("ComSpec"),
            Some(&"C:\\Windows\\system32\\cmd.exe".to_string())
        );
    }

    #[test]
    fn startup_env_floor_never_fills_path_or_module_path() {
        let mut env_map = HashMap::new();

        ensure_windows_startup_env_floor_from(&mut env_map, fake_process_env);

        for excluded in ["PATH", "PATHEXT", "PSModulePath"] {
            assert!(
                !env_map.keys().any(|key| key.eq_ignore_ascii_case(excluded)),
                "{excluded} must stay owned by the caller, got {env_map:?}"
            );
        }
    }

    #[test]
    fn startup_env_floor_preserves_case_variant_caller_overrides() {
        let mut env_map = HashMap::from([
            ("systemroot".to_string(), "D:\\CallerWindows".to_string()),
            ("comspec".to_string(), "D:\\caller\\cmd.exe".to_string()),
            ("TeMp".to_string(), "D:\\caller-temp".to_string()),
            (
                "ProgramFiles(X86)".to_string(),
                "D:\\caller-x86".to_string(),
            ),
        ]);

        ensure_windows_startup_env_floor_from(&mut env_map, fake_process_env);

        assert_eq!(
            env_map.get("systemroot"),
            Some(&"D:\\CallerWindows".to_string())
        );
        assert_eq!(
            env_map.get("comspec"),
            Some(&"D:\\caller\\cmd.exe".to_string())
        );
        assert_eq!(env_map.get("TeMp"), Some(&"D:\\caller-temp".to_string()));
        assert_eq!(
            env_map.get("ProgramFiles(X86)"),
            Some(&"D:\\caller-x86".to_string())
        );
        for canonical in ["SystemRoot", "ComSpec", "TEMP", "ProgramFiles(x86)"] {
            assert_eq!(
                env_map.get(canonical),
                None,
                "floor must not add a second casing for {canonical}"
            );
        }
        // Absent keys are still filled while overrides are preserved.
        assert_eq!(
            env_map.get("LOCALAPPDATA"),
            Some(&"C:\\Users\\runner\\AppData\\Local".to_string())
        );
    }

    #[test]
    fn startup_env_floor_skips_absent_and_empty_process_values() {
        let mut env_map = HashMap::new();

        ensure_windows_startup_env_floor_from(&mut env_map, |name| match name {
            "SystemRoot" => Some("C:\\Windows".to_string()),
            "ComSpec" => Some(String::new()),
            _ => None,
        });

        assert_eq!(
            env_map,
            HashMap::from([("SystemRoot".to_string(), "C:\\Windows".to_string())])
        );
    }

    #[test]
    fn startup_env_floor_stays_within_protocol_core_env_vars() {
        for name in WINDOWS_STARTUP_ENV_FLOOR {
            let known = WINDOWS_CORE_ENV_VARS
                .iter()
                .chain(WINDOWS_STARTUP_ENV_FLOOR_EXTRAS.iter())
                .any(|allowed| allowed.eq_ignore_ascii_case(name));
            assert!(
                known,
                "{name} is not covered by the protocol core env allowlist"
            );
        }
    }
}
