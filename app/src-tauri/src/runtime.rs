//! Where `vega-core` and `vega-net` meet.
//!
//! Core turns messages into sealed envelopes; net moves opaque bytes. This
//! module is the only place that knows both, and it owns the decisions that
//! need both: which peer to hand an envelope to, what to do when that fails,
//! and when a received envelope becomes a message in the UI.
//!
//! Nothing here performs network I/O. Every method is synchronous and returns a
//! *plan* — deliveries to attempt, records to publish, lookups to make. The
//! caller runs those without holding the lock, then reports back. A slow peer
//! therefore stalls one delivery rather than the whole application.

use crate::invite::Invite;
use std::collections::{HashMap, HashSet};
use vega_core::session::Directory;
use vega_core::{
    envelope::Body, fan_out, AccountId, ChainState, Content, Envelope, Identity, Recipient,
    Sessions, Sigchain, Store,
};
use vega_net::{AddressRecord, Multiaddr, NetEvent, PeerId};

/// Top up one-time keys once this many remain.
///
/// Each new inbound session burns one. At zero, every new conversation falls
/// back to the reusable fallback key, which costs forward secrecy on its first
/// message — so the threshold sits high enough that we refill long before a
/// normally active account could get there.
const PREKEY_LOW_WATER: usize = 20;

/// How many to publish at a time. Bounded, because the whole chain travels
/// inside messages and rendezvous records.
const PREKEY_BATCH: usize = 50;

/// How long a message id stays in the replay set.
pub const SEEN_WINDOW_SECS: u64 = 14 * 24 * 3600;

/// How long a half-finished file transfer is kept before its chunks are thrown
/// away.
///
/// Matched to `vega_net::protocol::MAX_PARK_SECS`: a sender who goes offline
/// mid-transfer may have parked the rest in a mailbox, and those chunks are
/// collectable for a week. Pruning sooner would discard the near-complete
/// transfer just before the missing piece arrived.
pub const TRANSFER_WINDOW_SECS: u64 = 7 * 24 * 3600;

/// Answers "which devices speak for this account?" from what we have stored.
///
/// Only chains we accepted through an invite are here, so a message from a
/// stranger has nowhere to resolve and is refused — contact exchange stays the
/// single trust root.
struct StoreDirectory<'a> {
    store: &'a Store,
    me: AccountId,
    my_chain: &'a Sigchain,
}

impl Directory for StoreDirectory<'_> {
    fn chain_for(&self, account: &AccountId) -> Option<ChainState> {
        if *account == self.me {
            return self.my_chain.validate().ok();
        }
        self.store.load_chain(account).ok()??.validate().ok()
    }

    fn offer_chain(&self, account: &AccountId, offered: &Sigchain) {
        if *account == self.me {
            return;
        }
        // Extend only. Adopting a chain for an account we have never met would
        // let anyone who can reach us install themselves as a contact.
        let Ok(Some(mut known)) = self.store.load_chain(account) else {
            return;
        };
        match known.merge(offered) {
            Ok(true) => {
                if let Err(e) = self.store.save_chain(account, &known) {
                    tracing::warn!(error = %e, "could not persist an extended chain");
                }
            }
            Ok(false) => {}
            Err(e) => tracing::warn!(%account, error = %e, "refused a chain update"),
        }
    }
}

/// What happened to one received envelope.
pub enum Received {
    /// A message worth showing. Carries the conversation it belongs to.
    Message(AccountId),
    /// The last chunk of a file arrived and the whole thing checked out. The
    /// caller writes it to disk — this module deals in bytes, not paths.
    File {
        conversation: AccountId,
        transfer: [u8; 32],
        name: String,
        bytes: Vec<u8>,
    },
    /// Decrypted, but nothing for the UI (a receipt, a sync copy, one chunk of
    /// a file still arriving).
    Housekeeping,
    /// Not for us. Expected and cheap — on a LAN every peer sees every envelope.
    NotOurs,
}

/// One queued envelope and the peers worth offering it to.
pub struct Delivery {
    pub seq: u64,
    pub to_account: AccountId,
    pub envelope: Vec<u8>,
    pub targets: Vec<PeerId>,
}

/// A rendezvous lookup: where to look, and how to open what is found.
pub struct LookupPlan {
    pub account: AccountId,
    pub key: [u8; 32],
    pub record_key: [u8; 32],
}

pub struct Runtime {
    pub identity: Identity,
    pub chain: Sigchain,
    pub sessions: Sessions,
    pub store: Store,
    pickle_key: [u8; 32],

    /// Peers we can hand an envelope to this instant.
    connected: HashSet<PeerId>,
    /// Account → peer, learned from a rendezvous record. Absent on the LAN,
    /// where we do not know which peer is which until they decrypt something.
    routes: HashMap<AccountId, PeerId>,
}

impl Runtime {
    pub fn new(
        identity: Identity,
        chain: Sigchain,
        sessions: Sessions,
        store: Store,
        pickle_key: [u8; 32],
    ) -> Self {
        Self {
            identity,
            chain,
            sessions,
            store,
            pickle_key,
            connected: HashSet::new(),
            routes: HashMap::new(),
        }
    }

    pub fn connected_count(&self) -> usize {
        self.connected.len()
    }

    pub fn on_net_event(&mut self, event: &NetEvent) {
        match event {
            NetEvent::PeerConnected(p) => {
                self.connected.insert(*p);
            }
            NetEvent::PeerDisconnected(p) => {
                self.connected.remove(p);
                self.routes.retain(|_, v| v != p);
            }
            _ => {}
        }
    }

    fn directory(&self) -> StoreDirectory<'_> {
        StoreDirectory {
            store: &self.store,
            me: self.identity.account_id,
            my_chain: &self.chain,
        }
    }

    // ---- contacts -------------------------------------------------------

    pub fn my_invite(&self, display_name: &str) -> vega_core::Result<String> {
        Invite {
            account_id: self.identity.account_id,
            display_name: display_name.to_string(),
            contact_key: self.identity.contact_public(),
            chain: self.chain.clone(),
        }
        .encode()
    }

    /// Accept an invite: verify it, then remember the person and their chain.
    pub fn add_contact(&mut self, encoded: &str) -> vega_core::Result<AccountId> {
        let invite = Invite::decode(encoded)?;
        if invite.account_id == self.identity.account_id {
            return Err(vega_core::Error::Wire("that invite is your own".into()));
        }

        self.store.save_chain(&invite.account_id, &invite.chain)?;
        self.store.put_contact(&vega_core::Contact {
            account_id: invite.account_id,
            display_name: invite.display_name,
            contact_key: invite.contact_key,
            added_at: vega_core::now(),
            verified: false,
            chain_sent_len: 0,
        })?;
        Ok(invite.account_id)
    }

    // ---- prekeys --------------------------------------------------------

    /// Publish more one-time keys when the supply runs low.
    ///
    /// The new keys go into our own chain, which then reaches contacts two ways:
    /// attached to the next message we send them, and inside the rendezvous
    /// record they fetch when they need to find us. Neither route needs a
    /// server, and neither tells anyone who is not already a contact anything.
    pub fn maintain_prekeys(&mut self) -> vega_core::Result<bool> {
        if self.identity.one_time_keys_left() > PREKEY_LOW_WATER {
            return Ok(false);
        }

        let bundle = self.identity.replenish_prekeys(PREKEY_BATCH);
        let device_id = self.identity.device_id;
        self.chain.append_signed_by_device(
            &self.identity,
            vega_core::sigchain::Body::PublishPrekeys { device_id, bundle },
            vega_core::now(),
        )?;

        self.store
            .save_chain(&self.identity.account_id, &self.chain)?;
        self.persist()?;

        tracing::info!(
            remaining = self.identity.one_time_keys_left(),
            "published a fresh batch of one-time keys"
        );
        Ok(true)
    }

    /// Periodic housekeeping. Costs nothing when there is nothing to do.
    pub fn sweep(&self) {
        let now = vega_core::now();
        match self.store.prune_seen(now, SEEN_WINDOW_SECS) {
            Ok(n) if n > 0 => tracing::debug!(pruned = n, "trimmed the replay set"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "could not prune the replay set"),
        }
        // A sender who vanished halfway leaves chunks behind. Without this they
        // would sit in the database for as long as the account exists.
        match self.store.prune_transfers(now, TRANSFER_WINDOW_SECS) {
            Ok(n) if n > 0 => tracing::info!(pruned = n, "dropped abandoned file transfers"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "could not prune file transfers"),
        }
    }

    // ---- sending --------------------------------------------------------

    /// Who a contact is, the devices their signed chain vouches for, and the
    /// pairwise secret the routing tag comes from.
    ///
    /// A contact we hold no chain for is unreachable rather than merely unknown:
    /// without the roster there is no device to encrypt to.
    fn address(
        &self,
        to: &AccountId,
    ) -> vega_core::Result<(vega_core::Contact, ChainState, vega_core::Pairwise)> {
        let contact = self
            .store
            .get_contact(to)?
            .ok_or_else(|| vega_core::Error::UnknownContact(to.short()))?;
        let state = self
            .store
            .load_chain(to)?
            .ok_or_else(|| vega_core::Error::UnknownContact(to.short()))?
            .validate()?;
        let pairwise = self
            .identity
            .pairwise_with(&contact.contact_key, &contact.account_id);
        Ok((contact, state, pairwise))
    }

    /// Encrypt a message to a contact and queue it for delivery.
    pub fn send_text(&mut self, to: &AccountId, text: &str) -> vega_core::Result<()> {
        let (contact, their_state, pairwise) = self.address(to)?;
        let my_state = self.chain.validate()?;

        let now = vega_core::now();
        let pairwise_self = self
            .identity
            .pairwise_with(&my_state.contact, &self.identity.account_id);

        let mut content = Content::new(
            *to,
            now,
            self.store.next_message_seq()?,
            Body::Text { text: text.into() },
        );

        // Carry our chain only when this contact has not seen the current one.
        // Attaching it to every message would bloat every message for nothing.
        let chain_len = self.chain.len() as u64;
        let carries_chain = contact.chain_sent_len < chain_len;
        if carries_chain {
            content = content.with_chain(self.chain.clone());
        }

        let me = self.identity.account_id;
        let my_device = self.identity.device_id;
        let message_id = content.id;

        let envelopes = fan_out(
            &mut self.identity,
            &mut self.sessions,
            &Recipient {
                account: *to,
                state: &their_state,
                pairwise: &pairwise,
            },
            Some(&Recipient {
                account: me,
                state: &my_state,
                pairwise: &pairwise_self,
            }),
            &content,
            now,
        )?;

        // Record it locally before anything touches the network, so the message
        // is never lost because delivery happened to fail.
        self.store.append_message(&vega_core::StoredMessage {
            seq: self.store.next_message_seq()?,
            conversation: *to,
            from_account: me,
            from_device: my_device,
            outgoing: true,
            received_at: now,
            content,
        })?;

        for (device, envelope) in envelopes {
            self.store.queue(&vega_core::OutboxItem {
                seq: self.store.next_outbox_seq()?,
                to_account: *to,
                to_device: device,
                envelope: envelope.to_bytes()?,
                queued_at: now,
                attempts: 0,
                message_id,
            })?;
        }

        if carries_chain {
            let mut updated = contact;
            updated.chain_sent_len = chain_len;
            self.store.put_contact(&updated)?;
        }

        self.persist()?;
        Ok(())
    }

    /// Send a file as a manifest followed by its bytes in chunks.
    ///
    /// Returns the transfer id, which is what names the file on disk at both
    /// ends. The whole file is turned into envelopes here and queued at once:
    /// delivery, retries and climbing the transport ladder are the outbox's job
    /// and work exactly as they do for a text message.
    ///
    /// No self-copy. A text message is echoed to the sender's own devices
    /// because it is cheap; a file would double a ten-megabyte send to give
    /// another device a copy of something it can already be sent directly. That
    /// means a file sent from the laptop does not appear on the phone — a real
    /// limitation, and a better one than silently doubling everyone's traffic.
    pub fn send_file(
        &mut self,
        to: &AccountId,
        name: &str,
        bytes: &[u8],
    ) -> vega_core::Result<[u8; 32]> {
        let size = bytes.len() as u64;
        let chunks = vega_core::chunk_count(size).ok_or_else(|| {
            vega_core::Error::File(if size == 0 {
                "that file is empty".to_string()
            } else {
                format!(
                    "that file is {:.1} MB — Vega sends at most {} MB",
                    size as f64 / (1024.0 * 1024.0),
                    vega_core::MAX_FILE_BYTES / (1024 * 1024)
                )
            })
        })?;

        let (contact, their_state, pairwise) = self.address(to)?;
        let now = vega_core::now();

        let mut transfer = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut transfer);
        let hash = *blake3::hash(bytes).as_bytes();

        // The name is cleaned before it is sent, not only on arrival. The
        // recipient sanitises it again — they have to, since anyone could send
        // them anything — but a name that is safe at both ends is also the name
        // the sender sees in their own thread.
        let name = vega_core::safe_file_name(name);

        let recipient = Recipient {
            account: *to,
            state: &their_state,
            pairwise: &pairwise,
        };

        let file = vega_core::envelope::FileManifest {
            transfer,
            name,
            size,
            hash,
            chunks,
        };

        // The announcement carries the chain when the contact is behind, exactly
        // as a text message would. The chunks never do: they are already the
        // largest envelopes Vega produces, and a chain on top could push one
        // past what a peer will accept.
        let mut manifest = Content::new(
            *to,
            now,
            self.store.next_message_seq()?,
            Body::File { file: file.clone() },
        );
        let chain_len = self.chain.len() as u64;
        let carries_chain = contact.chain_sent_len < chain_len;
        if carries_chain {
            manifest = manifest.with_chain(self.chain.clone());
        }
        let manifest_id = manifest.id;

        let envelopes = fan_out(
            &mut self.identity,
            &mut self.sessions,
            &recipient,
            None,
            &manifest,
            now,
        )?;

        // Stored before anything is queued, so the file shows in the sender's
        // own thread whether or not delivery ever succeeds.
        self.store.append_message(&vega_core::StoredMessage {
            seq: self.store.next_message_seq()?,
            conversation: *to,
            from_account: self.identity.account_id,
            from_device: self.identity.device_id,
            outgoing: true,
            received_at: now,
            content: manifest,
        })?;

        for (device, envelope) in envelopes {
            self.store.queue(&vega_core::OutboxItem {
                seq: self.store.next_outbox_seq()?,
                to_account: *to,
                to_device: device,
                envelope: envelope.to_bytes()?,
                queued_at: now,
                attempts: 0,
                message_id: manifest_id,
            })?;
        }

        for (index, piece) in bytes.chunks(vega_core::FILE_CHUNK_BYTES).enumerate() {
            // `chunk_count` already refused anything over MAX_FILE_BYTES, so
            // this cannot fail — checked rather than asserted because the cost
            // is one comparison and the alternative is a silent wrap.
            let index = u32::try_from(index)
                .map_err(|_| vega_core::Error::File("that file has too many chunks".into()))?;
            let content = Content::new(
                *to,
                now,
                self.store.next_message_seq()?,
                Body::FileChunk {
                    file: file.clone(),
                    index,
                    data: piece.to_vec(),
                },
            );
            let chunk_id = content.id;
            let envelopes = fan_out(
                &mut self.identity,
                &mut self.sessions,
                &recipient,
                None,
                &content,
                now,
            )?;
            for (device, envelope) in envelopes {
                self.store.queue(&vega_core::OutboxItem {
                    seq: self.store.next_outbox_seq()?,
                    to_account: *to,
                    to_device: device,
                    envelope: envelope.to_bytes()?,
                    queued_at: now,
                    attempts: 0,
                    message_id: chunk_id,
                })?;
            }
        }

        if carries_chain {
            let mut updated = contact;
            updated.chain_sent_len = chain_len;
            self.store.put_contact(&updated)?;
        }

        tracing::info!(%to, chunks, size, "queued a file");
        self.persist()?;
        Ok(transfer)
    }

    // ---- receiving ------------------------------------------------------

    /// Try to open an envelope that arrived from the network.
    pub fn receive(&mut self, bytes: &[u8]) -> Received {
        let Ok(envelope) = Envelope::from_bytes(bytes) else {
            return Received::NotOurs;
        };

        // Borrowed from disjoint fields, so the sender check can consult stored
        // chains while the session table and identity are mutably in use.
        let directory = StoreDirectory {
            store: &self.store,
            me: self.identity.account_id,
            my_chain: &self.chain,
        };

        // Failing to decrypt is the common case, not an error: on a LAN an
        // envelope is offered to every connected peer and only one can open it.
        // A sender that fails authentication lands here too — also silent,
        // because a forged message deserves no more attention than noise.
        let Ok(opened) = self
            .sessions
            .decrypt(&mut self.identity, &envelope, &directory)
        else {
            return Received::NotOurs;
        };

        let now = vega_core::now();
        let (conversation, outgoing) = match &opened.content.body {
            // Filed under the *sender*, not under the label the sender chose.
            // `content.conversation` is the recipient from the sender's point of
            // view, so trusting it would both misfile every incoming message and
            // let a contact drop messages into a conversation with someone else.
            Body::Text { .. } => (opened.from_account, false),

            // Only my own devices may claim to be echoing something I sent.
            // Without this a contact could plant messages in my outgoing column.
            Body::SelfCopy { to, .. } => {
                if opened.from_account != self.identity.account_id {
                    tracing::warn!(
                        from = %opened.from_account,
                        "discarded a self-copy from another account"
                    );
                    return Received::NotOurs;
                }
                (*to, true)
            }

            // The only signal that actually means *delivered*. A peer accepting
            // an envelope only means it took the bytes; it may not have been
            // the recipient at all, since delivery on a LAN is a broadcast.
            Body::Receipt { message_id } => {
                match self.store.dequeue_message(message_id) {
                    Ok(n) if n > 0 => tracing::debug!(cleared = n, "receipt cleared the outbox"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "could not clear the outbox"),
                }
                let _ = self.persist();
                return Received::Housekeeping;
            }

            // The announcement. It opens the transfer if no chunk has already
            // done so, and it is the message the thread shows either way.
            Body::File { file } => {
                // `begin_transfer` is where the sender's arithmetic is checked.
                // A manifest that fails it is not shown at all: a message
                // announcing a file that can never complete is worse than none.
                let state = vega_core::Transfer::opening(
                    file,
                    opened.from_account,
                    opened.from_account,
                    now,
                );
                if let Err(e) = self.store.begin_transfer(&state) {
                    tracing::warn!(from = %opened.from_account, error = %e, "refused a file manifest");
                    return Received::Housekeeping;
                }
                (opened.from_account, false)
            }

            // Chunks are not messages: not stored as one, not shown, not
            // receipted. The manifest they belong to is what the UI tracks.
            Body::FileChunk { file, index, data } => {
                // A chunk that arrives before the announcement opens the
                // transfer itself. Already open is the ordinary case and
                // `begin_transfer` reports it without disturbing what is there.
                let state = vega_core::Transfer::opening(
                    file,
                    opened.from_account,
                    opened.from_account,
                    now,
                );
                if let Err(e) = self.store.begin_transfer(&state) {
                    tracing::warn!(from = %opened.from_account, error = %e, "refused a file chunk");
                    return Received::Housekeeping;
                }

                let progress = match self.store.put_chunk(&file.transfer, *index, data) {
                    Ok(Some(progress)) => progress,
                    // Opened a moment ago, so this can only be a transfer that
                    // completed and was taken between the two calls.
                    Ok(None) => return Received::Housekeeping,
                    Err(e) => {
                        tracing::warn!(error = %e, "could not store a file chunk");
                        return Received::Housekeeping;
                    }
                };

                if !progress.is_complete() {
                    return Received::Housekeeping;
                }

                return match self.store.take_file(&file.transfer) {
                    Ok(Some((done, bytes))) => {
                        tracing::info!(from = %done.from, name = %done.name, "a file arrived");
                        Received::File {
                            conversation: done.conversation,
                            transfer: done.transfer,
                            name: done.name,
                            bytes,
                        }
                    }
                    // Complete a moment ago and gone now: two copies of the last
                    // chunk raced, and the other one took it.
                    Ok(None) => Received::Housekeeping,
                    // Hash or length mismatch. `take_file` has already discarded
                    // it; nothing reaches the disk.
                    Err(e) => {
                        tracing::warn!(from = %progress.from, error = %e, "discarded a file");
                        Received::Housekeeping
                    }
                };
            }
        };

        // Not `unwrap_or(0)`: seq is the storage key, so falling back to a
        // constant on error would overwrite whatever already sits at that key.
        let Ok(seq) = self.store.next_message_seq() else {
            tracing::error!("could not allocate a message sequence; dropping");
            return Received::Housekeeping;
        };

        let message_id = opened.content.id;
        let stored = vega_core::StoredMessage {
            seq,
            conversation,
            from_account: opened.from_account,
            from_device: opened.from_device,
            outgoing,
            received_at: now,
            content: opened.content,
        };

        match self.store.append_message(&stored) {
            // Already had it — a replay, or the same message over two tiers.
            Ok(false) => Received::Housekeeping,
            Ok(true) => {
                // Tell the sender it arrived, so they can stop retrying and any
                // mailbox still holding a copy can drop it. Only for text: a
                // receipt for a receipt would never terminate.
                if !outgoing {
                    if let Err(e) = self.acknowledge(&opened.from_account, &message_id) {
                        tracing::debug!(error = %e, "could not queue a receipt");
                    }
                }

                // Whoever opened this may have consumed one of our one-time keys.
                if let Err(e) = self.maintain_prekeys() {
                    tracing::warn!(error = %e, "could not replenish one-time keys");
                }
                let _ = self.persist();
                Received::Message(conversation)
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not store a decrypted message");
                Received::Housekeeping
            }
        }
    }

    /// Queue a receipt telling `to` that `message_id` was decrypted.
    ///
    /// Encrypted and fanned out like any other message, so a receipt reveals no
    /// more to the network than the message it acknowledges. No self-copy: my
    /// other devices do not need to know I read something.
    fn acknowledge(&mut self, to: &AccountId, message_id: &[u8; 32]) -> vega_core::Result<()> {
        let contact = self
            .store
            .get_contact(to)?
            .ok_or_else(|| vega_core::Error::UnknownContact(to.short()))?;
        let their_state = self
            .store
            .load_chain(to)?
            .ok_or_else(|| vega_core::Error::UnknownContact(to.short()))?
            .validate()?;

        let now = vega_core::now();
        let pairwise = self
            .identity
            .pairwise_with(&contact.contact_key, &contact.account_id);

        let content = Content::new(
            *to,
            now,
            self.store.next_message_seq()?,
            Body::Receipt {
                message_id: *message_id,
            },
        );
        let receipt_id = content.id;

        let envelopes = fan_out(
            &mut self.identity,
            &mut self.sessions,
            &Recipient {
                account: *to,
                state: &their_state,
                pairwise: &pairwise,
            },
            None,
            &content,
            now,
        )?;

        for (device, envelope) in envelopes {
            self.store.queue(&vega_core::OutboxItem {
                seq: self.store.next_outbox_seq()?,
                to_account: *to,
                to_device: device,
                envelope: envelope.to_bytes()?,
                queued_at: now,
                attempts: 0,
                message_id: receipt_id,
            })?;
        }
        Ok(())
    }

    // ---- plans for the caller to execute --------------------------------

    /// Everything queued, with the peers worth trying. No I/O.
    ///
    /// On the LAN we do not know which connected peer is the recipient, so an
    /// envelope goes to all of them; the sealed layer means only the right
    /// device can open it, and the rest drop it without learning anything. Once
    /// a rendezvous lookup has told us a peer id, delivery is targeted instead.
    pub fn pending_deliveries(&self) -> Vec<Delivery> {
        let Ok(pending) = self.store.pending() else {
            return Vec::new();
        };
        pending
            .into_iter()
            .map(|item| {
                let targets = match self.routes.get(&item.to_account) {
                    Some(peer) => vec![*peer],
                    None => self.connected.iter().copied().collect(),
                };
                Delivery {
                    seq: item.seq,
                    to_account: item.to_account,
                    envelope: item.envelope,
                    targets,
                }
            })
            .collect()
    }

    pub fn mark_delivered(&mut self, seqs: &[u64]) {
        for seq in seqs {
            let _ = self.store.dequeue(*seq);
        }
    }

    /// Rendezvous records to publish: one per contact, each under a key only
    /// that contact can compute.
    pub fn announce_records(
        &self,
        peer_id: &PeerId,
        addrs: &[Multiaddr],
    ) -> Vec<([u8; 32], Vec<u8>)> {
        if addrs.is_empty() {
            return Vec::new();
        }
        let Ok(contacts) = self.store.list_contacts() else {
            return Vec::new();
        };

        let now = vega_core::now();
        let epoch = vega_core::epoch_at(now);
        let me = self.identity.account_id;

        // The chain travels with the record, so a contact who looks us up also
        // picks up whatever devices and one-time keys we published since.
        let record = AddressRecord::new(peer_id, addrs, now).with_chain(self.chain.clone());

        contacts
            .iter()
            .filter_map(|contact| {
                let pairwise = self
                    .identity
                    .pairwise_with(&contact.contact_key, &contact.account_id);
                let sealed =
                    vega_net::seal_record(&pairwise.record_key(&me, epoch), &record).ok()?;
                Some((pairwise.rendezvous_key(&me, epoch), sealed))
            })
            .collect()
    }

    /// Accounts with queued mail and nowhere to send it.
    pub fn stranded_accounts(&self) -> Vec<AccountId> {
        if !self.connected.is_empty() {
            return Vec::new();
        }
        let Ok(pending) = self.store.pending() else {
            return Vec::new();
        };
        let mut out: Vec<AccountId> = pending
            .into_iter()
            .map(|i| i.to_account)
            .filter(|a| !self.routes.contains_key(a))
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Where to look for a contact, and how to open what comes back.
    ///
    /// Their epoch may have rolled over just before ours or just after, so the
    /// neighbouring ones are worth asking for too.
    pub fn lookup_plan(&self, account: &AccountId) -> Vec<LookupPlan> {
        let Ok(Some(contact)) = self.store.get_contact(account) else {
            return Vec::new();
        };
        let pairwise = self
            .identity
            .pairwise_with(&contact.contact_key, &contact.account_id);
        let current = vega_core::epoch_at(vega_core::now());

        [current, current.saturating_sub(1), current + 1]
            .into_iter()
            .map(|epoch| LookupPlan {
                account: *account,
                key: pairwise.rendezvous_key(account, epoch),
                record_key: pairwise.record_key(account, epoch),
            })
            .collect()
    }

    /// Open a record the DHT returned, and remember the route it describes.
    ///
    /// A record that does not decrypt is simply somebody else's — the key space
    /// is shared, so that is expected rather than suspicious.
    pub fn absorb_record(
        &mut self,
        plan: &LookupPlan,
        value: &[u8],
    ) -> Option<(PeerId, Vec<Multiaddr>)> {
        let record = vega_net::open_record(&plan.record_key, value).ok()?;
        if !record.is_fresh(vega_core::now()) {
            return None;
        }
        let peer = record.peer().ok()?;

        // Nobody signs a record, so the chain inside is trusted only as far as
        // `merge` allows: it must extend a chain we already hold.
        if let Some(chain) = &record.chain {
            self.directory().offer_chain(&plan.account, chain);
        }

        self.routes.insert(plan.account, peer);
        Some((peer, record.multiaddrs()))
    }

    /// Write session and identity state back to disk.
    ///
    /// The ratchet advances on every message, so a crash between sending and
    /// saving would leave a session the peer has moved past. Persisting eagerly
    /// costs a write per message and buys not having to re-establish sessions.
    pub fn persist(&self) -> vega_core::Result<()> {
        self.store.save_identity(&self.identity, &self.pickle_key)?;
        self.store.save_sessions(&self.sessions, &self.pickle_key)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vega_core::{bootstrap_account, Sessions, Store};

    const PICKLE: [u8; 32] = [9u8; 32];

    /// A runtime on a temporary store. The directory is returned because
    /// dropping it deletes the database out from under the runtime.
    fn runtime(label: &str) -> (Runtime, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("vega.redb"), PICKLE).unwrap();
        let (identity, chain) = bootstrap_account(label).unwrap();
        store.save_identity(&identity, &PICKLE).unwrap();
        store.save_chain(&identity.account_id, &chain).unwrap();
        (
            Runtime::new(identity, chain, Sessions::new(), store, PICKLE),
            dir,
        )
    }

    /// The invite exchange, both directions, as the UI would do it.
    fn introduce(a: &mut Runtime, b: &mut Runtime) {
        let from_a = a.my_invite("alice").unwrap();
        let from_b = b.my_invite("bob").unwrap();
        b.add_contact(&from_a).unwrap();
        a.add_contact(&from_b).unwrap();
    }

    fn payload(len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| i.wrapping_mul(2_654_435_761).to_le_bytes()[1])
            .collect()
    }

    /// Everything the sender queued, in the order the outbox holds it.
    fn queued(rt: &Runtime) -> Vec<Vec<u8>> {
        let mut items = rt.store.pending().unwrap();
        items.sort_by_key(|i| i.seq);
        items.into_iter().map(|i| i.envelope).collect()
    }

    #[test]
    fn a_file_crosses_between_two_runtimes_and_arrives_intact() {
        let (mut alice, _a) = runtime("alice-laptop");
        let (mut bob, _b) = runtime("bob-laptop");
        introduce(&mut alice, &mut bob);

        // Three chunks: two full and a short one, so the tail is exercised.
        let data = payload(vega_core::FILE_CHUNK_BYTES * 2 + 517);
        // The name is hostile on purpose — this is the path a peer controls.
        alice
            .send_file(&bob.identity.account_id, "../../.ssh/holiday 🏖️.jpg", &data)
            .unwrap();

        let envelopes = queued(&alice);
        assert_eq!(envelopes.len(), 4, "one announcement and three chunks");

        let mut arrived = None;
        for envelope in &envelopes {
            if let Received::File {
                conversation,
                name,
                bytes,
                ..
            } = bob.receive(envelope)
            {
                assert!(arrived.is_none(), "a file must complete exactly once");
                arrived = Some((conversation, name, bytes));
            }
        }

        let (conversation, name, bytes) = arrived.expect("the file completed");
        assert_eq!(bytes, data, "the file bob assembled is the file alice sent");
        assert_eq!(
            name, "holiday 🏖️.jpg",
            "the traversal is stripped and the rest of the name kept"
        );
        assert_eq!(
            conversation, alice.identity.account_id,
            "filed under the sender, not under what the sender claimed"
        );

        // And it shows in the thread as one file message.
        let thread = bob
            .store
            .conversation(&alice.identity.account_id, 10)
            .unwrap();
        assert_eq!(thread.len(), 1);
        let manifest = thread[0].content.file().expect("a file message");
        assert_eq!(manifest.size, data.len() as u64);
        assert!(!thread[0].outgoing);
    }

    /// The reason every chunk carries the manifest. Nothing orders these
    /// envelopes: a retry, or a mailbox handing back what it held, can deliver
    /// the last chunk first and the announcement last.
    #[test]
    fn a_file_arrives_even_when_the_chunks_beat_the_announcement() {
        let (mut alice, _a) = runtime("alice-laptop");
        let (mut bob, _b) = runtime("bob-laptop");
        introduce(&mut alice, &mut bob);

        let data = payload(vega_core::FILE_CHUNK_BYTES + 40);
        alice
            .send_file(&bob.identity.account_id, "reversed.bin", &data)
            .unwrap();

        let mut envelopes = queued(&alice);
        envelopes.reverse();

        let mut arrived = None;
        for envelope in &envelopes {
            if let Received::File { bytes, .. } = bob.receive(envelope) {
                assert!(arrived.is_none(), "a file must complete exactly once");
                arrived = Some(bytes);
            }
        }
        assert_eq!(arrived.expect("the file completed"), data);

        // The announcement arrived last, after the file was already whole. It
        // must still produce the message — and must not reopen the transfer and
        // leave it looking like it is still coming in.
        let thread = bob
            .store
            .conversation(&alice.identity.account_id, 10)
            .unwrap();
        assert_eq!(thread.len(), 1);
        let manifest = thread[0].content.file().unwrap();
        let state = bob.store.get_transfer(&manifest.transfer).unwrap().unwrap();
        assert!(state.done, "the transfer is finished, not restarted");
    }

    #[test]
    fn a_replayed_chunk_does_not_produce_the_file_twice() {
        let (mut alice, _a) = runtime("alice-laptop");
        let (mut bob, _b) = runtime("bob-laptop");
        introduce(&mut alice, &mut bob);

        let data = payload(1024);
        alice
            .send_file(&bob.identity.account_id, "once.txt", &data)
            .unwrap();

        let envelopes = queued(&alice);
        let mut completions = 0;
        // Everything twice, which is what the same envelope arriving over two
        // tiers looks like.
        for envelope in envelopes.iter().chain(envelopes.iter()) {
            if let Received::File { .. } = bob.receive(envelope) {
                completions += 1;
            }
        }
        assert_eq!(completions, 1);
    }

    #[test]
    fn a_file_past_the_limit_is_refused_before_anything_is_queued() {
        let (mut alice, _a) = runtime("alice-laptop");
        let (mut bob, _b) = runtime("bob-laptop");
        introduce(&mut alice, &mut bob);

        let too_big = vec![0u8; usize::try_from(vega_core::MAX_FILE_BYTES).unwrap() + 1];
        let err = alice
            .send_file(&bob.identity.account_id, "huge.bin", &too_big)
            .unwrap_err();
        assert!(
            err.to_string().contains("at most"),
            "the message should say what the limit is, got: {err}"
        );

        assert!(
            alice
                .send_file(&bob.identity.account_id, "empty", &[])
                .is_err(),
            "an empty file has nothing to send"
        );

        assert!(
            queued(&alice).is_empty(),
            "a refused file must leave nothing in the outbox"
        );
        assert!(
            alice
                .store
                .conversation(&bob.identity.account_id, 10)
                .unwrap()
                .is_empty(),
            "and nothing in the thread"
        );
    }
}
