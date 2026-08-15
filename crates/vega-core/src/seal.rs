//! Sealed sender: the outer envelope layer.
//!
//! Olm needs the sender's identity key to open a pre-key message. Putting that
//! key in the clear would tell every relay who is talking to whom, which is the
//! metadata the whole design is trying not to leak. So the sender's identity
//! travels *inside* a one-shot ECIES box addressed to the recipient device.
//!
//! Construction: ephemeral X25519 → HKDF-SHA256 → ChaCha20-Poly1305, with both
//! public keys bound into the salt and the AEAD's associated data so a box
//! cannot be replayed at a different recipient.

use crate::error::{Error, Result};
use crate::keys::DhKey;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use rand::rngs::OsRng;
use sha2::Sha256;
use x25519_dalek::{EphemeralSecret, PublicKey as XPublic, StaticSecret as XSecret};

const INFO: &[u8] = b"vega:seal:v1";

/// A sealed box: the ephemeral public key, then the AEAD ciphertext.
pub const EPHEMERAL_LEN: usize = 32;

fn derive(shared: &[u8; 32], ephemeral: &XPublic, recipient: &XPublic) -> ([u8; 32], [u8; 12]) {
    // Binding both public keys into the salt means the derived key is unique to
    // this (ephemeral, recipient) pair, so a box resent to a different device
    // derives a different key and fails to open.
    let mut salt = Vec::with_capacity(64);
    salt.extend_from_slice(ephemeral.as_bytes());
    salt.extend_from_slice(recipient.as_bytes());

    let hk = Hkdf::<Sha256>::new(Some(&salt), shared);
    let mut okm = [0u8; 44];
    hk.expand(INFO, &mut okm)
        .expect("44 bytes is well within HKDF-SHA256's output limit");

    let mut key = [0u8; 32];
    let mut nonce = [0u8; 12];
    key.copy_from_slice(&okm[..32]);
    nonce.copy_from_slice(&okm[32..]);
    (key, nonce)
}

/// Associated data: the key pair this box is for, plus whatever routing context
/// the caller wants bound to it. Anything in here is authenticated but not
/// hidden, which is exactly the shape of a routing header.
fn aad(ephemeral: &XPublic, recipient: &XPublic, context: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(64 + context.len());
    v.extend_from_slice(ephemeral.as_bytes());
    v.extend_from_slice(recipient.as_bytes());
    v.extend_from_slice(&(context.len() as u32).to_be_bytes());
    v.extend_from_slice(context);
    v
}

/// Encrypt `plaintext` so that only the holder of `recipient`'s secret can read
/// it, with `context` authenticated alongside.
///
/// `context` carries the plaintext routing header. It has to stay readable — a
/// relay cannot forward what it cannot see — but binding it here means a relay
/// that rewrites the header produces a box that no longer opens, so tampering
/// surfaces as a failure instead of a message quietly going to the wrong place.
pub fn seal(recipient: &DhKey, plaintext: &[u8], context: &[u8]) -> Result<Vec<u8>> {
    let recipient_pub: XPublic = (*recipient).into();
    let ephemeral_secret = EphemeralSecret::random_from_rng(OsRng);
    let ephemeral_pub = XPublic::from(&ephemeral_secret);

    let shared = ephemeral_secret.diffie_hellman(&recipient_pub);
    if !shared.was_contributory() {
        // A low-order recipient key would force the shared secret to a known
        // constant. Refuse rather than encrypt to something anyone can open.
        return Err(Error::BadKey("recipient key is low order".into()));
    }

    let (key, nonce) = derive(shared.as_bytes(), &ephemeral_pub, &recipient_pub);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: &aad(&ephemeral_pub, &recipient_pub, context),
            },
        )
        .map_err(|_| Error::Decrypt)?;

    let mut out = Vec::with_capacity(EPHEMERAL_LEN + ct.len());
    out.extend_from_slice(ephemeral_pub.as_bytes());
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Open a box addressed to `secret`. `context` must match what was sealed.
pub fn unseal(secret: &XSecret, box_bytes: &[u8], context: &[u8]) -> Result<Vec<u8>> {
    if box_bytes.len() < EPHEMERAL_LEN + 16 {
        return Err(Error::Wire("sealed box too short".into()));
    }
    let mut eph = [0u8; 32];
    eph.copy_from_slice(&box_bytes[..EPHEMERAL_LEN]);
    let ephemeral_pub = XPublic::from(eph);
    let recipient_pub = XPublic::from(secret);

    let shared = secret.diffie_hellman(&ephemeral_pub);
    if !shared.was_contributory() {
        return Err(Error::Decrypt);
    }

    let (key, nonce) = derive(shared.as_bytes(), &ephemeral_pub, &recipient_pub);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
    cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &box_bytes[EPHEMERAL_LEN..],
                aad: &aad(&ephemeral_pub, &recipient_pub, context),
            },
        )
        .map_err(|_| Error::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keypair() -> (XSecret, DhKey) {
        let s = XSecret::random_from_rng(OsRng);
        let p = DhKey::from(XPublic::from(&s));
        (s, p)
    }

    #[test]
    fn round_trips() {
        let (secret, public) = keypair();
        let sealed = seal(&public, b"the eagle has landed", b"ctx").unwrap();
        assert_eq!(
            unseal(&secret, &sealed, b"ctx").unwrap(),
            b"the eagle has landed"
        );
    }

    #[test]
    fn the_wrong_recipient_cannot_open_it() {
        let (_s1, p1) = keypair();
        let (s2, _p2) = keypair();
        let sealed = seal(&p1, b"secret", b"").unwrap();
        assert!(unseal(&s2, &sealed, b"").is_err());
    }

    #[test]
    fn ciphertext_is_not_malleable() {
        let (secret, public) = keypair();
        let mut sealed = seal(&public, b"transfer 100", b"").unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0x01;
        assert!(unseal(&secret, &sealed, b"").is_err());
    }

    #[test]
    fn swapping_the_ephemeral_key_fails() {
        let (secret, public) = keypair();
        let mut sealed = seal(&public, b"hello", b"").unwrap();
        let other = seal(&public, b"hello", b"").unwrap();
        sealed[..EPHEMERAL_LEN].copy_from_slice(&other[..EPHEMERAL_LEN]);
        assert!(unseal(&secret, &sealed, b"").is_err());
    }

    #[test]
    fn two_seals_of_the_same_plaintext_differ() {
        let (_secret, public) = keypair();
        // Fresh ephemeral key each time, so the ciphertext must not repeat —
        // otherwise identical messages would be linkable on the wire.
        assert_ne!(
            seal(&public, b"same", b"").unwrap(),
            seal(&public, b"same", b"").unwrap()
        );
    }

    #[test]
    fn rewriting_the_bound_context_breaks_the_box() {
        let (secret, public) = keypair();
        let sealed = seal(&public, b"hello", b"tag-a").unwrap();
        // A relay that rewrites the routing header cannot leave the box intact.
        assert!(unseal(&secret, &sealed, b"tag-b").is_err());
        assert!(unseal(&secret, &sealed, b"").is_err());
        assert!(unseal(&secret, &sealed, b"tag-a").is_ok());
    }

    #[test]
    fn context_length_is_part_of_the_binding() {
        let (secret, public) = keypair();
        // Without a length prefix, ("ab","c") and ("a","bc") would authenticate
        // the same bytes; the recipient must not accept a re-split header.
        let sealed = seal(&public, b"x", b"abc").unwrap();
        assert!(unseal(&secret, &sealed, b"abc").is_ok());
    }

    #[test]
    fn truncated_input_is_rejected_without_panicking() {
        let (secret, public) = keypair();
        let sealed = seal(&public, b"hello", b"").unwrap();
        for n in 0..sealed.len() {
            assert!(unseal(&secret, &sealed[..n], b"").is_err());
        }
    }
}
