//! Where the database encryption key lives.
//!
//! Right now: a 0600 file next to the database. That protects against another
//! user on the same machine and against a stolen backup, and against nothing
//! else — anything running as this user can read it.
//!
//! This is the one place in Vega that is knowingly weaker than the design calls
//! for. The key belongs in the platform keystore (Android Keystore, Keychain,
//! DPAPI, Secret Service), which needs a Tauri plugin per platform. The
//! interface here is deliberately narrow so that swapping the implementation
//! touches nothing else.

use rand::RngCore;
use std::io;
use std::path::Path;

pub fn load_or_create(path: &Path) -> io::Result<[u8; 32]> {
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

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, key)?;
    restrict(path)?;
    Ok(key)
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

    #[test]
    fn the_key_is_stable_across_calls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        let a = load_or_create(&path).unwrap();
        let b = load_or_create(&path).unwrap();
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
        assert!(load_or_create(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn the_key_file_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        load_or_create(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "group and other must have no access");
    }
}
