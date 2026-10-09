//! Where GitHub tokens live. The app backs [`SecretStore`] with the macOS Keychain.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::sync::MutexExt;

/// Where GitHub tokens live, keyed by organization login. Calls may block, so
/// the engine makes them off the async runtime.
pub trait SecretStore: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<String>, String>;
    fn set(&self, account: &str, value: &str) -> Result<(), String>;
    fn delete(&self, account: &str) -> Result<(), String>;
}

#[derive(Default)]
pub struct MemorySecrets(Mutex<HashMap<String, String>>);

/// Secret values are never shown.
impl std::fmt::Debug for MemorySecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.0.lock().map_or(0, |m| m.len());
        f.debug_struct("MemorySecrets").field("count", &count).finish()
    }
}

impl SecretStore for MemorySecrets {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        Ok(self.0.locked().get(account).cloned())
    }
    fn set(&self, account: &str, value: &str) -> Result<(), String> {
        self.0.locked().insert(account.into(), value.into());
        Ok(())
    }
    fn delete(&self, account: &str) -> Result<(), String> {
        self.0.locked().remove(account);
        Ok(())
    }
}
