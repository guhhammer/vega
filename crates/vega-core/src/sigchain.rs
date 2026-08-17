//! The device roster, as a hash-linked signed log.
//!
//! This is what replaces "ask the server which devices belong to Alice". Every
//! entry is signed by a key the chain already trusts and commits to the hash of
//! its predecessor, so the whole history can be replayed and verified offline by
//! anyone holding the account id. Tampering with an old entry invalidates every
//! entry after it.

use crate::codec::{Canonical, Writer};
use crate::error::{Error, Result};
use crate::identity::{AccountId, DeviceId, DeviceRecord, Identity, PrekeyBundle};
use crate::keys::{DhKey, Sig, VerifyKey};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const ENTRY_DOMAIN: &[u8] = b"vega:sigchain-entry:v1";

/// Validating a chain costs one signature check per entry, and a chain arrives
/// from whoever sent the invite. Cap it so a hostile invite cannot buy
/// unbounded CPU with a few kilobytes.
pub const MAX_ENTRIES: usize = 4096;

/// Every live device means another ciphertext for every message sent to this
/// account. A contact who claims a thousand devices would turn each of our
/// messages into a thousand — cap what we are willing to fan out to.
pub const MAX_DEVICES: usize = 32;

/// What an entry asserts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Body {
    /// Opens the chain. Must be first, must be self-signed by the root key.
    Genesis {
        root: VerifyKey,
        /// Account-level X25519 key contacts use to derive the pairwise secret.
        contact: DhKey,
        label: String,
    },
    AddDevice(DeviceRecord),
    RevokeDevice {
        device_id: DeviceId,
    },
    PublishPrekeys {
        device_id: DeviceId,
        bundle: PrekeyBundle,
    },
}

impl Body {
    fn tag(&self) -> u8 {
        match self {
            Body::Genesis { .. } => 1,
            Body::AddDevice(_) => 2,
            Body::RevokeDevice { .. } => 3,
            Body::PublishPrekeys { .. } => 4,
        }
    }
}

impl Canonical for Body {
    fn write_canonical(&self, w: &mut Writer) {
        w.u8(self.tag());
        match self {
            Body::Genesis {
                root,
                contact,
                label,
            } => {
                root.write_canonical(w);
                contact.write_canonical(w);
                w.str(label);
            }
            Body::AddDevice(rec) => rec.write_canonical(w),
            Body::RevokeDevice { device_id } => device_id.write_canonical(w),
            Body::PublishPrekeys { device_id, bundle } => {
                device_id.write_canonical(w);
                bundle.write_canonical(w);
            }
        }
    }
}

/// A single signed link in the chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub seq: u64,
    #[serde(with = "crate::identity::hex32")]
    pub prev: [u8; 32],
    pub ts: u64,
    pub body: Body,
    /// The key that signed this entry: the account root, or a live device.
    pub signer: VerifyKey,
    pub sig: Sig,
}

impl Entry {
    /// The bytes covered by the signature. Deliberately excludes `sig` itself.
    fn signing_bytes(
        seq: u64,
        prev: &[u8; 32],
        ts: u64,
        body: &Body,
        signer: &VerifyKey,
    ) -> Vec<u8> {
        let mut w = Writer::new();
        w.fixed(ENTRY_DOMAIN);
        w.u64(seq);
        w.fixed(prev);
        w.u64(ts);
        body.write_canonical(&mut w);
        signer.write_canonical(&mut w);
        w.finish()
    }

    /// The hash the *next* entry commits to. Covers the signature too, so the
    /// link pins the entry exactly as it was signed.
    pub fn id(&self) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        h.update(&Self::signing_bytes(
            self.seq,
            &self.prev,
            self.ts,
            &self.body,
            &self.signer,
        ));
        h.update(&self.sig.0);
        *h.finalize().as_bytes()
    }

    fn verify_self(&self) -> Result<()> {
        let msg = Self::signing_bytes(self.seq, &self.prev, self.ts, &self.body, &self.signer);
        self.signer.verify(&msg, &self.sig)
    }
}

/// The state a validated chain describes: who this account is, and which
/// devices currently speak for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainState {
    pub account_id: AccountId,
    pub root: VerifyKey,
    pub contact: DhKey,
    pub label: String,
    pub devices: BTreeMap<DeviceId, DeviceRecord>,
    pub revoked: BTreeSet<DeviceId>,
    pub prekeys: BTreeMap<DeviceId, PrekeyBundle>,
}

impl ChainState {
    pub fn device(&self, id: &DeviceId) -> Option<&DeviceRecord> {
        self.devices.get(id)
    }

    /// Devices a message should be encrypted for — everything live.
    pub fn live_devices(&self) -> impl Iterator<Item = &DeviceRecord> {
        self.devices.values()
    }
}

/// An append-only, self-verifying log of device authorisations.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sigchain {
    entries: Vec<Entry>,
}

impl Sigchain {
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Hash of the tip, which the next entry must commit to.
    pub fn head(&self) -> [u8; 32] {
        self.entries.last().map(Entry::id).unwrap_or([0u8; 32])
    }

    /// Open a new chain. The root key signs its own introduction.
    pub fn genesis(identity: &Identity, label: impl Into<String>, now: u64) -> Result<Self> {
        let body = Body::Genesis {
            root: identity.root_public()?,
            contact: identity.contact_public(),
            label: label.into(),
        };
        let mut chain = Self::default();
        chain.append_signed_by_root(identity, body, now)?;
        Ok(chain)
    }

    /// Append an entry signed by the account root key.
    pub fn append_signed_by_root(
        &mut self,
        identity: &Identity,
        body: Body,
        now: u64,
    ) -> Result<()> {
        let signer = identity.root_public()?;
        let (seq, prev) = (self.entries.len() as u64, self.head());
        let msg = Entry::signing_bytes(seq, &prev, now, &body, &signer);
        let sig = identity.sign_as_root(&msg)?;
        self.push(Entry {
            seq,
            prev,
            ts: now,
            body,
            signer,
            sig,
        })
    }

    /// Append an entry signed by this device's own key.
    pub fn append_signed_by_device(
        &mut self,
        identity: &Identity,
        body: Body,
        now: u64,
    ) -> Result<()> {
        let signer = identity.device_verify_key();
        let (seq, prev) = (self.entries.len() as u64, self.head());
        let msg = Entry::signing_bytes(seq, &prev, now, &body, &signer);
        let sig = identity.sign_as_device(&msg);
        self.push(Entry {
            seq,
            prev,
            ts: now,
            body,
            signer,
            sig,
        })
    }

    /// Accept an entry only if the chain still validates with it appended.
    /// Rejecting here rather than at read time means an invalid chain is never
    /// persisted in the first place.
    fn push(&mut self, entry: Entry) -> Result<()> {
        self.entries.push(entry);
        match self.validate() {
            Ok(_) => Ok(()),
            Err(e) => {
                self.entries.pop();
                Err(e)
            }
        }
    }

    /// Merge a chain received from the network. Because the log is append-only
    /// and hash-linked, a longer valid chain sharing our prefix is simply newer.
    /// A fork is never merged silently. Two valid chains for one account that
    /// disagree about history mean either a bug or a compromised root key, and
    /// both are things the caller must be told about rather than have papered
    /// over by taking whichever chain happened to be longer.
    pub fn merge(&mut self, other: &Sigchain) -> Result<bool> {
        if other.is_empty() {
            return Ok(false);
        }
        if self.is_empty() {
            other.validate()?;
            self.entries = other.entries.clone();
            return Ok(true);
        }

        // Same account? The genesis entry is the account, so this is the check.
        if self.entries[0] != other.entries[0] {
            return Err(Error::Sigchain(
                "incoming chain belongs to a different account".into(),
            ));
        }

        if other.entries.len() <= self.entries.len() {
            // Ours is at least as long: theirs must be a prefix of ours.
            return if self.entries.starts_with(&other.entries) {
                Ok(false)
            } else {
                Err(Error::Sigchain(
                    "incoming chain forks from ours — possible account compromise".into(),
                ))
            };
        }

        if !other.entries.starts_with(&self.entries) {
            return Err(Error::Sigchain(
                "incoming chain forks from ours — possible account compromise".into(),
            ));
        }
        other.validate()?;
        self.entries = other.entries.clone();
        Ok(true)
    }

    /// Replay the whole chain, verifying every link and signature.
    pub fn validate(&self) -> Result<ChainState> {
        if self.entries.len() > MAX_ENTRIES {
            return Err(Error::Sigchain(format!(
                "chain has {} entries, refusing above {MAX_ENTRIES}",
                self.entries.len()
            )));
        }

        let first = self
            .entries
            .first()
            .ok_or_else(|| Error::Sigchain("empty chain".into()))?;

        let (root, contact, label) = match &first.body {
            Body::Genesis {
                root,
                contact,
                label,
            } => (*root, *contact, label.clone()),
            _ => return Err(Error::Sigchain("first entry is not a genesis".into())),
        };

        if first.signer != root {
            return Err(Error::Sigchain(
                "genesis not signed by its own root key".into(),
            ));
        }

        let mut state = ChainState {
            account_id: AccountId::of(&root),
            root,
            contact,
            label,
            devices: BTreeMap::new(),
            revoked: BTreeSet::new(),
            prekeys: BTreeMap::new(),
        };

        let mut expected_prev = [0u8; 32];

        for (i, entry) in self.entries.iter().enumerate() {
            if entry.seq != i as u64 {
                return Err(Error::Sigchain(format!(
                    "entry {i} claims seq {}",
                    entry.seq
                )));
            }
            if entry.prev != expected_prev {
                return Err(Error::Sigchain(format!("entry {i} breaks the hash chain")));
            }

            // The signer must be trusted *as of this point in the replay* —
            // checked before the entry is applied, so an entry can never
            // authorise its own signer.
            let signer_is_root = entry.signer == root;
            let signer_is_live_device = state
                .devices
                .values()
                .any(|d| d.verify == entry.signer && !state.revoked.contains(&d.device_id));

            if i > 0 && !signer_is_root && !signer_is_live_device {
                return Err(Error::Sigchain(format!(
                    "entry {i} signed by a key that is neither the root nor a live device"
                )));
            }

            entry.verify_self()?;

            match &entry.body {
                Body::Genesis { .. } => {
                    if i != 0 {
                        return Err(Error::Sigchain(format!("genesis at position {i}")));
                    }
                }
                Body::AddDevice(rec) => {
                    if DeviceId::of(&rec.verify) != rec.device_id {
                        return Err(Error::Sigchain(format!(
                            "entry {i}: device id does not match its signing key"
                        )));
                    }
                    if state.devices.contains_key(&rec.device_id) {
                        return Err(Error::Sigchain(format!("entry {i}: device already added")));
                    }
                    if state.revoked.contains(&rec.device_id) {
                        return Err(Error::Sigchain(format!(
                            "entry {i}: revoked device cannot be re-added"
                        )));
                    }
                    if state.devices.len() >= MAX_DEVICES {
                        return Err(Error::Sigchain(format!(
                            "entry {i}: more than {MAX_DEVICES} live devices"
                        )));
                    }
                    state.devices.insert(rec.device_id, rec.clone());
                }
                Body::RevokeDevice { device_id } => {
                    if state.devices.remove(device_id).is_none() {
                        return Err(Error::Sigchain(format!(
                            "entry {i}: revoking a device that is not live"
                        )));
                    }
                    state.revoked.insert(*device_id);
                    state.prekeys.remove(device_id);
                }
                Body::PublishPrekeys { device_id, bundle } => {
                    let target = state.devices.get(device_id).ok_or_else(|| {
                        Error::Sigchain(format!("entry {i}: prekeys for an unknown device"))
                    })?;
                    // Only the device itself, or the root, may publish its keys.
                    if !signer_is_root && entry.signer != target.verify {
                        return Err(Error::Sigchain(format!(
                            "entry {i}: prekeys published by a different device"
                        )));
                    }
                    state.prekeys.insert(*device_id, bundle.clone());
                }
            }

            expected_prev = entry.id();
        }

        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use x25519_dalek::StaticSecret;

    fn now() -> u64 {
        1_755_000_000
    }

    /// An account with its first device registered — the normal starting point.
    fn account() -> (Identity, Sigchain) {
        let mut me = Identity::create("laptop");
        let mut chain = Sigchain::genesis(&me, "ada", now()).unwrap();
        chain
            .append_signed_by_root(&me, Body::AddDevice(me.device_record(now())), now())
            .unwrap();
        let bundle = me.replenish_prekeys(10);
        chain
            .append_signed_by_device(
                &me,
                Body::PublishPrekeys {
                    device_id: me.device_id,
                    bundle,
                },
                now(),
            )
            .unwrap();
        (me, chain)
    }

    #[test]
    fn a_fresh_account_validates() {
        let (me, chain) = account();
        let state = chain.validate().unwrap();
        assert_eq!(state.account_id, me.account_id);
        assert_eq!(state.devices.len(), 1);
        assert!(state.prekeys.contains_key(&me.device_id));
    }

    #[test]
    fn a_device_can_authorise_the_next_device() {
        let (me, mut chain) = account();
        let phone = Identity::adopt(
            StaticSecret::random_from_rng(rand::rngs::OsRng),
            me.account_id,
            "phone",
        );
        // Signed by the laptop, not the root — the root may not even be present.
        chain
            .append_signed_by_device(&me, Body::AddDevice(phone.device_record(now())), now())
            .unwrap();
        assert_eq!(chain.validate().unwrap().devices.len(), 2);
    }

    #[test]
    fn a_stranger_cannot_add_a_device() {
        let (me, mut chain) = account();
        let attacker = Identity::create("attacker");
        let forged = Body::AddDevice(attacker.device_record(now()));

        // The attacker signs with their own root key, which this chain never trusted.
        let seq = chain.len() as u64;
        let prev = chain.head();
        let signer = attacker.root_public().unwrap();
        let msg = Entry::signing_bytes(seq, &prev, now(), &forged, &signer);
        let sig = attacker.sign_as_root(&msg).unwrap();

        let err = chain.push(Entry {
            seq,
            prev,
            ts: now(),
            body: forged,
            signer,
            sig,
        });
        assert!(err.is_err());
        // The rejected entry left no trace.
        assert_eq!(chain.validate().unwrap().devices.len(), 1);
        assert_eq!(me.account_id, chain.validate().unwrap().account_id);
    }

    #[test]
    fn tampering_with_history_breaks_the_chain() {
        let (_me, mut chain) = account();
        if let Body::Genesis { label, .. } = &mut chain.entries[0].body {
            *label = "someone else".into();
        }
        assert!(chain.validate().is_err());
    }

    #[test]
    fn reordering_entries_breaks_the_chain() {
        let (_me, mut chain) = account();
        chain.entries.swap(1, 2);
        assert!(chain.validate().is_err());
    }

    #[test]
    fn a_revoked_device_cannot_sign() {
        let (me, mut chain) = account();
        let phone = Identity::adopt(
            StaticSecret::random_from_rng(rand::rngs::OsRng),
            me.account_id,
            "phone",
        );
        chain
            .append_signed_by_device(&me, Body::AddDevice(phone.device_record(now())), now())
            .unwrap();
        chain
            .append_signed_by_root(
                &me,
                Body::RevokeDevice {
                    device_id: phone.device_id,
                },
                now(),
            )
            .unwrap();

        // The revoked phone tries to add a device of its own.
        let rogue = Identity::adopt(
            StaticSecret::random_from_rng(rand::rngs::OsRng),
            me.account_id,
            "rogue",
        );
        let err = chain.append_signed_by_device(
            &phone,
            Body::AddDevice(rogue.device_record(now())),
            now(),
        );
        assert!(err.is_err());

        let state = chain.validate().unwrap();
        assert!(state.revoked.contains(&phone.device_id));
        assert_eq!(state.devices.len(), 1);
    }

    #[test]
    fn a_chain_cannot_claim_unbounded_devices() {
        let (me, mut chain) = account();
        // One device is already registered by `account()`.
        for n in 0..MAX_DEVICES {
            let d = Identity::adopt(
                StaticSecret::random_from_rng(rand::rngs::OsRng),
                me.account_id,
                format!("device-{n}"),
            );
            let result =
                chain.append_signed_by_device(&me, Body::AddDevice(d.device_record(now())), now());
            if result.is_err() {
                assert_eq!(chain.validate().unwrap().devices.len(), MAX_DEVICES);
                return;
            }
        }
        panic!("the device cap was never enforced");
    }

    #[test]
    fn merge_accepts_a_longer_chain_and_rejects_a_fork() {
        let (me, chain) = account();
        let mut theirs = chain.clone();
        let phone = Identity::adopt(
            StaticSecret::random_from_rng(rand::rngs::OsRng),
            me.account_id,
            "phone",
        );
        theirs
            .append_signed_by_device(&me, Body::AddDevice(phone.device_record(now())), now())
            .unwrap();

        let mut mine = chain.clone();
        assert!(mine.merge(&theirs).unwrap());
        assert_eq!(mine.validate().unwrap().devices.len(), 2);
        // Merging the same chain again is a no-op, not an error.
        assert!(!mine.merge(&theirs).unwrap());

        // A chain belonging to somebody else is refused, not quietly ignored.
        let (_other, unrelated) = account();
        assert!(mine.merge(&unrelated).is_err());

        // A genuine fork of our own account is refused in both directions.
        let (me2, base) = account();
        let mut left = base.clone();
        let mut right = base.clone();
        for (chain, label) in [(&mut left, "phone-a"), (&mut right, "phone-b")] {
            let d = Identity::adopt(
                StaticSecret::random_from_rng(rand::rngs::OsRng),
                me2.account_id,
                label,
            );
            chain
                .append_signed_by_device(&me2, Body::AddDevice(d.device_record(now())), now())
                .unwrap();
        }
        assert!(left.merge(&right).is_err());

        // A chain that is simply older than ours is a no-op, not an error.
        assert!(!left.merge(&base).unwrap());
    }

    #[test]
    fn one_device_cannot_publish_another_devices_prekeys() {
        let (me, mut chain) = account();
        let mut phone = Identity::adopt(
            StaticSecret::random_from_rng(rand::rngs::OsRng),
            me.account_id,
            "phone",
        );
        chain
            .append_signed_by_device(&me, Body::AddDevice(phone.device_record(now())), now())
            .unwrap();

        let bundle = phone.replenish_prekeys(3);
        // Laptop signs, but claims to be publishing for the phone.
        let err = chain.append_signed_by_device(
            &me,
            Body::PublishPrekeys {
                device_id: phone.device_id,
                bundle,
            },
            now(),
        );
        assert!(err.is_err());
    }
}
