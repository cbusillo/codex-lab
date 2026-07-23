use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use codex_config::types::AuthCredentialsStoreMode;

use super::storage::AuthDotJson;
use super::storage::AuthStorageBackend;

#[derive(Clone)]
pub(super) struct CatalogAccountStorage {
    codex_home: PathBuf,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    catalog_id: String,
}

impl CatalogAccountStorage {
    pub(super) fn new(
        codex_home: PathBuf,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        catalog_id: String,
    ) -> Arc<dyn AuthStorageBackend> {
        Arc::new(Self {
            codex_home,
            auth_credentials_store_mode,
            catalog_id,
        })
    }
}

impl fmt::Debug for CatalogAccountStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogAccountStorage")
            .field("catalog_id", &self.catalog_id)
            .finish_non_exhaustive()
    }
}

impl AuthStorageBackend for CatalogAccountStorage {
    fn load(&self) -> std::io::Result<Option<AuthDotJson>> {
        let account = crate::auth_accounts::find_account(
            &self.codex_home,
            self.auth_credentials_store_mode,
            &self.catalog_id,
        )?;
        if account.is_none() {
            return Ok(None);
        }
        let (_account, auth) = crate::auth_accounts::auth_for_account(
            &self.codex_home,
            self.auth_credentials_store_mode,
            &self.catalog_id,
        )?;
        Ok(Some(auth))
    }

    fn save(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        crate::auth_accounts::update_catalog_account_from_auth(
            &self.codex_home,
            self.auth_credentials_store_mode,
            &self.catalog_id,
            auth,
        )
    }

    fn delete(&self) -> std::io::Result<bool> {
        Ok(false)
    }
}
