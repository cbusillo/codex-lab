use anyhow::Result;

use super::LocalSecretsBackend;
use super::LockMode;
use super::SecretMutation;
use super::SecretName;
use super::SecretScope;

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
