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
use redb::{
    Database, MultimapTableDefinition, ReadableDatabase, ReadableMultimapTable, ReadableTable,
    ReadableTableMetadata, TableDefinition,
};
use serde::{Deserialize, Serialize};
use std::path::Path;

const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
const CHAINS: TableDefinition<&str, &[u8]> = TableDefinition::new("chains");
const CONTACTS: TableDefinition<&str, &[u8]> = TableDefinition::new("contacts");
const SESSIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("sessions");
const MESSAGES: TableDefinition<u64, &[u8]> = TableDefinition::new("messages");
/// conversation → the sequence numbers it contains. Without this, reading a
/// conversation means scanning every message ever stored.
const BY_CONVERSATION: MultimapTableDefinition<&str, u64> =
    MultimapTableDefinition::new("by_conversation");
const SEEN: TableDefinition<&[u8], u64> = TableDefinition::new("seen");
const OUTBOX: TableDefinition<u64, &[u8]> = TableDefinition::new("outbox");
/// Files being received, by transfer id. A manifest arrives before its chunks
/// and the chunks arrive in whatever order the network delivers them, so a
/// partial transfer has to survive a restart like anything else.
const TRANSFERS: TableDefinition<&str, &[u8]> = TableDefinition::new("transfers");
/// `<transfer hex>/<index, zero-padded>` → the raw chunk. Separate from the
/// manifest so a chunk arriving does not rewrite the whole record.
const TRANSFER_CHUNKS: TableDefinition<&str, &[u8]> = TableDefinition::new("transfer_chunks");

const KEY_IDENTITY: &str = "identity";
/// A name for this device chosen locally. Never leaves the machine.
const KEY_DEVICE_LABEL: &str = "device_label";
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
    /// Which message this envelope carries, so a delivery receipt can clear it.
    #[serde(default, with = "crate::identity::hex32")]
    pub message_id: [u8; 32],
}

/// A file being received, assembled one chunk at a time.
///
/// Every field except `have` and `started_at` is the sender's claim, copied from
/// the manifest. They are checked once, when the transfer begins, so that
/// nothing downstream has to trust them again.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transfer {
    #[serde(with = "crate::identity::hex32")]
    pub transfer: [u8; 32],
    pub from: AccountId,
    /// The thread this file belongs to, decided by us from the sender — never
    /// from the conversation field the sender wrote.
    pub conversation: AccountId,
    pub name: String,
    pub size: u64,
    #[serde(with = "crate::identity::hex32")]
    pub hash: [u8; 32],
    pub chunks: u32,
    /// How many distinct chunks have arrived.
    pub have: u32,
    /// Set once the file has been reassembled and handed over.
    ///
    /// The record outlives the chunks on purpose. A chunk can open a transfer,
    /// so the announcement may well arrive after the last chunk — and if
    /// completion were recorded by deleting the transfer, that late
    /// announcement would open a second, empty one and the file would show as
    /// still arriving forever.
    #[serde(default)]
    pub done: bool,
    pub started_at: u64,
}

impl Transfer {
    /// Open a transfer from a manifest, whichever message carried it — the
    /// announcement or any one of the chunks.
    ///
    /// `conversation` is decided by the caller from the authenticated sender,
    /// never read from the message: a contact must not be able to drop a file
    /// into a thread with somebody else.
    pub fn opening(
        manifest: &crate::envelope::FileManifest,
        from: AccountId,
        conversation: AccountId,
        now: u64,
    ) -> Self {
        Self {
            transfer: manifest.transfer,
            from,
            conversation,
            name: crate::envelope::safe_file_name(&manifest.name),
            size: manifest.size,
            hash: manifest.hash,
            chunks: manifest.chunks,
            have: 0,
            done: false,
            started_at: now,
        }
    }

    pub fn is_complete(&self) -> bool {
        self.have >= self.chunks
    }
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
            tx.open_multimap_table(BY_CONVERSATION)?;
            tx.open_table(SEEN)?;
            tx.open_table(OUTBOX)?;
            tx.open_table(TRANSFERS)?;
            tx.open_table(TRANSFER_CHUNKS)?;
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

                let mut index = tx.open_multimap_table(BY_CONVERSATION)?;
                index.insert(msg.conversation.to_display().as_str(), msg.seq)?;
                true
            }
        };
        tx.commit()?;
        Ok(fresh)
    }

    pub fn next_message_seq(&self) -> Result<u64> {
        self.bump(KEY_MESSAGE_SEQ)
    }

    /// The most recent `limit` messages in a conversation, oldest first.
    ///
    /// Reads through the index rather than scanning: cost is proportional to
    /// the messages in *this* conversation, not to everything ever stored.
    /// Sequence numbers are allocated monotonically, so the multimap's ordering
    /// is chronological and the tail is what we want.
    pub fn conversation(&self, other: &AccountId, limit: usize) -> Result<Vec<StoredMessage>> {
        let tx = self.db.begin_read()?;
        let index = tx.open_multimap_table(BY_CONVERSATION)?;
        let messages = tx.open_table(MESSAGES)?;

        let mut seqs: Vec<u64> = index
            .get(other.to_display().as_str())?
            .filter_map(|v| v.ok().map(|v| v.value()))
            .collect();
        if seqs.len() > limit {
            seqs.drain(..seqs.len() - limit);
        }

        let mut out = Vec::with_capacity(seqs.len());
        for seq in seqs {
            // A missing row means the index outlived the message, which should
            // not happen — but a corrupt index must not take the whole thread
            // down with it.
            if let Some(v) = messages.get(seq)? {
                out.push(serde_json::from_slice(v.value())?);
            }
        }
        Ok(out)
    }

    /// How many messages each conversation holds, largest first.
    ///
    /// Read from the index alone, so it costs nothing to show for an account
    /// with years of history behind it.
    pub fn message_counts(&self) -> Result<Vec<(AccountId, usize)>> {
        let tx = self.db.begin_read()?;
        let index = tx.open_multimap_table(BY_CONVERSATION)?;
        let mut out = Vec::new();
        for row in index.iter()? {
            let (key, seqs) = row?;
            let Ok(account) = AccountId::from_display(key.value()) else {
                continue;
            };
            out.push((account, seqs.count()));
        }
        out.sort_by(|a, b| b.1.cmp(&a.1));
        Ok(out)
    }

    /// Delete one conversation's messages, and any file transfers filed under
    /// it. Returns how many messages went.
    ///
    /// The replay set is deliberately left alone: forgetting that these ids were
    /// seen would let the same ciphertext, still sitting in a mailbox, put the
    /// conversation back. Clearing history is not the same as agreeing to
    /// receive it again.
    ///
    /// The outbox is left alone too. A message already handed over is on its
    /// way, and quietly cancelling it would tell the sender something untrue
    /// about what the other person is going to see.
    pub fn clear_conversation(&self, other: &AccountId) -> Result<usize> {
        let key = other.to_display();
        let tx = self.db.begin_write()?;
        let removed = {
            let mut index = tx.open_multimap_table(BY_CONVERSATION)?;
            let seqs: Vec<u64> = index
                .get(key.as_str())?
                .filter_map(|v| v.ok().map(|v| v.value()))
                .collect();

            let mut messages = tx.open_table(MESSAGES)?;
            for seq in &seqs {
                messages.remove(*seq)?;
            }
            index.remove_all(key.as_str())?;
            seqs.len()
        };
        tx.commit()?;

        for transfer in self.transfers_in(other)? {
            self.drop_transfer(&transfer)?;
        }
        Ok(removed)
    }

    /// Delete every message in every conversation, and every file transfer.
    pub fn clear_all_messages(&self) -> Result<usize> {
        let tx = self.db.begin_write()?;
        let removed = {
            let mut messages = tx.open_table(MESSAGES)?;
            let count = usize::try_from(messages.len()?).unwrap_or(usize::MAX);
            messages.retain(|_, _| false)?;

            // A multimap has no retain, so the keys are collected first and
            // dropped one at a time. There is one key per conversation, not one
            // per message, so the list is small whatever the history.
            let mut index = tx.open_multimap_table(BY_CONVERSATION)?;
            let keys: Vec<String> = index
                .iter()?
                .filter_map(|row| row.ok().map(|(k, _)| k.value().to_string()))
                .collect();
            for key in keys {
                index.remove_all(key.as_str())?;
            }

            let mut transfers = tx.open_table(TRANSFERS)?;
            transfers.retain(|_, _| false)?;
            let mut chunks = tx.open_table(TRANSFER_CHUNKS)?;
            chunks.retain(|_, _| false)?;
            count
        };
        tx.commit()?;
        Ok(removed)
    }

    /// The transfers filed under one conversation.
    fn transfers_in(&self, other: &AccountId) -> Result<Vec<[u8; 32]>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(TRANSFERS)?;
        let mut out = Vec::new();
        for row in t.iter()? {
            let (_, v) = row?;
            let state: Transfer = serde_json::from_slice(v.value())?;
            if state.conversation == *other {
                out.push(state.transfer);
            }
        }
        Ok(out)
    }

    // ---- this device ----

    /// A name for this device chosen here, which nothing else ever sees.
    ///
    /// The label inside the sigchain is signed and travels with the device
    /// record, so it cannot be edited after the fact without invalidating the
    /// entry that carries it. This is the local override instead: it changes
    /// what *this* installation calls itself, and no contact learns of it.
    pub fn set_device_label(&self, label: &str) -> Result<()> {
        self.put_meta(KEY_DEVICE_LABEL, label.as_bytes())
    }

    pub fn device_label(&self) -> Result<Option<String>> {
        let Some(raw) = self.get_meta(KEY_DEVICE_LABEL)? else {
            return Ok(None);
        };
        Ok(String::from_utf8(raw).ok().filter(|s| !s.trim().is_empty()))
    }

    /// Drop every queued envelope carrying this message.
    ///
    /// Called when the recipient confirms they decrypted it, which is the only
    /// signal that actually means delivered — a peer accepting an envelope only
    /// means it took the bytes.
    pub fn dequeue_message(&self, message_id: &[u8; 32]) -> Result<usize> {
        let doomed: Vec<u64> = self
            .pending()?
            .into_iter()
            .filter(|i| i.message_id == *message_id)
            .map(|i| i.seq)
            .collect();
        for seq in &doomed {
            self.dequeue(*seq)?;
        }
        Ok(doomed.len())
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

    // ---- file transfers ----

    /// Record a manifest and start accepting its chunks.
    ///
    /// This is the one place the sender's numbers are checked. Past here every
    /// bound the rest of the code relies on — that a transfer cannot exceed
    /// [`crate::envelope::MAX_FILE_BYTES`], that an index is within range, that
    /// a chunk is chunk-sized — follows from `chunks` agreeing with `size`.
    ///
    /// Returns `false` if this transfer is already known, which is what a
    /// replayed or re-sent manifest looks like.
    pub fn begin_transfer(&self, t: &Transfer) -> Result<bool> {
        if crate::envelope::chunk_count(t.size) != Some(t.chunks) {
            return Err(Error::Wire(format!(
                "a manifest claiming {} bytes in {} chunks is not self-consistent",
                t.size, t.chunks
            )));
        }
        if t.name.is_empty() {
            return Err(Error::Wire("a file manifest with no name".into()));
        }

        let key = hex(&t.transfer);
        let tx = self.db.begin_write()?;
        let fresh = {
            let mut table = tx.open_table(TRANSFERS)?;
            if table.get(key.as_str())?.is_some() {
                false
            } else {
                table.insert(key.as_str(), serde_json::to_vec(t)?.as_slice())?;
                true
            }
        };
        tx.commit()?;
        Ok(fresh)
    }

    pub fn get_transfer(&self, transfer: &[u8; 32]) -> Result<Option<Transfer>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(TRANSFERS)?;
        let Some(v) = t.get(hex(transfer).as_str())? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_slice(v.value())?))
    }

    /// File one chunk.
    ///
    /// `None` means there is no such transfer open. Otherwise the transfer comes
    /// back as it now stands, whether or not this chunk counted for anything —
    /// a duplicate of the last chunk still leaves a complete transfer, and the
    /// caller should see that.
    ///
    /// A chunk that is oversized, out of range, or one we already hold is
    /// ignored rather than treated as an error: the same envelope legitimately
    /// arrives twice over two tiers, so a duplicate is ordinary traffic, and
    /// quietly refusing junk is the whole job of this function.
    pub fn put_chunk(
        &self,
        transfer: &[u8; 32],
        index: u32,
        data: &[u8],
    ) -> Result<Option<Transfer>> {
        let key = hex(transfer);
        let tx = self.db.begin_write()?;
        let updated = {
            let mut manifests = tx.open_table(TRANSFERS)?;
            let Some(raw) = manifests.get(key.as_str())? else {
                return Ok(None);
            };
            let mut state: Transfer = serde_json::from_slice(raw.value())?;
            drop(raw);

            let usable = if index >= state.chunks {
                tracing::warn!(
                    index,
                    chunks = state.chunks,
                    "ignored an out-of-range chunk"
                );
                false
            } else if data.len() > crate::envelope::FILE_CHUNK_BYTES {
                tracing::warn!(len = data.len(), "ignored an oversized file chunk");
                false
            } else {
                true
            };

            if usable {
                let mut chunks = tx.open_table(TRANSFER_CHUNKS)?;
                let chunk_key = chunk_key(&key, index);
                // Not counted twice, which is what would otherwise let one chunk
                // sent repeatedly "complete" a transfer it never filled.
                if chunks.get(chunk_key.as_str())?.is_none() {
                    chunks.insert(chunk_key.as_str(), data)?;
                    state.have += 1;
                    manifests.insert(key.as_str(), serde_json::to_vec(&state)?.as_slice())?;
                }
            }
            Some(state)
        };
        tx.commit()?;
        Ok(updated)
    }

    /// Reassemble a completed transfer and verify it.
    ///
    /// `Ok(None)` means there is nothing to hand over: the transfer is unknown,
    /// still missing chunks, or was already taken. Only the first caller to
    /// complete a transfer gets the bytes, so a duplicate of the final chunk
    /// cannot produce the file twice.
    ///
    /// An assembled file whose hash or length disagrees with its manifest is
    /// discarded along with the transfer: the sender is a contact, but a contact
    /// whose bytes do not add up has either a bug or an intention, and neither
    /// deserves a file on disk.
    pub fn take_file(&self, transfer: &[u8; 32]) -> Result<Option<(Transfer, Vec<u8>)>> {
        let Some(mut state) = self.get_transfer(transfer)? else {
            return Ok(None);
        };
        if !state.is_complete() || state.done {
            return Ok(None);
        }

        let key = hex(transfer);
        // A hint only, and the size is a sender's claim — so a value that does
        // not fit this machine's usize costs a few reallocations, not a panic.
        let mut file = Vec::with_capacity(usize::try_from(state.size).unwrap_or(0));
        {
            let tx = self.db.begin_read()?;
            let chunks = tx.open_table(TRANSFER_CHUNKS)?;
            for index in 0..state.chunks {
                let Some(v) = chunks.get(chunk_key(&key, index).as_str())? else {
                    // `have` said otherwise, so the store is inconsistent rather
                    // than the transfer incomplete.
                    self.drop_transfer(transfer)?;
                    return Err(Error::Storage(format!("chunk {index} of {key} is missing")));
                };
                file.extend_from_slice(v.value());
            }
        }

        // Whatever happens now, these bytes are not needed again: either they
        // check out and the caller takes them, or they do not and the transfer
        // is over.
        self.drop_chunks(transfer, state.chunks)?;

        if file.len() as u64 != state.size {
            self.drop_transfer(transfer)?;
            return Err(Error::Wire(format!(
                "{} reassembled to {} bytes, not the {} it announced",
                state.name,
                file.len(),
                state.size
            )));
        }
        if *blake3::hash(&file).as_bytes() != state.hash {
            self.drop_transfer(transfer)?;
            return Err(Error::Wire(format!("{} failed its hash check", state.name)));
        }

        // The record stays behind, marked done. It is how a manifest arriving
        // after the last chunk recognises a file that is already here.
        state.done = true;
        let tx = self.db.begin_write()?;
        {
            let mut manifests = tx.open_table(TRANSFERS)?;
            manifests.insert(
                hex(transfer).as_str(),
                serde_json::to_vec(&state)?.as_slice(),
            )?;
        }
        tx.commit()?;

        Ok(Some((state, file)))
    }

    /// Forget a transfer and every chunk of it.
    pub fn drop_transfer(&self, transfer: &[u8; 32]) -> Result<()> {
        let key = hex(transfer);
        let expected = match self.get_transfer(transfer)? {
            Some(state) => state.chunks,
            None => 0,
        };
        let tx = self.db.begin_write()?;
        {
            tx.open_table(TRANSFERS)?.remove(key.as_str())?;
        }
        tx.commit()?;
        self.drop_chunks(transfer, expected)
    }

    /// Throw away the stored pieces, keeping the record of the transfer itself.
    fn drop_chunks(&self, transfer: &[u8; 32], expected: u32) -> Result<()> {
        let key = hex(transfer);
        let tx = self.db.begin_write()?;
        {
            let mut chunks = tx.open_table(TRANSFER_CHUNKS)?;
            for index in 0..expected {
                chunks.remove(chunk_key(&key, index).as_str())?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Transfers abandoned before they finished.
    ///
    /// A sender who goes offline halfway leaves chunks behind, and without this
    /// they would sit in the database until the account is deleted.
    pub fn prune_transfers(&self, now: u64, keep_secs: u64) -> Result<usize> {
        let cutoff = now.saturating_sub(keep_secs);
        let stale: Vec<[u8; 32]> = {
            let tx = self.db.begin_read()?;
            let t = tx.open_table(TRANSFERS)?;
            t.iter()?
                .filter_map(|row| row.ok())
                .filter_map(|(_, v)| serde_json::from_slice::<Transfer>(v.value()).ok())
                .filter(|t| t.started_at < cutoff)
                .map(|t| t.transfer)
                .collect()
        };
        for transfer in &stale {
            self.drop_transfer(transfer)?;
        }
        Ok(stale.len())
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

fn hex(bytes: &[u8; 32]) -> String {
    data_encoding::HEXLOWER.encode(bytes)
}

/// Zero-padded so the keys of one transfer sort in chunk order, which is what
/// makes a range scan over them meaningful even though reassembly indexes
/// directly.
fn chunk_key(transfer_hex: &str, index: u32) -> String {
    format!("{transfer_hex}/{index:08}")
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

    fn transfer_of(data: &[u8]) -> Transfer {
        Transfer {
            transfer: [5u8; 32],
            from: AccountId([1u8; 32]),
            conversation: AccountId([1u8; 32]),
            name: "notes 📓.txt".into(),
            size: data.len() as u64,
            hash: *blake3::hash(data).as_bytes(),
            chunks: crate::envelope::chunk_count(data.len() as u64).unwrap(),
            have: 0,
            done: false,
            started_at: NOW,
        }
    }

    fn chunks_of(data: &[u8]) -> Vec<&[u8]> {
        data.chunks(crate::envelope::FILE_CHUNK_BYTES).collect()
    }

    #[test]
    fn a_file_is_reassembled_from_chunks_in_any_order() {
        let (store, _dir) = store();
        // Two and a bit chunks, so ordering and the short final chunk both matter.
        let data: Vec<u8> = (0..crate::envelope::FILE_CHUNK_BYTES * 2 + 17)
            .map(|i| i.to_le_bytes()[0] ^ i.to_le_bytes()[1])
            .collect();
        let state = transfer_of(&data);
        assert_eq!(state.chunks, 3);
        assert!(store.begin_transfer(&state).unwrap());

        // Backwards, because the network does not promise order.
        let pieces = chunks_of(&data);
        for index in (0..state.chunks).rev() {
            let progress = store
                .put_chunk(&state.transfer, index, pieces[index as usize])
                .unwrap()
                .expect("the transfer is open");
            assert_eq!(progress.have, state.chunks - index);
        }

        let (finished, file) = store.take_file(&state.transfer).unwrap().unwrap();
        assert_eq!(file, data);
        assert_eq!(finished.name, "notes 📓.txt");
        assert!(finished.done);

        // The record stays, marked done, so a manifest arriving after the last
        // chunk finds a finished transfer rather than opening an empty one.
        let after = store.get_transfer(&state.transfer).unwrap().unwrap();
        assert!(after.done);
        assert!(
            !store.begin_transfer(&transfer_of(&data)).unwrap(),
            "a late manifest must not reopen a finished transfer"
        );
        // And the bytes are handed over exactly once.
        assert!(store.take_file(&state.transfer).unwrap().is_none());
    }

    #[test]
    fn the_same_chunk_twice_does_not_complete_a_transfer() {
        let (store, _dir) = store();
        let data: Vec<u8> = vec![7; crate::envelope::FILE_CHUNK_BYTES + 1];
        let state = transfer_of(&data);
        assert_eq!(state.chunks, 2);
        store.begin_transfer(&state).unwrap();

        let pieces = chunks_of(&data);
        for _ in 0..5 {
            let progress = store
                .put_chunk(&state.transfer, 0, pieces[0])
                .unwrap()
                .unwrap();
            assert_eq!(progress.have, 1, "a duplicate must not count again");
        }
        assert!(
            store.take_file(&state.transfer).unwrap().is_none(),
            "a transfer missing a chunk is not complete"
        );
    }

    #[test]
    fn junk_chunks_are_ignored_rather_than_stored() {
        let (store, _dir) = store();
        let data = vec![1u8; 64];
        let state = transfer_of(&data);
        store.begin_transfer(&state).unwrap();

        // Past the end of the file.
        let progress = store.put_chunk(&state.transfer, 9, &data).unwrap().unwrap();
        assert_eq!(progress.have, 0);
        // Larger than a chunk is allowed to be.
        let huge = vec![0u8; crate::envelope::FILE_CHUNK_BYTES + 1];
        let progress = store.put_chunk(&state.transfer, 0, &huge).unwrap().unwrap();
        assert_eq!(progress.have, 0);
        // For a transfer nobody announced.
        assert!(store.put_chunk(&[0xaa; 32], 0, &data).unwrap().is_none());
        // None of that junk left the transfer completable.
        assert!(store.take_file(&state.transfer).unwrap().is_none());
    }

    #[test]
    fn a_manifest_whose_own_numbers_disagree_is_refused() {
        let (store, _dir) = store();
        let data = vec![1u8; 100];

        let mut lying = transfer_of(&data);
        lying.chunks = 400; // 100 bytes is one chunk, not four hundred.
        assert!(store.begin_transfer(&lying).is_err());

        let mut too_big = transfer_of(&data);
        too_big.size = crate::envelope::MAX_FILE_BYTES + 1;
        assert!(store.begin_transfer(&too_big).is_err());

        let mut nameless = transfer_of(&data);
        nameless.name = String::new();
        assert!(store.begin_transfer(&nameless).is_err());
    }

    #[test]
    fn a_file_that_does_not_match_its_hash_is_thrown_away() {
        let (store, _dir) = store();
        let data = vec![3u8; 200];
        let mut state = transfer_of(&data);
        state.hash = [0u8; 32]; // Not the hash of anything we are about to send.
        store.begin_transfer(&state).unwrap();
        store.put_chunk(&state.transfer, 0, &data).unwrap();

        assert!(store.take_file(&state.transfer).is_err());
        assert!(
            store.get_transfer(&state.transfer).unwrap().is_none(),
            "a failed transfer is dropped, not left to be retried forever"
        );
    }

    #[test]
    fn abandoned_transfers_are_pruned() {
        let (store, _dir) = store();
        let data = vec![9u8; 128];
        let state = transfer_of(&data);
        store.begin_transfer(&state).unwrap();
        store.put_chunk(&state.transfer, 0, &data).unwrap();

        assert_eq!(store.prune_transfers(NOW, 3600).unwrap(), 0, "still fresh");
        assert_eq!(store.prune_transfers(NOW + 7200, 3600).unwrap(), 1);
        assert!(store.get_transfer(&state.transfer).unwrap().is_none());
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
            message_id: [9u8; 32],
        };
        store.queue(&item).unwrap();
        assert_eq!(store.pending().unwrap().len(), 1);
        store.dequeue(item.seq).unwrap();
        assert!(store.pending().unwrap().is_empty());
    }

    #[test]
    fn a_receipt_clears_every_envelope_carrying_that_message() {
        let (store, _dir) = store();
        let who = Identity::create("x").account_id;
        let id = [7u8; 32];

        // One message fans out to several devices, so several outbox entries
        // carry it. A receipt must clear all of them, not just the first.
        for _ in 0..3 {
            store
                .queue(&OutboxItem {
                    seq: store.next_outbox_seq().unwrap(),
                    to_account: who,
                    to_device: DeviceId([1u8; 32]),
                    envelope: vec![1],
                    queued_at: NOW,
                    attempts: 0,
                    message_id: id,
                })
                .unwrap();
        }
        store
            .queue(&OutboxItem {
                seq: store.next_outbox_seq().unwrap(),
                to_account: who,
                to_device: DeviceId([1u8; 32]),
                envelope: vec![2],
                queued_at: NOW,
                attempts: 0,
                message_id: [8u8; 32],
            })
            .unwrap();

        assert_eq!(store.dequeue_message(&id).unwrap(), 3);
        assert_eq!(store.pending().unwrap().len(), 1);
        // An unknown id clears nothing.
        assert_eq!(store.dequeue_message(&[0u8; 32]).unwrap(), 0);
    }

    #[test]
    fn a_conversation_reads_only_its_own_messages() {
        let (store, _dir) = store();
        let a = Identity::create("a").account_id;
        let b = Identity::create("b").account_id;

        for (who, text) in [(a, "a1"), (b, "b1"), (a, "a2"), (b, "b2"), (a, "a3")] {
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
        assert_eq!(convo.len(), 3);
        assert_eq!(convo[0].content.text(), Some("a1"));
        assert_eq!(convo[2].content.text(), Some("a3"));

        // The limit keeps the most recent, still oldest-first.
        let tail = store.conversation(&a, 2).unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].content.text(), Some("a2"));
        assert_eq!(tail[1].content.text(), Some("a3"));
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
