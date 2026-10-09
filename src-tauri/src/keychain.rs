//! macOS Keychain-backed secret store (generic passwords).
//!
//! Development builds are unsigned, so macOS may ask to allow access after a
//! rebuild changes the binary; signed release builds keep a stable identity.

use fleet::engine::SecretStore;

pub struct Keychain;

/// The Keychain service that GitHub tokens are stored under, one item per organization.
#[cfg(target_os = "macos")]
const SERVICE: &str = "dev.runnerdashboard.github-token";

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use security_framework::base::Error;
    use security_framework::passwords::{delete_generic_password, get_generic_password, set_generic_password};

    const ITEM_NOT_FOUND: i32 = -25300;

    impl SecretStore for Keychain {
        fn get(&self, account: &str) -> Result<Option<String>, String> {
            match get_generic_password(SERVICE, account) {
                Ok(bytes) => String::from_utf8(bytes)
                    .map(Some)
                    .map_err(|_| "Keychain item is not valid UTF-8".to_string()),
                Err(e) if e.code() == ITEM_NOT_FOUND => Ok(None),
                Err(e) => Err(describe(e)),
            }
        }

        fn set(&self, account: &str, value: &str) -> Result<(), String> {
            set_generic_password(SERVICE, account, value.as_bytes()).map_err(describe)
        }

        fn delete(&self, account: &str) -> Result<(), String> {
            match delete_generic_password(SERVICE, account) {
                Ok(()) => Ok(()),
                Err(e) if e.code() == ITEM_NOT_FOUND => Ok(()),
                Err(e) => Err(describe(e)),
            }
        }
    }

    fn describe(e: Error) -> String {
        format!("Keychain error {}: {}", e.code(), e.message().unwrap_or_default())
    }
}

#[cfg(not(target_os = "macos"))]
impl SecretStore for Keychain {
    fn get(&self, _: &str) -> Result<Option<String>, String> {
        Err("Secure storage is only implemented for macOS".into())
    }
    fn set(&self, _: &str, _: &str) -> Result<(), String> {
        Err("Secure storage is only implemented for macOS".into())
    }
    fn delete(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
}
