//! Encryption for what stays on this device.
//!
//! Everything in [`crate::seal`] protects a message from the network. This
//! protects it from whoever ends up holding the disk: message history, contacts,
//! and received files are stored as ciphertext rather than as the plaintext they
//! were decrypted into.
//!
//! The key is the same device key the identity and the Olm sessions are pickled
//! with — from the platform keyring where there is one. Each kind of stored
//! value gets its own subkey through HKDF, so a ciphertext lifted out of one
//! table cannot be replayed into another, and the purpose is authenticated
//! alongside in case a future caller reuses a subkey by accident.
//!
//! XChaCha20-Poly1305 rather than the ChaCha20-Poly1305 used on the wire: its
//! 192-bit nonce can be drawn at random without a counter, and a store has no
//! natural counter to keep. Reuse would be catastrophic and a random 24-byte
//! nonce makes it not happen.

use crate::error::{Error, Result};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::Sha256;
use zeroize::Zeroize;

const INFO: &[u8] = b"vega:at-rest:v1";

/// XChaCha20's nonce, prepended to every ciphertext.
pub const NONCE_LEN: usize = 24;

/// Purposes. One subkey each, so a stored contact cannot be swapped in where a
/// stored message is expected.
pub mod purpose {
    pub const MESSAGE: &[u8] = b"message";
    pub const CONTACT: &[u8] = b"contact";
    pub const GROUP: &[u8] = b"group";
    pub const CHAIN: &[u8] = b"chain";
    pub const OUTBOX: &[u8] = b"outbox";
    pub const TRANSFER: &[u8] = b"transfer";
    /// A file written beside the database rather than inside it.
    pub const FILE: &[u8] = b"file";
}

fn cipher(key: &[u8; 32], purpose: &[u8]) -> XChaCha20Poly1305 {
    let hk = Hkdf::<Sha256>::new(Some(INFO), key);
    let mut sub = [0u8; 32];
    hk.expand(purpose, &mut sub)
        .expect("32 bytes is well within HKDF-SHA256's output limit");
    let cipher = XChaCha20Poly1305::new((&sub).into());
    // The subkey is copied into the cipher's state; this one goes away.
    sub.zeroize();
    cipher
}

/// Encrypt a value for storage. The nonce is prepended to the ciphertext.
pub fn seal(key: &[u8; 32], purpose: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);

    let ciphertext = cipher(key, purpose)
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: purpose,
            },
        )
        .map_err(|_| Error::Storage("could not encrypt a value for storage".into()))?;

    let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt a stored value.
///
/// Fails for anything that is not a value sealed with this key and purpose,
/// including one that has been altered — which is the point of using an AEAD for
/// data at rest rather than a bare cipher.
pub fn open(key: &[u8; 32], purpose: &[u8], sealed: &[u8]) -> Result<Vec<u8>> {
    if sealed.len() < NONCE_LEN {
        return Err(Error::Storage(
            "stored value is too short to be sealed".into(),
        ));
    }
    let (nonce, ciphertext) = sealed.split_at(NONCE_LEN);

    cipher(key, purpose)
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: purpose,
            },
        )
        .map_err(|_| Error::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7u8; 32];

    #[test]
    fn a_sealed_value_comes_back_unchanged() {
        let secret = b"the pier at four, bring the ledger \xf0\x9f\x93\x92";
        let sealed = seal(&KEY, purpose::MESSAGE, secret).unwrap();

        assert!(
            !sealed.windows(secret.len()).any(|w| w == secret),
            "the plaintext is sitting in the ciphertext"
        );
        assert_eq!(open(&KEY, purpose::MESSAGE, &sealed).unwrap(), secret);
    }

    #[test]
    fn the_same_value_seals_differently_every_time() {
        // A random nonce per value, so two identical messages do not produce
        // identical rows — otherwise the database leaks which messages repeat.
        let a = seal(&KEY, purpose::MESSAGE, b"yes").unwrap();
        let b = seal(&KEY, purpose::MESSAGE, b"yes").unwrap();
        assert_ne!(a, b);
        assert_eq!(open(&KEY, purpose::MESSAGE, &a).unwrap(), b"yes");
        assert_eq!(open(&KEY, purpose::MESSAGE, &b).unwrap(), b"yes");
    }

    #[test]
    fn another_key_does_not_open_it() {
        let sealed = seal(&KEY, purpose::MESSAGE, b"private").unwrap();
        assert!(open(&[8u8; 32], purpose::MESSAGE, &sealed).is_err());
    }

    #[test]
    fn a_value_cannot_be_moved_to_another_purpose() {
        // Someone with write access to the database should not be able to make a
        // stored contact be read as a stored message.
        let sealed = seal(&KEY, purpose::CONTACT, b"a contact").unwrap();
        assert!(open(&KEY, purpose::MESSAGE, &sealed).is_err());
        assert!(open(&KEY, purpose::CONTACT, &sealed).is_ok());
    }

    #[test]
    fn tampering_is_detected_rather_than_decrypted() {
        let sealed = seal(&KEY, purpose::FILE, b"an image").unwrap();

        for i in 0..sealed.len() {
            let mut broken = sealed.clone();
            broken[i] ^= 1;
            assert!(
                open(&KEY, purpose::FILE, &broken).is_err(),
                "a flipped bit at {i} was not caught"
            );
        }

        // Truncation, including shorter than the nonce.
        assert!(open(&KEY, purpose::FILE, &sealed[..sealed.len() - 1]).is_err());
        assert!(open(&KEY, purpose::FILE, &sealed[..8]).is_err());
        assert!(open(&KEY, purpose::FILE, b"").is_err());
    }
}
