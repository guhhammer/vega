//! Public-key newtypes.
//!
//! vodozemac has perfectly good key types, but they are not what we want in
//! wire structs: we need one representation that is `Copy`, ordered, canonical
//! as raw bytes, and readable as base64 in JSON. These wrap raw bytes and
//! convert to vodozemac types only at the point of use.

use crate::codec::{Canonical, Writer};
use crate::error::{Error, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use vodozemac::{Curve25519PublicKey, Ed25519PublicKey, Ed25519Signature};

fn b64(bytes: &[u8]) -> String {
    vodozemac::base64_encode(bytes)
}

fn unb64(s: &str, want: usize) -> Result<Vec<u8>> {
    let v = vodozemac::base64_decode(s).map_err(|e| Error::BadKey(e.to_string()))?;
    if v.len() != want {
        return Err(Error::BadKey(format!(
            "expected {want} bytes, got {}",
            v.len()
        )));
    }
    Ok(v)
}

macro_rules! byte_key {
    ($name:ident, $len:expr, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub [u8; $len]);

        impl $name {
            pub const LEN: usize = $len;

            pub fn as_bytes(&self) -> &[u8; $len] {
                &self.0
            }

            pub fn to_base64(&self) -> String {
                b64(&self.0)
            }

            pub fn from_base64(s: &str) -> Result<Self> {
                let v = unb64(s, $len)?;
                let mut out = [0u8; $len];
                out.copy_from_slice(&v);
                Ok(Self(out))
            }

            pub fn from_slice(s: &[u8]) -> Result<Self> {
                if s.len() != $len {
                    return Err(Error::BadKey(format!(
                        "expected {} bytes, got {}",
                        $len,
                        s.len()
                    )));
                }
                let mut out = [0u8; $len];
                out.copy_from_slice(s);
                Ok(Self(out))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                // Truncated: full keys in logs are noise, and for secrets-adjacent
                // values a short prefix is all anyone should be reading anyway.
                write!(f, "{}({}…)", stringify!($name), &self.to_base64()[..8])
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
                s.serialize_str(&self.to_base64())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                Self::from_base64(&s).map_err(serde::de::Error::custom)
            }
        }

        impl Canonical for $name {
            fn write_canonical(&self, w: &mut Writer) {
                w.fixed(&self.0);
            }
        }
    };
}

byte_key!(
    VerifyKey,
    32,
    "An Ed25519 public key — used to verify sigchain entries."
);
byte_key!(
    DhKey,
    32,
    "A Curve25519 public key — Olm identity keys, one-time keys, and X25519 agreement keys."
);
byte_key!(Sig, 64, "An Ed25519 signature.");

impl VerifyKey {
    pub fn verify(&self, message: &[u8], sig: &Sig) -> Result<()> {
        let key =
            Ed25519PublicKey::from_slice(&self.0).map_err(|e| Error::BadKey(e.to_string()))?;
        let sig = Ed25519Signature::from_slice(&sig.0)
            .map_err(|_| Error::BadSignature("malformed signature"))?;
        key.verify(message, &sig)
            .map_err(|_| Error::BadSignature("does not verify under this key"))
    }
}

impl From<Ed25519PublicKey> for VerifyKey {
    fn from(k: Ed25519PublicKey) -> Self {
        Self(*k.as_bytes())
    }
}

impl From<Ed25519Signature> for Sig {
    fn from(s: Ed25519Signature) -> Self {
        Self(s.to_bytes())
    }
}

impl From<Curve25519PublicKey> for DhKey {
    fn from(k: Curve25519PublicKey) -> Self {
        Self(k.to_bytes())
    }
}

impl From<DhKey> for Curve25519PublicKey {
    fn from(k: DhKey) -> Self {
        Curve25519PublicKey::from_bytes(k.0)
    }
}

impl From<x25519_dalek::PublicKey> for DhKey {
    fn from(k: x25519_dalek::PublicKey) -> Self {
        Self(k.to_bytes())
    }
}

impl From<DhKey> for x25519_dalek::PublicKey {
    fn from(k: DhKey) -> Self {
        x25519_dalek::PublicKey::from(k.0)
    }
}
