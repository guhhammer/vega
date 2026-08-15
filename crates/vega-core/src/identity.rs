//! Accounts, devices, and the secret material that lives on this machine.
//!
//! An account is an Ed25519 keypair. The account id is the BLAKE3 hash of its
//! public key, which makes identity self-certifying: if you can name someone
//! you already hold everything needed to verify them, so there is no registry
//! to query, poison, or take offline.

use crate::codec::{Canonical, Writer};
use crate::error::{Error, Result};
use crate::keys::{DhKey, Sig, VerifyKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use std::fmt;
use vodozemac::olm::Account as OlmAccount;
use vodozemac::Ed25519SecretKey;
use x25519_dalek::{PublicKey as XPublic, StaticSecret as XSecret};

/// Domain separator for every hash in this module, so a device id can never be
/// mistaken for an account id even if the two hashed the same input.
const ACCOUNT_DOMAIN: &[u8] = b"vega:account-id:v1";
const DEVICE_DOMAIN: &[u8] = b"vega:device-id:v1";

macro_rules! hash_id {
    ($name:ident, $domain:expr, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(#[serde(with = "crate::identity::hex32")] pub [u8; 32]);

        impl $name {
            /// Derive the id from the public key it names.
            pub fn of(key: &VerifyKey) -> Self {
                let mut h = blake3::Hasher::new();
                h.update($domain);
                h.update(key.as_bytes());
                Self(*h.finalize().as_bytes())
            }

            pub fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }

            /// Lowercase base32 in groups of four — meant to be read aloud and
            /// compared by a human, so no mixed case and no `+`/`/`.
            pub fn to_display(&self) -> String {
                let s = data_encoding::BASE32_NOPAD.encode(&self.0).to_lowercase();
                s.as_bytes()
                    .chunks(4)
                    .map(|c| std::str::from_utf8(c).unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join("-")
            }

            /// Parse the display form back, tolerating case and grouping.
            pub fn from_display(s: &str) -> Result<Self> {
                let cleaned: String = s
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric())
                    .collect::<String>()
                    .to_uppercase();
                let raw = data_encoding::BASE32_NOPAD
                    .decode(cleaned.as_bytes())
                    .map_err(|e| Error::BadKey(e.to_string()))?;
                if raw.len() != 32 {
                    return Err(Error::BadKey(format!(
                        "expected 32 bytes, got {}",
                        raw.len()
                    )));
                }
                let mut out = [0u8; 32];
                out.copy_from_slice(&raw);
                Ok(Self(out))
            }

            /// Short form for logs and UI lists.
            pub fn short(&self) -> String {
                self.to_display().chars().take(9).collect()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.to_display())
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.short())
            }
        }

        impl Canonical for $name {
            fn write_canonical(&self, w: &mut Writer) {
                w.fixed(&self.0);
            }
        }
    };
}

hash_id!(
    AccountId,
    ACCOUNT_DOMAIN,
    "Stable identifier for a person: BLAKE3 of their account root public key."
);
hash_id!(
    DeviceId,
    DEVICE_DOMAIN,
    "Stable identifier for one device: BLAKE3 of that device's signing key."
);

/// Serde helper — ids are hex in JSON so they survive being copied around.
pub(crate) mod hex32 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&data_encoding::HEXLOWER.encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        let raw = data_encoding::HEXLOWER
            .decode(s.as_bytes())
            .map_err(serde::de::Error::custom)?;
        if raw.len() != 32 {
            return Err(serde::de::Error::custom("expected 32 bytes"));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&raw);
        Ok(out)
    }
}

/// The public description of one device, as published in the sigchain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRecord {
    pub device_id: DeviceId,
    /// Signs sigchain entries authored by this device.
    pub verify: VerifyKey,
    /// The Olm identity key — the long-term half of the message ratchet.
    pub olm_identity: DhKey,
    /// X25519 key for the sealed-sender outer layer. Separate from the Olm
    /// identity key because vodozemac does not expose that key's secret half.
    pub seal: DhKey,
    pub label: String,
    pub added_at: u64,
}

impl Canonical for DeviceRecord {
    fn write_canonical(&self, w: &mut Writer) {
        self.device_id.write_canonical(w);
        self.verify.write_canonical(w);
        self.olm_identity.write_canonical(w);
        self.seal.write_canonical(w);
        w.str(&self.label);
        w.u64(self.added_at);
    }
}

/// One-time keys a sender consumes to open a new Olm session with a device.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PrekeyBundle {
    pub one_time: Vec<DhKey>,
    /// Used when the one-time keys run out. Reusable, so it costs a little
    /// forward secrecy on the very first message — the ratchet recovers after.
    pub fallback: Option<DhKey>,
}

impl Canonical for PrekeyBundle {
    fn write_canonical(&self, w: &mut Writer) {
        w.u64(self.one_time.len() as u64);
        for k in &self.one_time {
            k.write_canonical(w);
        }
        match &self.fallback {
            Some(k) => {
                w.u8(1);
                k.write_canonical(w);
            }
            None => {
                w.u8(0);
            }
        }
    }
}

/// Everything secret that lives on this device.
///
/// The root key is `Option` on purpose. It signs the genesis entry and the
/// first device, after which any live device can authorise the next one — so
/// the root does not need to be present on every device, and a device that
/// never holds it cannot be used to forge an account takeover.
pub struct Identity {
    root: Option<Ed25519SecretKey>,
    /// Account-level X25519 key. Shared by all of an account's devices, because
    /// contacts derive the pairwise secret from it and must reach the same
    /// answer no matter which of your devices they are talking to.
    contact: XSecret,
    /// This device's Olm account: identity key, one-time keys, sessions.
    pub olm: OlmAccount,
    /// This device's sealed-sender key.
    seal: XSecret,
    pub device_id: DeviceId,
    pub account_id: AccountId,
    pub label: String,
}

impl Identity {
    /// Create a brand new account, and its first device.
    pub fn create(label: impl Into<String>) -> Self {
        let root = Ed25519SecretKey::new();
        let contact = XSecret::random_from_rng(OsRng);
        Self::assemble(Some(root), contact, OlmAccount::new(), label.into())
    }

    /// Build an identity for a device being added to an existing account. The
    /// caller supplies the account-level contact secret, which arrives over the
    /// device-linking channel.
    pub fn adopt(contact: XSecret, account_id: AccountId, label: impl Into<String>) -> Self {
        let olm = OlmAccount::new();
        let verify: VerifyKey = olm.ed25519_key().into();
        Self {
            root: None,
            contact,
            device_id: DeviceId::of(&verify),
            account_id,
            olm,
            seal: XSecret::random_from_rng(OsRng),
            label: label.into(),
        }
    }

    fn assemble(
        root: Option<Ed25519SecretKey>,
        contact: XSecret,
        olm: OlmAccount,
        label: String,
    ) -> Self {
        let account_id = root
            .as_ref()
            .map(|r| AccountId::of(&r.public_key().into()))
            .expect("assemble is only called with a root key present");
        let verify: VerifyKey = olm.ed25519_key().into();
        Self {
            root,
            contact,
            device_id: DeviceId::of(&verify),
            account_id,
            olm,
            seal: XSecret::random_from_rng(OsRng),
            label,
        }
    }

    pub fn root_public(&self) -> Result<VerifyKey> {
        self.root
            .as_ref()
            .map(|r| r.public_key().into())
            .ok_or(Error::NoRootKey)
    }

    pub fn has_root(&self) -> bool {
        self.root.is_some()
    }

    /// Sign with the account root key. Only genesis and the first device need
    /// this; everything after is signed by a device.
    pub fn sign_as_root(&self, message: &[u8]) -> Result<Sig> {
        let root = self.root.as_ref().ok_or(Error::NoRootKey)?;
        Ok(root.sign(message).into())
    }

    /// Sign with this device's key.
    pub fn sign_as_device(&self, message: &[u8]) -> Sig {
        self.olm.sign(message).into()
    }

    pub fn device_verify_key(&self) -> VerifyKey {
        self.olm.ed25519_key().into()
    }

    pub fn contact_public(&self) -> DhKey {
        XPublic::from(&self.contact).into()
    }

    pub(crate) fn contact_secret(&self) -> &XSecret {
        &self.contact
    }

    pub fn seal_public(&self) -> DhKey {
        XPublic::from(&self.seal).into()
    }

    pub(crate) fn seal_secret(&self) -> &XSecret {
        &self.seal
    }

    /// The public record for this device, ready to be put in the sigchain.
    pub fn device_record(&self, now: u64) -> DeviceRecord {
        DeviceRecord {
            device_id: self.device_id,
            verify: self.device_verify_key(),
            olm_identity: self.olm.curve25519_key().into(),
            seal: self.seal_public(),
            label: self.label.clone(),
            added_at: now,
        }
    }

    /// How many one-time keys remain unclaimed.
    ///
    /// Each new inbound session consumes one. When this reaches zero every new
    /// conversation falls back to the reusable fallback key, which costs forward
    /// secrecy on the first message — so this is the number that decides when to
    /// publish more.
    pub fn one_time_keys_left(&self) -> usize {
        self.olm.stored_one_time_key_count()
    }

    /// Top the device up to `count` unpublished one-time keys and return the
    /// bundle to publish.
    pub fn replenish_prekeys(&mut self, count: usize) -> PrekeyBundle {
        let capacity = self.olm.max_number_of_one_time_keys();
        self.olm.generate_one_time_keys(count.min(capacity));
        if self.olm.generate_fallback_key().is_none() {
            tracing::debug!("fallback key already current");
        }

        let one_time = self
            .olm
            .one_time_keys()
            .into_values()
            .map(DhKey::from)
            .collect();
        let fallback = self
            .olm
            .fallback_key()
            .into_values()
            .next()
            .map(DhKey::from);

        self.olm.mark_keys_as_published();
        PrekeyBundle { one_time, fallback }
    }
}

/// Derive the shared secret with a contact. See [`crate::tag`].
impl Identity {
    pub fn pairwise_with(
        &self,
        their_contact: &DhKey,
        their_account: &AccountId,
    ) -> crate::tag::Pairwise {
        crate::tag::Pairwise::derive(
            self.contact_secret(),
            &self.account_id,
            their_contact,
            their_account,
        )
    }
}

/// Identity at rest.
///
/// The Olm account is sealed with `pickle_key`; the raw X25519 and Ed25519
/// secrets are stored beside it. Custody of `pickle_key` is deliberately the
/// caller's problem — on a real install it comes from the platform keystore,
/// never from a file next to the database.
#[derive(Serialize, Deserialize)]
pub struct IdentityPickle {
    olm: String,
    root: Option<String>,
    contact: String,
    seal: String,
    label: String,
    account_id: AccountId,
}

impl Identity {
    pub fn pickle(&self, pickle_key: &[u8; 32]) -> IdentityPickle {
        IdentityPickle {
            olm: self.olm.pickle().encrypt(pickle_key),
            root: self.root.as_ref().map(|r| r.to_base64()),
            contact: vodozemac::base64_encode(self.contact.to_bytes()),
            seal: vodozemac::base64_encode(self.seal.to_bytes()),
            label: self.label.clone(),
            account_id: self.account_id,
        }
    }

    pub fn from_pickle(p: &IdentityPickle, pickle_key: &[u8; 32]) -> Result<Self> {
        let olm_pickle = vodozemac::olm::AccountPickle::from_encrypted(&p.olm, pickle_key)
            .map_err(|e| Error::BadKey(e.to_string()))?;
        let olm = OlmAccount::from_pickle(olm_pickle);

        let root = match &p.root {
            Some(s) => {
                Some(Ed25519SecretKey::from_base64(s).map_err(|e| Error::BadKey(e.to_string()))?)
            }
            None => None,
        };

        let contact = XSecret::from(decode_secret(&p.contact)?);
        let seal = XSecret::from(decode_secret(&p.seal)?);
        let verify: VerifyKey = olm.ed25519_key().into();

        Ok(Self {
            root,
            contact,
            device_id: DeviceId::of(&verify),
            account_id: p.account_id,
            olm,
            seal,
            label: p.label.clone(),
        })
    }
}

fn decode_secret(s: &str) -> Result<[u8; 32]> {
    let raw = vodozemac::base64_decode(s).map_err(|e| Error::BadKey(e.to_string()))?;
    if raw.len() != 32 {
        return Err(Error::BadKey("expected a 32 byte secret".into()));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&raw);
    Ok(out)
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("account_id", &self.account_id)
            .field("device_id", &self.device_id)
            .field("label", &self.label)
            .field("has_root", &self.root.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_id_round_trips_through_display() {
        let id = Identity::create("laptop").account_id;
        let parsed = AccountId::from_display(&id.to_display()).unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn display_form_tolerates_sloppy_input() {
        let id = Identity::create("laptop").account_id;
        let messy = id.to_display().to_uppercase().replace('-', " ");
        assert_eq!(AccountId::from_display(&messy).unwrap(), id);
    }

    #[test]
    fn account_and_device_ids_do_not_collide_on_the_same_key() {
        // Both hash a VerifyKey; only the domain separator keeps them apart.
        let key = VerifyKey([7u8; 32]);
        assert_ne!(AccountId::of(&key).0, DeviceId::of(&key).0);
    }

    #[test]
    fn adopted_device_has_no_root_key() {
        let first = Identity::create("laptop");
        let second = Identity::adopt(XSecret::random_from_rng(OsRng), first.account_id, "phone");
        assert!(!second.has_root());
        assert!(second.sign_as_root(b"x").is_err());
        assert_ne!(first.device_id, second.device_id);
    }

    #[test]
    fn replenish_produces_publishable_keys() {
        let mut id = Identity::create("laptop");
        let bundle = id.replenish_prekeys(5);
        assert_eq!(bundle.one_time.len(), 5);
        assert!(bundle.fallback.is_some());
        // Published keys are not handed out twice.
        assert!(id.replenish_prekeys(0).one_time.is_empty());
    }
}
