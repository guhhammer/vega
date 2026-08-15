//! Local persistence.
//!
//! An embedded, pure-Rust key/value store — no C to cross-compile, which is
//! what keeps the Android build simple. Everything secret is encrypted with a
//! `pickle_key` the caller supplies; this crate never decides where that key
//! lives, because on a real install it belongs in the platform keystore.

use crate::envelope::Content;
use crate::error::{Error, Result};
use crate::identity::{AccountId, DeviceId, Identity, IdentityPickle};
use crate::keys::DhKey;
use crate::session::Sessions;
use crate::sigchain::Sigchain;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use std::path::Path;

const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
const CHAINS: TableDefinition<&str, &[u8]> = TableDefinition::new("chains");
const CONTACTS: TableDefinition<&str, &[u8]> = TableDefinition::new("contacts");
const SESSIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("sessions");
const MESSAGES: TableDefinition<u64, &[u8]> = TableDefinition::new("messages");
const SEEN: TableDefinition<&[u8], u64> = TableDefinition::new("seen");
const OUTBOX: TableDefinition<u64, &[u8]> = TableDefinition::new("outbox");

const KEY_IDENTITY: &str = "identity";
const KEY_MESSAGE_SEQ: &str = "message_seq";
const KEY_OUTBOX_SEQ: &str = "outbox_seq";

/// A contact: their account id, their verified chain, and how we found them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    pub account_id: AccountId,
    pub display_name: String,
    /// Account-level X25519 key, from their genesis entry.
    pub contact_key: DhKey,
    pub added_at: u64,
    /// Set once the user has compared safety numbers out of band.
    pub verified: bool,
    /// How many chain entries of *ours* this contact has already received.
    /// Lets us attach the chain to a message only when it would tell them
    /// something new. `default` so records written before this field still load.
    #[serde(default)]
    pub chain_sent_len: u64,
}

/// One Olm session at rest.
#[derive(Debug, Serialize, Deserialize)]
struct StoredSession {
    /// vodozemac's own encrypted pickle.
    pickle: String,
    remote: DhKey,
}

/// A message as stored locally, after decryption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub seq: u64,
    pub conversation: AccountId,
    pub from_account: AccountId,
    pub from_device: DeviceId,
    pub outgoing: bool,
    pub received_at: u64,
    pub content: Content,
}

/// A message waiting to be delivered to one device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxItem {
    pub seq: u64,
    pub to_account: AccountId,
    pub to_device: DeviceId,
    /// The serialised [`crate::envelope::Envelope`], ready to hand to a transport.
    pub envelope: Vec<u8>,
    pub queued_at: u64,
    pub attempts: u32,
}

pub struct Store {
    db: Database,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Everything inside is either encrypted or somebody's message history.
        f.write_str("Store(<opaque>)")
    }
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = Database::create(path)?;
        // Creating every table up front means read transactions never fail
        // just because nothing has been written yet.
        let tx = db.begin_write()?;
        {
            tx.open_table(META)?;
            tx.open_table(CHAINS)?;
            tx.open_table(CONTACTS)?;
            tx.open_table(SESSIONS)?;
            tx.open_table(MESSAGES)?;
            tx.open_table(SEEN)?;
            tx.open_table(OUTBOX)?;
        }
        tx.commit()?;
        Ok(Self { db })
    }

    // ---- identity ----

    pub fn save_identity(&self, identity: &Identity, pickle_key: &[u8; 32]) -> Result<()> {
        let blob = serde_json::to_vec(&identity.pickle(pickle_key))?;
        self.put_meta(KEY_IDENTITY, &blob)
    }

    pub fn load_identity(&self, pickle_key: &[u8; 32]) -> Result<Option<Identity>> {
        let Some(blob) = self.get_meta(KEY_IDENTITY)? else {
            return Ok(None);
        };
        let pickle: IdentityPickle = serde_json::from_slice(&blob)?;
        Ok(Some(Identity::from_pickle(&pickle, pickle_key)?))
    }

    // ---- sigchains ----

    pub fn save_chain(&self, account: &AccountId, chain: &Sigchain) -> Result<()> {
        // Never persist a chain we have not verified — a corrupt chain on disk
        // is indistinguishable from one an attacker planted there.
        chain.validate()?;
        let bytes = serde_json::to_vec(chain)?;
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(CHAINS)?;
            t.insert(account.to_display().as_str(), bytes.as_slice())?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_chain(&self, account: &AccountId) -> Result<Option<Sigchain>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(CHAINS)?;
        let Some(v) = t.get(account.to_display().as_str())? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_slice(v.value())?))
    }

    // ---- contacts ----

    pub fn put_contact(&self, contact: &Contact) -> Result<()> {
        let bytes = serde_json::to_vec(contact)?;
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(CONTACTS)?;
            t.insert(contact.account_id.to_display().as_str(), bytes.as_slice())?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_contact(&self, account: &AccountId) -> Result<Option<Contact>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(CONTACTS)?;
        let Some(v) = t.get(account.to_display().as_str())? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_slice(v.value())?))
    }

    pub fn list_contacts(&self) -> Result<Vec<Contact>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(CONTACTS)?;
        let mut out = Vec::new();
        for row in t.iter()? {
            let (_, v) = row?;
            out.push(serde_json::from_slice(v.value())?);
        }
        Ok(out)
    }

    // ---- olm sessions ----

    pub fn save_sessions(&self, sessions: &Sessions, pickle_key: &[u8; 32]) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(SESSIONS)?;
            for (device, list) in sessions.iter() {
                for (i, peered) in list.iter().enumerate() {
                    let key = format!("{}/{i}", device.to_display());
                    // The remote identity key is stored beside the pickle: it is
                    // what proves who the session belongs to, and losing it would
                    // mean falling back to trusting the envelope.
                    let row = StoredSession {
                        pickle: peered.session.pickle().encrypt(pickle_key),
                        remote: peered.remote,
                    };
                    t.insert(key.as_str(), serde_json::to_vec(&row)?.as_slice())?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_sessions(&self, pickle_key: &[u8; 32]) -> Result<Sessions> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(SESSIONS)?;
        let mut sessions = Sessions::new();
        for row in t.iter()? {
            let (k, v) = row?;
            let key = k.value();
            let Some((device_part, _)) = key.rsplit_once('/') else {
                continue;
            };
            let device = DeviceId::from_display(device_part)?;
            let row: StoredSession = serde_json::from_slice(v.value())?;
            let pickle = vodozemac::olm::SessionPickle::from_encrypted(&row.pickle, pickle_key)
                .map_err(|e| Error::Storage(e.to_string()))?;
            sessions.insert(
                device,
                vodozemac::olm::Session::from_pickle(pickle),
                row.remote,
            );
        }
        Ok(sessions)
    }

    // ---- messages ----

    /// Append a message. Returns `false` if this id was already stored, which
    /// is how replayed ciphertext gets dropped instead of duplicating a chat.
    pub fn append_message(&self, msg: &StoredMessage) -> Result<bool> {
        let tx = self.db.begin_write()?;
        let fresh = {
            let mut seen = tx.open_table(SEEN)?;
            if seen.get(&msg.content.id[..])?.is_some() {
                false
            } else {
                seen.insert(&msg.content.id[..], msg.received_at)?;
                let mut t = tx.open_table(MESSAGES)?;
                let bytes = serde_json::to_vec(msg)?;
                t.insert(msg.seq, bytes.as_slice())?;
                true
            }
        };
        tx.commit()?;
        Ok(fresh)
    }

    pub fn next_message_seq(&self) -> Result<u64> {
        self.bump(KEY_MESSAGE_SEQ)
    }

    /// Messages in a conversation, oldest first.
    pub fn conversation(&self, other: &AccountId, limit: usize) -> Result<Vec<StoredMessage>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(MESSAGES)?;
        let mut out = Vec::new();
        for row in t.iter()? {
            let (_, v) = row?;
            let m: StoredMessage = serde_json::from_slice(v.value())?;
            if m.conversation == *other {
                out.push(m);
            }
        }
        if out.len() > limit {
            out.drain(..out.len() - limit);
        }
        Ok(out)
    }

    /// Forget message ids older than `keep_secs`.
    ///
    /// The set exists to drop replays, and it grew without bound. A window is
    /// safe because the Olm ratchet already refuses a replay inside a live
    /// session; this layer only catches the same message arriving twice over
    /// two different tiers, which happens within seconds, not months.
    pub fn prune_seen(&self, now: u64, keep_secs: u64) -> Result<usize> {
        let cutoff = now.saturating_sub(keep_secs);
        let tx = self.db.begin_write()?;
        let mut removed = 0;
        {
            let mut t = tx.open_table(SEEN)?;
            let stale: Vec<Vec<u8>> = t
                .iter()?
                .filter_map(|row| row.ok())
                .filter(|(_, v)| v.value() < cutoff)
                .map(|(k, _)| k.value().to_vec())
                .collect();
            for key in stale {
                t.remove(key.as_slice())?;
                removed += 1;
            }
        }
        tx.commit()?;
        Ok(removed)
    }

    // ---- outbox ----

    pub fn queue(&self, item: &OutboxItem) -> Result<()> {
        let bytes = serde_json::to_vec(item)?;
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(OUTBOX)?;
            t.insert(item.seq, bytes.as_slice())?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn next_outbox_seq(&self) -> Result<u64> {
        self.bump(KEY_OUTBOX_SEQ)
    }

    pub fn pending(&self) -> Result<Vec<OutboxItem>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(OUTBOX)?;
        let mut out = Vec::new();
        for row in t.iter()? {
            let (_, v) = row?;
            out.push(serde_json::from_slice(v.value())?);
        }
        Ok(out)
    }

    pub fn dequeue(&self, seq: u64) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(OUTBOX)?;
            t.remove(seq)?;
        }
        tx.commit()?;
        Ok(())
    }

    // ---- helpers ----

    fn bump(&self, key: &str) -> Result<u64> {
        let tx = self.db.begin_write()?;
        let next = {
            let mut t = tx.open_table(META)?;
            let current = t
                .get(key)?
                .and_then(|v| v.value().try_into().ok().map(u64::from_be_bytes))
                .unwrap_or(0);
            let next = current + 1;
            t.insert(key, &next.to_be_bytes()[..])?;
            next
        };
        tx.commit()?;
        Ok(next)
    }

    fn put_meta(&self, key: &str, value: &[u8]) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(META)?;
            t.insert(key, value)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn get_meta(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(META)?;
        Ok(t.get(key)?.map(|v| v.value().to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::Body;
    use crate::sigchain::{Body as ChainBody, Sigchain};

    const NOW: u64 = 1_755_000_000;

    fn store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("vega.redb")).unwrap();
        (store, dir)
    }

    #[test]
    fn identity_survives_a_restart() {
        let (store, _dir) = store();
        let key = [7u8; 32];
        let identity = Identity::create("laptop");
        let (account, device) = (identity.account_id, identity.device_id);

        store.save_identity(&identity, &key).unwrap();
        let loaded = store.load_identity(&key).unwrap().unwrap();

        assert_eq!(loaded.account_id, account);
        assert_eq!(loaded.device_id, device);
        assert_eq!(loaded.contact_public(), identity.contact_public());
        assert_eq!(loaded.seal_public(), identity.seal_public());
        assert!(loaded.has_root());
    }

    #[test]
    fn the_wrong_pickle_key_does_not_open_the_identity() {
        let (store, _dir) = store();
        store
            .save_identity(&Identity::create("laptop"), &[1u8; 32])
            .unwrap();
        assert!(store.load_identity(&[2u8; 32]).is_err());
    }

    #[test]
    fn an_invalid_chain_is_never_written() {
        let (store, _dir) = store();
        let identity = Identity::create("laptop");
        let mut chain = Sigchain::genesis(&identity, "me", NOW).unwrap();
        chain
            .append_signed_by_root(
                &identity,
                ChainBody::AddDevice(identity.device_record(NOW)),
                NOW,
            )
            .unwrap();
        store.save_chain(&identity.account_id, &chain).unwrap();
        assert!(store.load_chain(&identity.account_id).unwrap().is_some());

        let empty = Sigchain::default();
        assert!(store.save_chain(&identity.account_id, &empty).is_err());
        // The good chain is still there.
        assert_eq!(
            store
                .load_chain(&identity.account_id)
                .unwrap()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn replayed_messages_are_dropped() {
        let (store, _dir) = store();
        let who = Identity::create("x").account_id;
        let msg = StoredMessage {
            seq: store.next_message_seq().unwrap(),
            conversation: who,
            from_account: who,
            from_device: DeviceId([3u8; 32]),
            outgoing: false,
            received_at: NOW,
            content: Content::new(who, NOW, 1, Body::Text { text: "hi".into() }),
        };

        assert!(store.append_message(&msg).unwrap());
        // Same content id arriving again — a replayed ciphertext.
        let mut replay = msg.clone();
        replay.seq = store.next_message_seq().unwrap();
        assert!(!store.append_message(&replay).unwrap());
        assert_eq!(store.conversation(&who, 100).unwrap().len(), 1);
    }

    #[test]
    fn conversation_returns_only_that_conversation_in_order() {
        let (store, _dir) = store();
        let a = Identity::create("a").account_id;
        let b = Identity::create("b").account_id;

        for (who, text) in [(a, "a1"), (b, "b1"), (a, "a2")] {
            let msg = StoredMessage {
                seq: store.next_message_seq().unwrap(),
                conversation: who,
                from_account: who,
                from_device: DeviceId([0u8; 32]),
                outgoing: false,
                received_at: NOW,
                content: Content::new(who, NOW, 1, Body::Text { text: text.into() }),
            };
            store.append_message(&msg).unwrap();
        }

        let convo = store.conversation(&a, 100).unwrap();
        assert_eq!(convo.len(), 2);
        assert_eq!(convo[0].content.text(), Some("a1"));
        assert_eq!(convo[1].content.text(), Some("a2"));
    }

    #[test]
    fn the_dedupe_set_is_pruned_but_recent_replays_still_bounce() {
        let (store, _dir) = store();
        let who = Identity::create("x").account_id;

        let push = |text: &str, at: u64| {
            let msg = StoredMessage {
                seq: store.next_message_seq().unwrap(),
                conversation: who,
                from_account: who,
                from_device: DeviceId([0u8; 32]),
                outgoing: false,
                received_at: at,
                content: Content::new(who, at, 1, Body::Text { text: text.into() }),
            };
            assert!(store.append_message(&msg).unwrap());
            msg
        };

        let old = push("old", NOW - 10_000);
        let recent = push("recent", NOW);

        assert_eq!(store.prune_seen(NOW, 5_000).unwrap(), 1);

        // The pruned id is forgotten, so an ancient replay would be re-accepted —
        // acceptable, and exactly the trade the window makes.
        assert!(store.append_message(&old).unwrap());
        // The recent one is still guarded.
        assert!(!store.append_message(&recent).unwrap());
    }

    #[test]
    fn the_outbox_is_a_queue() {
        let (store, _dir) = store();
        let who = Identity::create("x").account_id;
        let item = OutboxItem {
            seq: store.next_outbox_seq().unwrap(),
            to_account: who,
            to_device: DeviceId([1u8; 32]),
            envelope: vec![1, 2, 3],
            queued_at: NOW,
            attempts: 0,
        };
        store.queue(&item).unwrap();
        assert_eq!(store.pending().unwrap().len(), 1);
        store.dequeue(item.seq).unwrap();
        assert!(store.pending().unwrap().is_empty());
    }

    #[test]
    fn sequence_numbers_do_not_repeat() {
        let (store, _dir) = store();
        let a = store.next_message_seq().unwrap();
        let b = store.next_message_seq().unwrap();
        assert!(b > a);
    }

    #[test]
    fn contacts_round_trip() {
        let (store, _dir) = store();
        let identity = Identity::create("them");
        let contact = Contact {
            account_id: identity.account_id,
            display_name: "Bob".into(),
            contact_key: identity.contact_public(),
            added_at: NOW,
            verified: false,
            chain_sent_len: 0,
        };
        store.put_contact(&contact).unwrap();
        let got = store.get_contact(&identity.account_id).unwrap().unwrap();
        assert_eq!(got.display_name, "Bob");
        assert_eq!(store.list_contacts().unwrap().len(), 1);
    }
}
