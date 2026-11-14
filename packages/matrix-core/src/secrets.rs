//! Where the engine keeps secrets: access and refresh tokens, and the passphrase each account's
//! local database is encrypted with. The app chooses the backend (an OS keychain on desktop);
//! the engine only sees this trait.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// A failure of the secret backend, as text fit to show the user.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct SecretError(pub String);

/// A key-value store for secrets. Calls may block (a keychain prompt, a D-Bus round trip); the
/// engine runs them off its async threads.
pub trait SecretStore: Send + Sync + 'static {
    /// The secret under `key`, or `None` when there is none.
    fn get(&self, key: &str) -> Result<Option<String>, SecretError>;
    /// Stores `value` under `key`, replacing any previous value.
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError>;
    /// Removes the secret under `key`; removing a missing key succeeds.
    fn delete(&self, key: &str) -> Result<(), SecretError>;
}

/// Secrets in memory only: for tests and headless runs. Nothing survives the process.
#[derive(Debug, Default)]
pub struct MemorySecrets(Mutex<HashMap<String, String>>);

impl SecretStore for MemorySecrets {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        Ok(self.lock()?.get(key).cloned())
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        self.lock()?.insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        self.lock()?.remove(key);
        Ok(())
    }
}

impl MemorySecrets {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<String, String>>, SecretError> {
        self.0.lock().map_err(|_| SecretError("secret store lock poisoned".into()))
    }
}

/// Secrets as files in one directory, readable only by the user on Unix. The fallback when no OS
/// keychain is available; weaker than a keychain, since anything running as the user can read
/// them.
#[derive(Debug)]
pub struct FileSecrets {
    dir: PathBuf,
}

impl FileSecrets {
    /// Secrets kept under `dir`, created on first write.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The file for `key`: letters and digits as they are, every other byte as `_` and two hex
    /// digits, so no two keys share a file (`@a-b` and `@a_b` used to).
    fn path(&self, key: &str) -> PathBuf {
        let mut name = String::with_capacity(key.len() * 2);
        for byte in key.bytes() {
            if byte.is_ascii_alphanumeric() {
                name.push(char::from(byte));
            } else {
                name.push_str(&format!("_{byte:02x}"));
            }
        }
        self.dir.join(name)
    }

    /// Where earlier versions kept `key`: read when the new file isn't there yet.
    fn legacy_path(&self, key: &str) -> PathBuf {
        let name: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { '_' }).collect();
        self.dir.join(name)
    }

    fn read(path: &std::path::Path) -> Result<Option<String>, SecretError> {
        match std::fs::read_to_string(path) {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(SecretError(error.to_string())),
        }
    }

    fn remove(path: &std::path::Path) -> Result<(), SecretError> {
        match std::fs::remove_file(path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(SecretError(error.to_string())),
            _ => Ok(()),
        }
    }
}

impl SecretStore for FileSecrets {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        match Self::read(&self.path(key))? {
            Some(value) => Ok(Some(value)),
            None => Self::read(&self.legacy_path(key)),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        let io = |error: std::io::Error| SecretError(error.to_string());
        std::fs::create_dir_all(&self.dir).map_err(io)?;
        let path = self.path(key);
        // Written whole to a private file first, then moved over the old one: a crash leaves
        // the old secret, never half of the new one, and no moment readable by others.
        let temp =
            path.with_file_name(format!("{}.tmp", path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        {
            use std::io::Write;
            let mut file = options.open(&temp).map_err(io)?;
            file.write_all(value.as_bytes()).map_err(io)?;
            file.sync_all().map_err(io)?;
        }
        std::fs::rename(&temp, &path).map_err(io)
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        Self::remove(&self.path(key))?;
        Self::remove(&self.legacy_path(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_secrets_should_return_what_was_set() {
        let store = MemorySecrets::default();
        store.set("k", "v").unwrap();
        assert_eq!(store.get("k").unwrap().as_deref(), Some("v"));
    }

    #[test]
    fn file_secrets_should_survive_a_new_instance() {
        let dir = std::env::temp_dir().join(format!("matrix-core-secrets-{}", std::process::id()));
        FileSecrets::new(&dir).set("@a:example.org", "token").unwrap();
        let value = FileSecrets::new(&dir).get("@a:example.org").unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(value.as_deref(), Some("token"));
    }

    #[test]
    fn file_secrets_of_similar_keys_should_not_share_a_file() {
        let dir = std::env::temp_dir().join(format!("matrix-core-secrets-similar-{}", std::process::id()));
        let store = FileSecrets::new(&dir);
        store.set("session:@a-b:example.org", "one").unwrap();
        store.set("session:@a_b:example.org", "two").unwrap();
        let first = store.get("session:@a-b:example.org").unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(first.as_deref(), Some("one"));
    }

    #[test]
    fn a_secret_saved_by_an_earlier_version_should_still_be_found() {
        let dir = std::env::temp_dir().join(format!("matrix-core-secrets-legacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("session__a_example.org"), "token").unwrap();
        let value = FileSecrets::new(&dir).get("session:@a:example.org").unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(value.as_deref(), Some("token"));
    }

    #[test]
    fn deleting_a_missing_secret_should_succeed() {
        assert!(MemorySecrets::default().delete("missing").is_ok());
    }
}
