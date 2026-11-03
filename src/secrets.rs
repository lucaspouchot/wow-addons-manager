//! Where the CurseForge API key comes from.
//!
//! Order: `WOWADDONS_CURSEFORGE_API_KEY`, then the operating system's secret store
//! (Windows Credential Manager, macOS Keychain, Secret Service on Linux).
//! The key is never written to the config file or the data dir.

use serde::Serialize;

use crate::net::{ApiKey, ENV_API_KEY};

pub const KEYRING_SERVICE: &str = "wowaddons";
pub const KEYRING_USER: &str = "curseforge-api-key";
/// Set to `1` to never use the system secret store (headless machines, CI, tests).
pub const ENV_NO_KEYRING: &str = "WOWADDONS_NO_KEYRING";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeySource {
    Environment,
    Keyring,
}

impl KeySource {
    pub fn describe(self, store: &dyn SecretStore) -> String {
        match self {
            KeySource::Environment => format!("environment variable {ENV_API_KEY}"),
            KeySource::Keyring => store.name().to_string(),
        }
    }
}

/// A place that can hold one secret: the API key.
pub trait SecretStore {
    /// Human-readable name, e.g. "Windows Credential Manager".
    fn name(&self) -> &'static str;
    fn get(&self) -> Result<Option<String>, String>;
    fn set(&self, secret: &str) -> Result<(), String>;
    /// Returns `false` when there was nothing to delete.
    fn delete(&self) -> Result<bool, String>;
}

/// The operating system's secret store.
pub struct OsKeyring;

/// The secret store is turned off with `WOWADDONS_NO_KEYRING`.
pub struct DisabledStore;

pub fn os_store() -> Box<dyn SecretStore> {
    let disabled = std::env::var(ENV_NO_KEYRING)
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false);
    if disabled {
        Box::new(DisabledStore)
    } else {
        Box::new(OsKeyring)
    }
}

/// Error messages that never include secret data (some keyring errors carry the raw bytes).
fn describe(e: &keyring::Error) -> String {
    match e {
        keyring::Error::NoEntry => "no key is stored".into(),
        keyring::Error::NoStorageAccess(inner) => {
            format!("the secret store is locked or not reachable ({inner})")
        }
        keyring::Error::PlatformFailure(inner) => format!("secret store error ({inner})"),
        keyring::Error::NoDefaultStore => "no secret store is available on this system".into(),
        keyring::Error::BadEncoding(_) | keyring::Error::BadDataFormat(..) => {
            "the stored key is not valid text".into()
        }
        keyring::Error::TooLong(..) => "the key is too long for the secret store".into(),
        _ => "secret store error".into(),
    }
}

/// Run a keyring call on its own thread: some stores drive their own async
/// executor and must not run inside the tokio runtime's threads.
fn on_thread<T: Send>(f: impl FnOnce() -> Result<T, String> + Send) -> Result<T, String> {
    std::thread::scope(|s| {
        s.spawn(f)
            .join()
            .unwrap_or_else(|_| Err("secret store error".into()))
    })
}

fn entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).map_err(|e| describe(&e))
}

impl SecretStore for OsKeyring {
    fn name(&self) -> &'static str {
        if cfg!(windows) {
            "Windows Credential Manager"
        } else if cfg!(target_os = "macos") {
            "macOS Keychain"
        } else {
            "Secret Service (GNOME Keyring / KWallet)"
        }
    }

    fn get(&self) -> Result<Option<String>, String> {
        on_thread(|| match entry()?.get_password() {
            Ok(p) => Ok(Some(p)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(describe(&e)),
        })
    }

    fn set(&self, secret: &str) -> Result<(), String> {
        on_thread(|| entry()?.set_password(secret).map_err(|e| describe(&e)))
    }

    fn delete(&self) -> Result<bool, String> {
        on_thread(|| match entry()?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(describe(&e)),
        })
    }
}

impl SecretStore for DisabledStore {
    fn name(&self) -> &'static str {
        "system secret store (disabled by WOWADDONS_NO_KEYRING)"
    }

    fn get(&self) -> Result<Option<String>, String> {
        Ok(None)
    }

    fn set(&self, _secret: &str) -> Result<(), String> {
        Err(format!(
            "the system secret store is disabled ({ENV_NO_KEYRING} is set)"
        ))
    }

    fn delete(&self) -> Result<bool, String> {
        Ok(false)
    }
}

/// The key to use and where it comes from. A store error is returned as a warning
/// so that a broken secret store never hides a key set in the environment.
pub struct Resolved {
    pub key: Option<(ApiKey, KeySource)>,
    pub warning: Option<String>,
}

pub fn resolve_api_key(store: &dyn SecretStore) -> Resolved {
    if let Some(key) = ApiKey::from_env() {
        return Resolved {
            key: Some((key, KeySource::Environment)),
            warning: None,
        };
    }
    match store.get() {
        Ok(Some(v)) => Resolved {
            key: ApiKey::new(v).map(|k| (k, KeySource::Keyring)),
            warning: None,
        },
        Ok(None) => Resolved {
            key: None,
            warning: None,
        },
        Err(e) => Resolved {
            key: None,
            warning: Some(format!(
                "cannot read the API key from the {}: {e}",
                store.name()
            )),
        },
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::cell::RefCell;

    /// In-memory store for tests.
    #[derive(Default)]
    pub struct MemoryStore(pub RefCell<Option<String>>);

    impl SecretStore for MemoryStore {
        fn name(&self) -> &'static str {
            "memory"
        }
        fn get(&self) -> Result<Option<String>, String> {
            Ok(self.0.borrow().clone())
        }
        fn set(&self, secret: &str) -> Result<(), String> {
            *self.0.borrow_mut() = Some(secret.to_string());
            Ok(())
        }
        fn delete(&self) -> Result<bool, String> {
            Ok(self.0.borrow_mut().take().is_some())
        }
    }

    struct BrokenStore;
    impl SecretStore for BrokenStore {
        fn name(&self) -> &'static str {
            "broken"
        }
        fn get(&self) -> Result<Option<String>, String> {
            Err("locked".into())
        }
        fn set(&self, _: &str) -> Result<(), String> {
            Err("locked".into())
        }
        fn delete(&self) -> Result<bool, String> {
            Err("locked".into())
        }
    }

    // The environment variable is process-wide, so the env-dependent checks
    // live in one test.
    #[test]
    fn resolution_order() {
        // SAFETY: no other unit test reads or writes this variable.
        unsafe { std::env::remove_var(ENV_API_KEY) };
        let store = MemoryStore::default();
        assert!(resolve_api_key(&store).key.is_none());

        store.set("from-store").unwrap();
        let r = resolve_api_key(&store);
        assert_eq!(r.key.map(|(_, s)| s), Some(KeySource::Keyring));

        // A blank stored value counts as no key.
        store.set("  ").unwrap();
        assert!(resolve_api_key(&store).key.is_none());

        let r = resolve_api_key(&BrokenStore);
        assert!(r.key.is_none());
        assert!(r.warning.unwrap().contains("locked"));

        unsafe { std::env::set_var(ENV_API_KEY, "from-env") };
        let r = resolve_api_key(&BrokenStore);
        assert_eq!(r.key.map(|(_, s)| s), Some(KeySource::Environment));
        assert!(r.warning.is_none());
        unsafe { std::env::remove_var(ENV_API_KEY) };
    }

    #[test]
    fn disabled_store() {
        let d = DisabledStore;
        assert_eq!(d.get().unwrap(), None);
        assert!(d.set("x").is_err());
        assert!(!d.delete().unwrap());
    }
}
