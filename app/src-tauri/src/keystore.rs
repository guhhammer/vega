//! Where the database encryption key lives.
//!
//! Preferred: the platform's own credential store — Secret Service on Linux,
//! Keychain on macOS, Credential Manager on Windows. There the key is held by
//! the OS, unlocked with the user's session, and never sits in a file another
//! process can read by walking the filesystem.
//!
//! Fallback: a 0600 file next to the database. Needed because a keyring is not
//! always there — a headless server, a container, a session with no D-Bus. That
//! is genuinely weaker, so it says so in the log rather than failing quietly.

use rand::RngCore;
use std::io;
use std::path::Path;

/// Names the entry appears under in the platform's credential manager, where a
/// user may well see it. "Vega" and "device-key" say what it is.
const SERVICE: &str = "Vega";
const ENTRY: &str = "device-key";

/// Where the key ended up, so the caller can tell the user the truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backing {
    /// The OS credential store.
    Keyring,
    /// A 0600 file. Protects against another user and a stolen backup; not
    /// against anything running as this user.
    File,
}

pub struct Key {
    pub bytes: [u8; 32],
    pub backing: Backing,
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Key")
            .field("backing", &self.backing)
            .finish_non_exhaustive()
    }
}

/// Fetch the device key, creating it on first run.
///
/// Order of preference: the keyring; then a file the keyring can adopt; then a
/// new key. A file that is successfully migrated is removed, but only after the
/// keyring has been read back and confirmed to hold the same bytes — losing the
/// key would render every stored message undecryptable.
pub fn load_or_create(path: &Path) -> io::Result<Key> {
    match keyring_get() {
        Ok(Some(bytes)) => {
            return Ok(Key {
                bytes,
                backing: Backing::Keyring,
            })
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(
                error = %e,
                "no platform keyring available; falling back to a file, which is weaker"
            );
            return Ok(Key {
                bytes: file_get_or_create(path)?,
                backing: Backing::File,
            });
        }
    }

    // The keyring works but holds nothing. Adopt an existing file if there is
    // one, so an upgrade does not orphan a database.
    if path.exists() {
        let bytes = file_get_or_create(path)?;
        if migrate(path, &bytes) {
            return Ok(Key {
                bytes,
                backing: Backing::Keyring,
            });
        }
        return Ok(Key {
            bytes,
            backing: Backing::File,
        });
    }

    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);

    match keyring_set(&bytes) {
        Ok(()) => Ok(Key {
            bytes,
            backing: Backing::Keyring,
        }),
        Err(e) => {
            tracing::warn!(error = %e, "could not write to the platform keyring; using a file");
            write_file(path, &bytes)?;
            Ok(Key {
                bytes,
                backing: Backing::File,
            })
        }
    }
}

/// Move a file-held key into the keyring. Returns whether it worked.
fn migrate(path: &Path, bytes: &[u8; 32]) -> bool {
    if keyring_set(bytes).is_err() {
        return false;
    }
    // Read back before deleting. A store that accepted the write but cannot
    // return it would otherwise cost the user every message they have.
    match keyring_get() {
        Ok(Some(stored)) if stored == *bytes => {
            if let Err(e) = std::fs::remove_file(path) {
                tracing::warn!(error = %e, "key moved to the keyring but the old file remains");
            } else {
                tracing::info!("moved the device key from a file into the platform keyring");
            }
            true
        }
        _ => {
            tracing::warn!("keyring accepted the key but did not return it; keeping the file");
            false
        }
    }
}

// ---- platform keyring ----

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn keyring_get() -> Result<Option<[u8; 32]>, String> {
    let entry = keyring::Entry::new(SERVICE, ENTRY).map_err(|e| e.to_string())?;
    match entry.get_password() {
        Ok(encoded) => {
            let raw = data_encoding::BASE64
                .decode(encoded.as_bytes())
                .map_err(|e| e.to_string())?;
            let bytes: [u8; 32] = raw
                .try_into()
                .map_err(|_| "keyring holds a key of the wrong length".to_string())?;
            Ok(Some(bytes))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn keyring_set(bytes: &[u8; 32]) -> Result<(), String> {
    let entry = keyring::Entry::new(SERVICE, ENTRY).map_err(|e| e.to_string())?;
    entry
        .set_password(&data_encoding::BASE64.encode(bytes))
        .map_err(|e| e.to_string())
}

// Android and anything else. Android has a keystore, but reaching it needs JNI
// and a Tauri plugin — outstanding native work. Until then the file path is
// used, and app-private storage is at least not world-readable.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn keyring_get() -> Result<Option<[u8; 32]>, String> {
    Err("no keyring backend compiled in for this target".into())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn keyring_set(_bytes: &[u8; 32]) -> Result<(), String> {
    Err("no keyring backend compiled in for this target".into())
}

// ---- file fallback ----

fn file_get_or_create(path: &Path) -> io::Result<[u8; 32]> {
    if path.exists() {
        let raw = std::fs::read(path)?;
        if raw.len() != 32 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "key file is not 32 bytes — refusing to guess",
            ));
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&raw);
        return Ok(key);
    }

    let mut key = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut key);
    write_file(path, &key)?;
    Ok(key)
}

fn write_file(path: &Path, key: &[u8; 32]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, key)?;
    restrict(path)
}

#[cfg(unix)]
fn restrict(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict(_path: &Path) -> io::Result<()> {
    // Windows inherits the user profile's ACL, which is already user-only.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // These exercise the file path directly. The keyring path is not tested
    // here on purpose: a test that writes to the developer's real credential
    // store is a test that leaves litter behind and fails on any machine
    // without a session keyring.

    #[test]
    fn the_file_key_is_stable_across_calls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        let a = file_get_or_create(&path).unwrap();
        let b = file_get_or_create(&path).unwrap();
        assert_eq!(a, b);
        assert_ne!(a, [0u8; 32]);
    }

    #[test]
    fn a_truncated_key_file_is_an_error_not_a_silent_reset() {
        // Silently regenerating would make every stored message undecryptable
        // while looking like a successful start.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        std::fs::write(&path, [1u8; 16]).unwrap();
        assert!(file_get_or_create(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn the_key_file_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        file_get_or_create(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "group and other must have no access");
    }

    #[test]
    fn load_or_create_always_yields_a_usable_key() {
        // Whichever backing it lands on, the caller gets 32 bytes and is told
        // which one, so it can report the weaker case honestly.
        let dir = tempfile::tempdir().unwrap();
        let key = load_or_create(&dir.path().join("key")).unwrap();
        assert_ne!(key.bytes, [0u8; 32]);
        assert!(matches!(key.backing, Backing::Keyring | Backing::File));
    }
}
