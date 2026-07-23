use anyhow::Context;
use anyhow::Result;

use super::LocalSecretsBackend;
use super::atomic_file;

impl LocalSecretsBackend {
    pub(super) fn recover_windows_atomic_write(&self) -> Result<()> {
        let path = self.secrets_path();
        atomic_file::recover_interrupted_write(&path).with_context(|| {
            format!(
                "failed to recover interrupted secrets replacement at {}",
                path.display()
            )
        })?;
        Ok(())
    }
}
