//! Pairwise secrets, rotating routing tags, and DHT rendezvous keys.
//!
//! Two accounts that know each other's contact key can derive a shared secret
//! with no round trip. Everything a third party might use to follow you around —
//! the tag you announce on a LAN, the key your address is published under in the
//! DHT — is derived from that secret and an epoch counter.
//!
//! This is the property that keeps the DHT from becoming a scrapable map of the
//! social graph: the lookup key is unguessable unless you are already a contact.

use crate::identity::AccountId;
use crate::keys::DhKey;
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{PublicKey as XPublic, StaticSecret as XSecret};

/// How long one tag is valid. An hour bounds how long a leaked tag stays useful
/// and stops one identity being correlated across a whole day.
pub const EPOCH_SECS: u64 = 3600;

/// Routing tags are truncated to 16 bytes — enough that collisions are
/// negligible, short enough to keep envelope headers small.
pub const TAG_LEN: usize = 16;

pub type Tag = [u8; TAG_LEN];
pub type RendezvousKey = [u8; 32];
/// Proof that the holder knows the pairwise secret behind a tag.
pub type CollectToken = [u8; 32];

/// The epoch a given unix timestamp falls in.
pub fn epoch_at(unix_secs: u64) -> u64 {
    unix_secs / EPOCH_SECS
}

/// A shared secret between two accounts, derived without any round trip.
#[derive(Clone)]
pub struct Pairwise([u8; 32]);

impl Pairwise {
    /// Derive from my account contact secret and their published contact key.
    ///
    /// The account ids are sorted before hashing so both sides derive the same
    /// value regardless of who is computing it.
    pub fn derive(
        my_secret: &XSecret,
        my_account: &AccountId,
        their_contact: &DhKey,
        their_account: &AccountId,
    ) -> Self {
        let their_pub: XPublic = (*their_contact).into();
        let shared = my_secret.diffie_hellman(&their_pub);

        let (lo, hi) = if my_account <= their_account {
            (my_account, their_account)
        } else {
            (their_account, my_account)
        };

        let mut salt = Vec::with_capacity(64);
        salt.extend_from_slice(lo.as_bytes());
        salt.extend_from_slice(hi.as_bytes());

        let hk = Hkdf::<Sha256>::new(Some(&salt), shared.as_bytes());
        let mut out = [0u8; 32];
        hk.expand(b"vega:pairwise:v1", &mut out)
            .expect("32 bytes is within HKDF-SHA256's output limit");
        Pairwise(out)
    }

    fn expand<const N: usize>(&self, info: &[u8], epoch: u64) -> [u8; N] {
        let hk = Hkdf::<Sha256>::new(Some(&epoch.to_be_bytes()), &self.0);
        let mut out = [0u8; N];
        hk.expand(info, &mut out)
            .expect("callers request well under HKDF-SHA256's output limit");
        out
    }

    /// The tag a peer announces so contacts — and only contacts — recognise it.
    ///
    /// Directional: the tag I use to address you is not the tag you use to
    /// address me, so an observer who sees both cannot pair them up.
    pub fn tag_for(&self, recipient: &AccountId, epoch: u64) -> Tag {
        let mut info = Vec::with_capacity(48);
        info.extend_from_slice(b"vega:tag:v1");
        info.extend_from_slice(recipient.as_bytes());
        self.expand::<TAG_LEN>(&info, epoch)
    }

    /// The DHT key a peer publishes its current addresses under.
    pub fn rendezvous_key(&self, owner: &AccountId, epoch: u64) -> RendezvousKey {
        let mut info = Vec::with_capacity(48);
        info.extend_from_slice(b"vega:rendezvous:v1");
        info.extend_from_slice(owner.as_bytes());
        self.expand::<32>(&info, epoch)
    }

    /// Released to a mailbox peer to claim mail parked under `tag`.
    ///
    /// The tag itself travels on the wire in every envelope, so anyone who
    /// relayed a message knows it. Without a second secret they could walk up to
    /// a mailbox and collect — and therefore delete — someone else's mail. This
    /// token is derived from the pairwise secret, which a relay never sees.
    pub fn collect_token(&self, tag: &Tag, epoch: u64) -> CollectToken {
        let mut info = Vec::with_capacity(48);
        info.extend_from_slice(b"vega:collect:v1");
        info.extend_from_slice(tag);
        self.expand::<32>(&info, epoch)
    }

    /// Symmetric key protecting the contents of a rendezvous record.
    pub fn record_key(&self, owner: &AccountId, epoch: u64) -> [u8; 32] {
        let mut info = Vec::with_capacity(48);
        info.extend_from_slice(b"vega:record:v1");
        info.extend_from_slice(owner.as_bytes());
        self.expand::<32>(&info, epoch)
    }
}

impl std::fmt::Debug for Pairwise {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pairwise(<redacted>)")
    }
}

impl Drop for Pairwise {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.0.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    struct Party {
        id: AccountId,
        secret: XSecret,
        contact: DhKey,
    }

    fn party() -> Party {
        let identity = Identity::create("x");
        let secret = XSecret::random_from_rng(rand::rngs::OsRng);
        let contact = DhKey::from(XPublic::from(&secret));
        Party {
            id: identity.account_id,
            secret,
            contact,
        }
    }

    fn both_sides(a: &Party, b: &Party) -> (Pairwise, Pairwise) {
        (
            Pairwise::derive(&a.secret, &a.id, &b.contact, &b.id),
            Pairwise::derive(&b.secret, &b.id, &a.contact, &a.id),
        )
    }

    #[test]
    fn both_sides_derive_the_same_secret() {
        let (a, b) = (party(), party());
        let (pa, pb) = both_sides(&a, &b);
        assert_eq!(pa.tag_for(&b.id, 100), pb.tag_for(&b.id, 100));
        assert_eq!(pa.rendezvous_key(&a.id, 7), pb.rendezvous_key(&a.id, 7));
    }

    #[test]
    fn tags_rotate_with_the_epoch() {
        let (a, b) = (party(), party());
        let (pa, _pb) = both_sides(&a, &b);
        assert_ne!(pa.tag_for(&b.id, 100), pa.tag_for(&b.id, 101));
    }

    #[test]
    fn tags_are_directional() {
        let (a, b) = (party(), party());
        let (pa, _pb) = both_sides(&a, &b);
        assert_ne!(pa.tag_for(&a.id, 100), pa.tag_for(&b.id, 100));
    }

    #[test]
    fn a_stranger_derives_nothing_useful() {
        let (a, b, c) = (party(), party(), party());
        let (pa, _) = both_sides(&a, &b);
        let (pc, _) = both_sides(&a, &c);
        // c shares a secret with a, but that tells it nothing about a↔b.
        assert_ne!(pa.tag_for(&b.id, 100), pc.tag_for(&b.id, 100));
    }

    #[test]
    fn the_four_derivations_never_collide() {
        let (a, b) = (party(), party());
        let (pa, _) = both_sides(&a, &b);
        let tag = pa.tag_for(&b.id, 5);
        let rv = pa.rendezvous_key(&b.id, 5);
        let rk = pa.record_key(&b.id, 5);
        assert_ne!(&rv[..], &rk[..]);
        assert_ne!(&tag[..], &rv[..TAG_LEN]);
    }

    #[test]
    fn a_collect_token_cannot_be_derived_from_the_tag_alone() {
        let (a, b, c) = (party(), party(), party());
        let (pa, pb) = both_sides(&a, &b);
        let tag = pa.tag_for(&b.id, 9);

        // Both ends of the conversation agree.
        assert_eq!(pa.collect_token(&tag, 9), pb.collect_token(&tag, 9));

        // A relay that carried the message knows the tag but not the secret.
        let (pc, _) = both_sides(&a, &c);
        assert_ne!(pa.collect_token(&tag, 9), pc.collect_token(&tag, 9));

        // And it is bound to both the tag and the epoch.
        assert_ne!(pa.collect_token(&tag, 9), pa.collect_token(&tag, 10));
        assert_ne!(pa.collect_token(&tag, 9), pa.collect_token(&[0u8; 16], 9));
    }

    #[test]
    fn epoch_boundaries_are_where_they_should_be() {
        assert_eq!(epoch_at(0), 0);
        assert_eq!(epoch_at(EPOCH_SECS - 1), 0);
        assert_eq!(epoch_at(EPOCH_SECS), 1);
    }
}
