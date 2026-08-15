//! Olm sessions: the end-to-end layer.
//!
//! One session per (my device, their device) pair. Sending to a contact who has
//! three devices means three ciphertexts, and a copy to each of my own devices
//! on top — that fan-out is what a server would otherwise do for us.

use crate::envelope::{Body, Content, Envelope, Inner};
use crate::error::{Error, Result};
use crate::identity::{AccountId, DeviceId, DeviceRecord, Identity, PrekeyBundle};
use crate::keys::DhKey;
use crate::seal;
use crate::sigchain::ChainState;
use crate::tag::{epoch_at, Pairwise};
use std::collections::HashMap;
use vodozemac::olm::{OlmMessage, Session, SessionConfig};
use vodozemac::Curve25519PublicKey;

/// Olm v1: AES-256 + HMAC with an 8-byte truncated MAC.
///
/// v2 (untruncated MAC) exists but only behind vodozemac's
/// `experimental-session-config` feature, and enabling an experimental flag for
/// a security-critical component is the wrong trade. The truncation is also not
/// load-bearing here: every Olm ciphertext is wrapped in the sealed-sender
/// layer, whose Poly1305 tag authenticates the whole thing at full length.
fn olm_session_config() -> SessionConfig {
    SessionConfig::version_1()
}

/// Live Olm sessions, keyed by the peer device they talk to.
///
/// A device can legitimately have more than one session with the same peer —
/// both sides may open one simultaneously — so decryption tries each in turn.
#[derive(Default)]
pub struct Sessions {
    by_device: HashMap<DeviceId, Vec<Peered>>,
}

/// A session together with the peer identity key it is bound to.
///
/// vodozemac's `SessionKeys::identity_key` is the *initiator's* key, which is
/// our own on any session we opened — useless for deciding who is talking to
/// us. So the remote key is recorded when the session is created, from a source
/// that is already authenticated: the peer's signed chain for sessions we
/// initiate, and the 3DH for sessions they initiate.
pub struct Peered {
    pub session: Session,
    pub remote: DhKey,
}

impl Sessions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn device_count(&self) -> usize {
        self.by_device.len()
    }

    pub fn has_session_with(&self, device: &DeviceId) -> bool {
        self.by_device.get(device).is_some_and(|v| !v.is_empty())
    }

    pub fn insert(&mut self, device: DeviceId, session: Session, remote: DhKey) {
        self.by_device
            .entry(device)
            .or_default()
            .push(Peered { session, remote });
    }

    /// All sessions, for persistence.
    pub fn iter(&self) -> impl Iterator<Item = (&DeviceId, &Vec<Peered>)> {
        self.by_device.iter()
    }

    /// Open a session with a device we have never spoken to, consuming one of
    /// its published prekeys.
    pub fn establish(
        &mut self,
        identity: &Identity,
        peer: &DeviceRecord,
        bundle: &PrekeyBundle,
    ) -> Result<()> {
        let one_time = bundle
            .one_time
            .first()
            .copied()
            .or(bundle.fallback)
            .ok_or_else(|| Error::NoPrekeys(peer.device_id.short()))?;

        let session = identity
            .olm
            .create_outbound_session(
                olm_session_config(),
                Curve25519PublicKey::from(peer.olm_identity),
                Curve25519PublicKey::from(one_time),
            )
            .map_err(|e| Error::BadKey(e.to_string()))?;

        // The peer's chain vouches for this key, so it is safe to record as the
        // remote identity for everything that follows on this session.
        self.insert(peer.device_id, session, peer.olm_identity);
        Ok(())
    }

    /// Encrypt one message for one peer device, wrapped in a sealed envelope.
    ///
    /// `pairwise` is the sender↔recipient-account secret; the tag it produces is
    /// what the network routes on.
    pub fn encrypt_for(
        &mut self,
        identity: &Identity,
        peer: &DeviceRecord,
        peer_account: &AccountId,
        pairwise: &Pairwise,
        content: &Content,
        now: u64,
    ) -> Result<Envelope> {
        let session = self
            .by_device
            .get_mut(&peer.device_id)
            .and_then(|v| v.last_mut())
            .map(|p| &mut p.session)
            .ok_or_else(|| Error::NoSession(peer.device_id.short()))?;

        let plaintext = serde_json::to_vec(content)?;
        let olm = session
            .encrypt(&plaintext)
            .map_err(|e| Error::Wire(e.to_string()))?;
        let (olm_type, olm_ct) = olm.to_parts();

        let inner = Inner {
            from_account: identity.account_id,
            from_device: identity.device_id,
            from_olm: identity.olm.curve25519_key().into(),
            to_device: peer.device_id,
            olm_type: olm_type as u8,
            olm_ct,
        };

        let epoch = epoch_at(now);
        let tag = pairwise.tag_for(peer_account, epoch);
        // The routing header is authenticated even though it stays readable.
        let sealed = seal::seal(
            &peer.seal,
            &serde_json::to_vec(&inner)?,
            &routing_context(&tag, epoch),
        )?;
        Ok(Envelope::new(tag, epoch, sealed))
    }

    /// Open an envelope addressed to this device.
    ///
    /// Tries every existing session first; a pre-key message that no session
    /// matches opens a new inbound session, which is how a conversation starts.
    ///
    /// Decryption alone proves only that the sender holds *some* Olm identity
    /// key — not whose account it belongs to. Anyone can seal a box to us and
    /// write whatever they like in the sender fields, so those fields are
    /// checked against the claimed account's signed device roster before this
    /// returns, and a session is never filed until that check passes.
    pub fn decrypt(
        &mut self,
        identity: &mut Identity,
        envelope: &Envelope,
        directory: &dyn Directory,
    ) -> Result<Opened> {
        let inner_bytes = seal::unseal(
            identity.seal_secret(),
            &envelope.sealed,
            &routing_context(&envelope.to_tag, envelope.epoch),
        )?;
        let inner: Inner = serde_json::from_slice(&inner_bytes)?;

        if inner.to_device != identity.device_id {
            return Err(Error::Wire("envelope addressed to another device".into()));
        }

        let olm = OlmMessage::from_parts(inner.olm_type as usize, &inner.olm_ct)
            .map_err(|e| Error::Wire(e.to_string()))?;

        // An existing session is the common case after the first message.
        if let Some(sessions) = self.by_device.get_mut(&inner.from_device) {
            for peered in sessions.iter_mut() {
                if let Ok(plaintext) = peered.session.decrypt(&olm) {
                    // Recorded when the session was created, from an already
                    // authenticated source — never from this envelope.
                    let proven = peered.remote;
                    let content: Content = serde_json::from_slice(&plaintext)?;

                    // Before authenticating, not after: a device the sender
                    // added since we last heard from them is not yet on the
                    // roster we hold, and the update that fixes that is riding
                    // in this very message.
                    if let Some(chain) = &content.sender_chain {
                        directory.offer_chain(&inner.from_account, chain);
                    }

                    authenticate(directory, &inner, proven)?;
                    return Ok(Opened {
                        from_account: inner.from_account,
                        from_device: inner.from_device,
                        from_olm: proven,
                        content,
                    });
                }
            }
        }

        // No session matched. Only a pre-key message can create one.
        let prekey = match olm {
            OlmMessage::PreKey(m) => m,
            OlmMessage::Normal(_) => return Err(Error::NoSession(inner.from_device.short())),
        };

        let result = identity
            .olm
            .create_inbound_session(
                olm_session_config(),
                Curve25519PublicKey::from(inner.from_olm),
                &prekey,
            )
            .map_err(|_| Error::Decrypt)?;

        // The 3DH covers `from_olm`, so a successful decrypt proves the sender
        // holds its secret half. What it does not prove is the account.
        let proven = inner.from_olm;
        let content: Content = serde_json::from_slice(&result.plaintext)?;

        if let Some(chain) = &content.sender_chain {
            directory.offer_chain(&inner.from_account, chain);
        }

        authenticate(directory, &inner, proven)?;
        self.insert(inner.from_device, result.session, proven);

        Ok(Opened {
            from_account: inner.from_account,
            from_device: inner.from_device,
            from_olm: proven,
            content,
        })
    }
}

/// The plaintext routing header, in the exact form both ends bind to the seal.
fn routing_context(tag: &crate::tag::Tag, epoch: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(24);
    v.extend_from_slice(tag);
    v.extend_from_slice(&epoch.to_be_bytes());
    v
}

/// Resolves an account to the device roster its signed chain describes.
///
/// This is what turns "somebody encrypted this" into "this person sent it".
pub trait Directory {
    fn chain_for(&self, account: &AccountId) -> Option<ChainState>;

    /// Offered a signed chain that may extend what we already hold for this
    /// account. Implementations must only ever *extend* a chain they already
    /// have — adopting one for an unknown account would let a stranger install
    /// themselves as a contact, which only an invite may do.
    fn offer_chain(&self, _account: &AccountId, _chain: &crate::sigchain::Sigchain) {}
}

/// Confirm the claimed account really does vouch for the key that decrypted.
fn authenticate(directory: &dyn Directory, inner: &Inner, proven: DhKey) -> Result<()> {
    let state = directory
        .chain_for(&inner.from_account)
        .ok_or_else(|| Error::UnknownContact(inner.from_account.short()))?;

    // Revoked devices are removed from `devices` during validation, so a
    // lookup here is also a revocation check.
    let device = state.device(&inner.from_device).ok_or(Error::BadSignature(
        "sending device is not on that account's chain",
    ))?;

    if device.olm_identity != proven {
        return Err(Error::BadSignature(
            "sending device's key does not match the one that decrypted",
        ));
    }
    Ok(())
}

/// A successfully decrypted message, with the sender the sigchain vouches for.
#[derive(Debug, Clone)]
pub struct Opened {
    pub from_account: AccountId,
    pub from_device: DeviceId,
    /// The Olm identity key the ratchet authenticated, and which the sender's
    /// chain confirms belongs to `from_device`.
    pub from_olm: DhKey,
    pub content: Content,
}

/// Everything needed to address one contact: their verified device roster and
/// the pairwise secret shared with them.
pub struct Recipient<'a> {
    pub account: AccountId,
    pub state: &'a ChainState,
    pub pairwise: &'a Pairwise,
}

/// Encrypt one message to every live device of a contact, plus a self-copy to
/// each of my own other devices.
///
/// Returns one envelope per destination device. The caller decides how each is
/// routed — direct, relayed, or parked in a mailbox.
pub fn fan_out(
    identity: &mut Identity,
    sessions: &mut Sessions,
    to: &Recipient<'_>,
    mine: Option<&Recipient<'_>>,
    content: &Content,
    now: u64,
) -> Result<Vec<(DeviceId, Envelope)>> {
    let mut out = Vec::new();

    for device in to.state.live_devices() {
        ensure_session(identity, sessions, to.state, device)?;
        let env = sessions.encrypt_for(identity, device, &to.account, to.pairwise, content, now)?;
        out.push((device.device_id, env));
    }

    if let Some(me) = mine {
        let copy = Content::new(
            content.conversation,
            content.sent_at,
            content.seq,
            Body::SelfCopy {
                to: to.account,
                message_id: content.id,
                text: content.text().unwrap_or_default().to_string(),
            },
        );
        for device in me.state.live_devices() {
            if device.device_id == identity.device_id {
                continue;
            }
            ensure_session(identity, sessions, me.state, device)?;
            let env =
                sessions.encrypt_for(identity, device, &me.account, me.pairwise, &copy, now)?;
            out.push((device.device_id, env));
        }
    }

    if out.is_empty() {
        return Err(Error::UnknownContact(to.account.short()));
    }
    Ok(out)
}

fn ensure_session(
    identity: &Identity,
    sessions: &mut Sessions,
    state: &ChainState,
    device: &DeviceRecord,
) -> Result<()> {
    if sessions.has_session_with(&device.device_id) {
        return Ok(());
    }
    let bundle = state
        .prekeys
        .get(&device.device_id)
        .ok_or_else(|| Error::NoPrekeys(device.device_id.short()))?;
    sessions.establish(identity, device, bundle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sigchain::{Body as ChainBody, Sigchain};
    use x25519_dalek::{PublicKey as XPublic, StaticSecret as XSecret};

    const NOW: u64 = 1_755_000_000;

    /// Stands in for the app's contact store.
    struct TestDirectory(Vec<ChainState>);

    impl Directory for TestDirectory {
        fn chain_for(&self, account: &AccountId) -> Option<ChainState> {
            self.0.iter().find(|s| s.account_id == *account).cloned()
        }
    }

    struct Peer {
        identity: Identity,
        chain: Sigchain,
        sessions: Sessions,
    }

    impl Peer {
        fn new(label: &str) -> Self {
            let mut identity = Identity::create(label);
            let mut chain = Sigchain::genesis(&identity, label, NOW).unwrap();
            chain
                .append_signed_by_root(
                    &identity,
                    ChainBody::AddDevice(identity.device_record(NOW)),
                    NOW,
                )
                .unwrap();
            let bundle = identity.replenish_prekeys(20);
            chain
                .append_signed_by_device(
                    &identity,
                    ChainBody::PublishPrekeys {
                        device_id: identity.device_id,
                        bundle,
                    },
                    NOW,
                )
                .unwrap();
            Self {
                identity,
                chain,
                sessions: Sessions::new(),
            }
        }

        fn state(&self) -> ChainState {
            self.chain.validate().unwrap()
        }

        fn pairwise_with(&self, other: &Peer) -> Pairwise {
            Pairwise::derive(
                self.identity.contact_secret(),
                &self.identity.account_id,
                &other.state().contact,
                &other.identity.account_id,
            )
        }
    }

    fn text(to: &Peer, s: &str) -> Content {
        Content::new(
            to.identity.account_id,
            NOW,
            1,
            Body::Text { text: s.into() },
        )
    }

    #[test]
    fn two_strangers_can_hold_a_conversation() {
        let mut a = Peer::new("alice-laptop");
        let mut b = Peer::new("bob-laptop");

        let b_state = b.state();
        let pw_ab = a.pairwise_with(&b);
        let to_b = Recipient {
            account: b.identity.account_id,
            state: &b_state,
            pairwise: &pw_ab,
        };

        let msg = text(&b, "meet me at the usual place");
        let envelopes = fan_out(&mut a.identity, &mut a.sessions, &to_b, None, &msg, NOW).unwrap();
        assert_eq!(envelopes.len(), 1);

        let both = TestDirectory(vec![a.state(), b_state.clone()]);
        let opened = b
            .sessions
            .decrypt(&mut b.identity, &envelopes[0].1, &both)
            .unwrap();
        assert_eq!(opened.from_account, a.identity.account_id);
        assert_eq!(opened.content.text(), Some("meet me at the usual place"));

        // And back the other way, on the session the pre-key message opened.
        let a_state = a.state();
        let pw_ba = b.pairwise_with(&a);
        let to_a = Recipient {
            account: a.identity.account_id,
            state: &a_state,
            pairwise: &pw_ba,
        };
        let reply = text(&a, "understood");
        let back = fan_out(&mut b.identity, &mut b.sessions, &to_a, None, &reply, NOW).unwrap();
        let opened = a
            .sessions
            .decrypt(&mut a.identity, &back[0].1, &both)
            .unwrap();
        assert_eq!(opened.content.text(), Some("understood"));
    }

    #[test]
    fn the_routing_tag_is_recognisable_only_to_the_recipient() {
        let mut a = Peer::new("alice");
        let b = Peer::new("bob");
        let c = Peer::new("carol");

        let b_state = b.state();
        let pw_ab = a.pairwise_with(&b);
        let to_b = Recipient {
            account: b.identity.account_id,
            state: &b_state,
            pairwise: &pw_ab,
        };
        let env = fan_out(
            &mut a.identity,
            &mut a.sessions,
            &to_b,
            None,
            &text(&b, "hi"),
            NOW,
        )
        .unwrap();

        let expected = b
            .pairwise_with(&a)
            .tag_for(&b.identity.account_id, epoch_at(NOW));
        assert_eq!(env[0].1.to_tag, expected);

        // Carol, who is a contact of neither, computes a different tag.
        let carol_guess = c
            .pairwise_with(&a)
            .tag_for(&b.identity.account_id, epoch_at(NOW));
        assert_ne!(env[0].1.to_tag, carol_guess);
    }

    /// Eve holds Bob's invite — which is public by design, it is how anyone
    /// starts a conversation — and uses it to open a legitimate Olm session with
    /// Bob. She then labels her message as coming from Alice.
    ///
    /// Nothing in the Olm layer stops this: the ciphertext proves only that the
    /// sender holds *some* identity key, not whose account it belongs to. The
    /// binding has to come from the sigchain.
    #[test]
    fn a_sender_cannot_claim_someone_elses_account() {
        let mut eve = Peer::new("eve");
        let mut bob = Peer::new("bob");
        let alice = Peer::new("alice");

        let bob_state = bob.state();
        let bob_device = bob_state.devices.values().next().unwrap().clone();
        let bundle = &bob_state.prekeys[&bob_device.device_id];

        eve.sessions
            .establish(&eve.identity, &bob_device, bundle)
            .unwrap();

        let content = text(&bob, "transfer the money, love Alice");
        let session = eve
            .sessions
            .by_device
            .get_mut(&bob_device.device_id)
            .unwrap()
            .last_mut()
            .unwrap();
        let olm = session
            .session
            .encrypt(serde_json::to_vec(&content).unwrap())
            .unwrap();
        let (olm_type, olm_ct) = olm.to_parts();

        // Everything Eve is entitled to say about herself, except the two
        // identity fields, which she fills in with Alice's.
        let forged = Inner {
            from_account: alice.identity.account_id,
            from_device: alice.identity.device_id,
            from_olm: eve.identity.olm.curve25519_key().into(),
            to_device: bob_device.device_id,
            olm_type: olm_type as u8,
            olm_ct,
        };

        let sealed = seal::seal(
            &bob_device.seal,
            &serde_json::to_vec(&forged).unwrap(),
            &routing_context(&[0u8; 16], 0),
        )
        .unwrap();
        let envelope = Envelope::new([0u8; 16], 0, sealed);

        let directory = TestDirectory(vec![alice.state(), bob_state.clone(), eve.state()]);
        let result = bob
            .sessions
            .decrypt(&mut bob.identity, &envelope, &directory);

        assert!(
            result.is_err(),
            "a message claiming an account whose chain does not list the sending device must be refused"
        );
    }

    /// The same attack aimed at poisoning the session table: Eve files her own
    /// session under Alice's device id, hoping later messages inherit the trust.
    #[test]
    fn a_refused_message_does_not_leave_a_session_behind() {
        let mut eve = Peer::new("eve");
        let mut bob = Peer::new("bob");
        let alice = Peer::new("alice");

        let bob_state = bob.state();
        let bob_device = bob_state.devices.values().next().unwrap().clone();
        eve.sessions
            .establish(
                &eve.identity,
                &bob_device,
                &bob_state.prekeys[&bob_device.device_id],
            )
            .unwrap();

        let content = text(&bob, "first");
        let session = eve
            .sessions
            .by_device
            .get_mut(&bob_device.device_id)
            .unwrap()
            .last_mut()
            .unwrap();
        let (olm_type, olm_ct) = session
            .session
            .encrypt(serde_json::to_vec(&content).unwrap())
            .unwrap()
            .to_parts();

        let forged = Inner {
            from_account: alice.identity.account_id,
            from_device: alice.identity.device_id,
            from_olm: eve.identity.olm.curve25519_key().into(),
            to_device: bob_device.device_id,
            olm_type: olm_type as u8,
            olm_ct,
        };
        let sealed = seal::seal(
            &bob_device.seal,
            &serde_json::to_vec(&forged).unwrap(),
            &routing_context(&[0u8; 16], 0),
        )
        .unwrap();
        let directory = TestDirectory(vec![alice.state(), bob_state, eve.state()]);

        let _ = bob.sessions.decrypt(
            &mut bob.identity,
            &Envelope::new([0u8; 16], 0, sealed),
            &directory,
        );

        assert!(
            !bob.sessions.has_session_with(&alice.identity.device_id),
            "a rejected message must not install a session under the claimed device"
        );
    }

    #[test]
    fn a_message_from_an_account_we_do_not_know_is_refused() {
        let mut a = Peer::new("alice");
        let mut b = Peer::new("bob");

        let b_state = b.state();
        let pw = a.pairwise_with(&b);
        let to_b = Recipient {
            account: b.identity.account_id,
            state: &b_state,
            pairwise: &pw,
        };
        let env = fan_out(
            &mut a.identity,
            &mut a.sessions,
            &to_b,
            None,
            &text(&b, "unsolicited"),
            NOW,
        )
        .unwrap();

        // Bob has never added Alice, so he holds no chain for her and cannot
        // establish that this device speaks for that account.
        let empty = TestDirectory(vec![b_state]);
        assert!(b
            .sessions
            .decrypt(&mut b.identity, &env[0].1, &empty)
            .is_err());
    }

    #[test]
    fn a_third_party_cannot_open_the_envelope() {
        let mut a = Peer::new("alice");
        let b = Peer::new("bob");
        let mut c = Peer::new("carol");

        let b_state = b.state();
        let pw = a.pairwise_with(&b);
        let to_b = Recipient {
            account: b.identity.account_id,
            state: &b_state,
            pairwise: &pw,
        };
        let env = fan_out(
            &mut a.identity,
            &mut a.sessions,
            &to_b,
            None,
            &text(&b, "private"),
            NOW,
        )
        .unwrap();

        let all = TestDirectory(vec![a.state(), b.state(), c.state()]);
        assert!(c
            .sessions
            .decrypt(&mut c.identity, &env[0].1, &all)
            .is_err());
    }

    #[test]
    fn rewriting_the_routing_tag_is_detected() {
        let mut a = Peer::new("alice");
        let mut b = Peer::new("bob");

        let b_state = b.state();
        let pw = a.pairwise_with(&b);
        let to_b = Recipient {
            account: b.identity.account_id,
            state: &b_state,
            pairwise: &pw,
        };
        let mut env = fan_out(
            &mut a.identity,
            &mut a.sessions,
            &to_b,
            None,
            &text(&b, "route me"),
            NOW,
        )
        .unwrap();

        // A relay rewrites the header to misdirect the message.
        env[0].1.to_tag = [0xAAu8; 16];

        let both = TestDirectory(vec![a.state(), b_state]);
        assert!(
            b.sessions
                .decrypt(&mut b.identity, &env[0].1, &both)
                .is_err(),
            "a rewritten routing tag must not open"
        );
    }

    #[test]
    fn a_tampered_envelope_is_rejected() {
        let mut a = Peer::new("alice");
        let mut b = Peer::new("bob");

        let b_state = b.state();
        let pw = a.pairwise_with(&b);
        let to_b = Recipient {
            account: b.identity.account_id,
            state: &b_state,
            pairwise: &pw,
        };
        let mut env = fan_out(
            &mut a.identity,
            &mut a.sessions,
            &to_b,
            None,
            &text(&b, "transfer 100"),
            NOW,
        )
        .unwrap();

        let last = env[0].1.sealed.len() - 1;
        env[0].1.sealed[last] ^= 0x01;
        let both = TestDirectory(vec![a.state(), b_state.clone()]);
        assert!(b
            .sessions
            .decrypt(&mut b.identity, &env[0].1, &both)
            .is_err());
    }

    #[test]
    fn every_device_of_a_contact_gets_its_own_ciphertext() {
        let mut a = Peer::new("alice");
        let mut b = Peer::new("bob-laptop");

        // Bob adds a phone, authorised by the laptop.
        let mut phone = Identity::adopt(
            XSecret::from(b.identity.contact_secret().to_bytes()),
            b.identity.account_id,
            "bob-phone",
        );
        b.chain
            .append_signed_by_device(
                &b.identity,
                ChainBody::AddDevice(phone.device_record(NOW)),
                NOW,
            )
            .unwrap();
        let bundle = phone.replenish_prekeys(10);
        b.chain
            .append_signed_by_device(
                &phone,
                ChainBody::PublishPrekeys {
                    device_id: phone.device_id,
                    bundle,
                },
                NOW,
            )
            .unwrap();

        let b_state = b.state();
        assert_eq!(b_state.devices.len(), 2);

        let pw = a.pairwise_with(&b);
        let to_b = Recipient {
            account: b.identity.account_id,
            state: &b_state,
            pairwise: &pw,
        };
        let envelopes = fan_out(
            &mut a.identity,
            &mut a.sessions,
            &to_b,
            None,
            &text(&b, "both of you"),
            NOW,
        )
        .unwrap();

        assert_eq!(envelopes.len(), 2);
        // Same tag, different ciphertext — the network cannot tell them apart,
        // but each device can only open its own.
        assert_eq!(envelopes[0].1.to_tag, envelopes[1].1.to_tag);
        assert_ne!(envelopes[0].1.sealed, envelopes[1].1.sealed);

        let mut phone_sessions = Sessions::new();
        let for_phone = envelopes
            .iter()
            .find(|(d, _)| *d == phone.device_id)
            .unwrap();
        let for_laptop = envelopes
            .iter()
            .find(|(d, _)| *d == b.identity.device_id)
            .unwrap();

        let known = TestDirectory(vec![a.state(), b_state.clone()]);
        assert!(phone_sessions
            .decrypt(&mut phone, &for_laptop.1, &known)
            .is_err());
        let opened = phone_sessions
            .decrypt(&mut phone, &for_phone.1, &known)
            .unwrap();
        assert_eq!(opened.content.text(), Some("both of you"));

        let opened = b
            .sessions
            .decrypt(&mut b.identity, &for_laptop.1, &known)
            .unwrap();
        assert_eq!(opened.content.text(), Some("both of you"));
    }

    #[test]
    fn my_other_devices_receive_a_copy_of_what_i_send() {
        let mut a = Peer::new("alice-laptop");
        let b = Peer::new("bob");

        let mut a_phone = Identity::adopt(
            XSecret::from(a.identity.contact_secret().to_bytes()),
            a.identity.account_id,
            "alice-phone",
        );
        a.chain
            .append_signed_by_device(
                &a.identity,
                ChainBody::AddDevice(a_phone.device_record(NOW)),
                NOW,
            )
            .unwrap();
        let bundle = a_phone.replenish_prekeys(10);
        a.chain
            .append_signed_by_device(
                &a_phone,
                ChainBody::PublishPrekeys {
                    device_id: a_phone.device_id,
                    bundle,
                },
                NOW,
            )
            .unwrap();

        let a_state = a.state();
        let b_state = b.state();
        let pw_ab = a.pairwise_with(&b);
        // The secret with myself: same derivation, both ends mine.
        let pw_self = Pairwise::derive(
            a.identity.contact_secret(),
            &a.identity.account_id,
            &a_state.contact,
            &a.identity.account_id,
        );

        let to_b = Recipient {
            account: b.identity.account_id,
            state: &b_state,
            pairwise: &pw_ab,
        };
        let to_me = Recipient {
            account: a.identity.account_id,
            state: &a_state,
            pairwise: &pw_self,
        };

        let envelopes = fan_out(
            &mut a.identity,
            &mut a.sessions,
            &to_b,
            Some(&to_me),
            &text(&b, "sent from the laptop"),
            NOW,
        )
        .unwrap();

        // One for Bob, one for my phone — and none for the device that sent it.
        assert_eq!(envelopes.len(), 2);
        let mine = envelopes
            .iter()
            .find(|(d, _)| *d == a_phone.device_id)
            .expect("phone should get a copy");

        let mut phone_sessions = Sessions::new();
        let known = TestDirectory(vec![a_state.clone(), b_state.clone()]);
        let opened = phone_sessions
            .decrypt(&mut a_phone, &mine.1, &known)
            .unwrap();
        match opened.content.body {
            Body::SelfCopy { to, text, .. } => {
                assert_eq!(to, b.identity.account_id);
                assert_eq!(text, "sent from the laptop");
            }
            other => panic!("expected a self copy, got {other:?}"),
        }
    }

    #[test]
    fn sending_to_a_contact_with_no_prekeys_fails_cleanly() {
        let mut a = Peer::new("alice");
        let b = Peer::new("bob");

        let mut state = b.state();
        state.prekeys.clear();
        let pw = a.pairwise_with(&b);
        let to_b = Recipient {
            account: b.identity.account_id,
            state: &state,
            pairwise: &pw,
        };

        let err = fan_out(
            &mut a.identity,
            &mut a.sessions,
            &to_b,
            None,
            &text(&b, "hello"),
            NOW,
        );
        assert!(matches!(err, Err(Error::NoPrekeys(_))));
    }

    #[test]
    fn a_revoked_device_stops_receiving() {
        let mut a = Peer::new("alice");
        let mut b = Peer::new("bob-laptop");

        let mut phone = Identity::adopt(
            XSecret::from(b.identity.contact_secret().to_bytes()),
            b.identity.account_id,
            "bob-phone",
        );
        b.chain
            .append_signed_by_device(
                &b.identity,
                ChainBody::AddDevice(phone.device_record(NOW)),
                NOW,
            )
            .unwrap();
        let bundle = phone.replenish_prekeys(5);
        b.chain
            .append_signed_by_device(
                &phone,
                ChainBody::PublishPrekeys {
                    device_id: phone.device_id,
                    bundle,
                },
                NOW,
            )
            .unwrap();
        b.chain
            .append_signed_by_root(
                &b.identity,
                ChainBody::RevokeDevice {
                    device_id: phone.device_id,
                },
                NOW,
            )
            .unwrap();

        let b_state = b.state();
        let pw = a.pairwise_with(&b);
        let to_b = Recipient {
            account: b.identity.account_id,
            state: &b_state,
            pairwise: &pw,
        };
        let envelopes = fan_out(
            &mut a.identity,
            &mut a.sessions,
            &to_b,
            None,
            &text(&b, "laptop only"),
            NOW,
        )
        .unwrap();

        assert_eq!(envelopes.len(), 1);
        assert_eq!(envelopes[0].0, b.identity.device_id);
        let _ = phone.device_id;
    }

    #[test]
    fn a_second_message_uses_the_established_session() {
        let mut a = Peer::new("alice");
        let mut b = Peer::new("bob");

        let b_state = b.state();
        let pw = a.pairwise_with(&b);
        let to_b = Recipient {
            account: b.identity.account_id,
            state: &b_state,
            pairwise: &pw,
        };

        let first = fan_out(
            &mut a.identity,
            &mut a.sessions,
            &to_b,
            None,
            &text(&b, "one"),
            NOW,
        )
        .unwrap();
        let both = TestDirectory(vec![a.state(), b_state.clone()]);
        b.sessions
            .decrypt(&mut b.identity, &first[0].1, &both)
            .unwrap();

        let second = fan_out(
            &mut a.identity,
            &mut a.sessions,
            &to_b,
            None,
            &text(&b, "two"),
            NOW,
        )
        .unwrap();
        let opened = b
            .sessions
            .decrypt(&mut b.identity, &second[0].1, &both)
            .unwrap();
        assert_eq!(opened.content.text(), Some("two"));
        assert_eq!(b.sessions.device_count(), 1);
    }

    #[test]
    fn contact_keys_agree_between_a_devices_own_pair() {
        // Sanity: adopt() copies the account contact secret, so both of my
        // devices derive the same pairwise secret with any contact.
        let a = Peer::new("laptop");
        let phone = Identity::adopt(
            XSecret::from(a.identity.contact_secret().to_bytes()),
            a.identity.account_id,
            "phone",
        );
        assert_eq!(
            a.identity.contact_public(),
            DhKey::from(XPublic::from(phone.contact_secret()))
        );
    }
}
