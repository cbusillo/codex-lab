use std::cell::RefCell;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;

use super::LocalSecretsBackend;
use super::LockMode;
use super::SecretMutation;
use super::SecretName;
use super::SecretScope;

std::thread_local! {
    static ACTIVE_MUTATION_CALLBACKS: RefCell<HashSet<PathBuf>> = RefCell::new(HashSet::new());
}

pub(super) fn ensure_backend_access_allowed(lock_path: &Path) -> Result<()> {
    let has_active_callback = ACTIVE_MUTATION_CALLBACKS.with(|active| !active.borrow().is_empty());
    if !has_active_callback {
        return Ok(());
    }
    let lock_identity = canonical_lock_identity(lock_path)?;
    ACTIVE_MUTATION_CALLBACKS.with(|active| {
        anyhow::ensure!(
            !active.borrow().contains(&lock_identity),
            "secret mutation callback must not access the same secrets backend"
        );
        Ok(())
    })
}

struct MutationCallbackGuard {
    lock_path: PathBuf,
}

impl MutationCallbackGuard {
    fn enter(lock_path: PathBuf) -> Result<Self> {
        let lock_path = canonical_lock_identity(&lock_path)?;
        ACTIVE_MUTATION_CALLBACKS.with(|active| {
            anyhow::ensure!(
                active.borrow_mut().insert(lock_path.clone()),
                "secret mutation callback must not re-enter the same secrets backend"
            );
            Ok(Self { lock_path })
        })
    }
}

fn canonical_lock_identity(lock_path: &Path) -> Result<PathBuf> {
    let mut existing_ancestor = if lock_path.is_absolute() {
        lock_path.to_path_buf()
    } else {
        std::env::current_dir()?.join(lock_path)
    };
    let mut missing_components = Vec::new();

    loop {
        match fs::canonicalize(&existing_ancestor) {
            Ok(mut canonical) => {
                for component in missing_components.iter().rev() {
                    canonical.push(component);
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let component = existing_ancestor.file_name().ok_or_else(|| {
                    anyhow::anyhow!(
                        "failed to resolve secrets lock identity at {}",
                        lock_path.display()
                    )
                })?;
                missing_components.push(component.to_os_string());
                existing_ancestor = existing_ancestor
                    .parent()
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "failed to resolve secrets lock identity at {}",
                            lock_path.display()
                        )
                    })?
                    .to_path_buf();
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to resolve secrets lock identity at {}",
                        lock_path.display()
                    )
                });
            }
        }
    }
}

impl Drop for MutationCallbackGuard {
    fn drop(&mut self) {
        ACTIVE_MUTATION_CALLBACKS.with(|active| {
            let removed = active.borrow_mut().remove(&self.lock_path);
            debug_assert!(removed);
        });
    }
}

impl LocalSecretsBackend {
    pub fn mutate(
        &self,
        scope: &SecretScope,
        name: &SecretName,
        mutator: &mut dyn FnMut(Option<&str>) -> Result<SecretMutation>,
    ) -> Result<bool> {
        let _lock = self.acquire_lock(LockMode::Exclusive)?;
        #[cfg(windows)]
        self.recover_windows_atomic_write()?;

        let canonical_key = scope.canonical_key(name);
        let mut file = self.load_file()?;
        let mutation = {
            let current = file.secrets.get(&canonical_key).map(String::as_str);
            let _callback_guard = MutationCallbackGuard::enter(self.lock_path())?;
            mutator(current)?
        };

        match mutation {
            SecretMutation::Keep => Ok(false),
            SecretMutation::Set(value) => {
                anyhow::ensure!(!value.is_empty(), "secret value must not be empty");
                if file.secrets.get(&canonical_key) == Some(&value) {
                    return Ok(false);
                }
                file.secrets.insert(canonical_key, value);
                self.save_file(&file)?;
                Ok(true)
            }
            SecretMutation::Delete => {
                if file.secrets.remove(&canonical_key).is_none() {
                    return Ok(false);
                }
                self.save_file(&file)?;
                Ok(true)
            }
        }
    }
}
