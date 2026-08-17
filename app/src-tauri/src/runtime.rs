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
    envelope::Body, fan_out, fan_out_to, AccountId, ChainState, Content, Conversation, Envelope,
    Group, GroupId, GroupOp, Identity, Recipient, Sessions, Sigchain, Store,
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
    /// A message worth showing. Carries the conversation it belongs to — a
    /// contact for a one-to-one message, a group for a group one.
    Message(Conversation),
    /// The last chunk of a file arrived and the whole thing checked out. The
    /// caller writes it to disk — this module deals in bytes, not paths.
    File {
        /// Files are one-to-one only, so this is always a contact.
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
            // Nothing has arrived from somebody who was added a moment ago, and
            // a conversation that opened already badged would be a lie.
            read_seq: 0,
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

    // ---- groups ------------------------------------------------------------

    /// Everything needed to address one member, skipping the ones we cannot.
    ///
    /// A group may name somebody we have never exchanged an invite with — the
    /// creator knows them, we do not. There is no way to message such a person
    /// and no server to ask, so they are reported and skipped rather than
    /// failing the send for everybody else. The UI shows who they are.
    fn group_recipients(
        &self,
        members: &[AccountId],
    ) -> (
        Vec<(AccountId, ChainState, vega_core::Pairwise)>,
        Vec<AccountId>,
    ) {
        let mut reachable = Vec::new();
        let mut unreachable = Vec::new();

        for member in members {
            if *member == self.identity.account_id {
                // My own account: my other devices, addressed with the
                // self-pairwise secret the same way a self-copy is.
                match self.chain.validate() {
                    Ok(state) => {
                        let pairwise = self
                            .identity
                            .pairwise_with(&state.contact, &self.identity.account_id);
                        reachable.push((*member, state, pairwise));
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "own chain does not validate");
                        unreachable.push(*member);
                    }
                }
                continue;
            }
            match self.address(member) {
                Ok((_, state, pairwise)) => reachable.push((*member, state, pairwise)),
                Err(_) => unreachable.push(*member),
            }
        }
        (reachable, unreachable)
    }

    /// Encrypt one body to every member of a group and queue it.
    ///
    /// Returns the members it could not be addressed to.
    fn send_to_group(
        &mut self,
        members: &[AccountId],
        conversation: Conversation,
        body: Body,
        store_it: bool,
    ) -> vega_core::Result<Vec<AccountId>> {
        let now = vega_core::now();
        let (reachable, mut unreachable) = self.group_recipients(members);

        // `conversation` on the content is the addressing hint a one-to-one
        // message uses; for a group the id inside the body is what decides the
        // thread, and every recipient checks it against their own membership.
        let content = Content::new(
            self.identity.account_id,
            now,
            self.store.next_message_seq()?,
            body,
        );
        let message_id = content.id;

        let recipients: Vec<Recipient<'_>> = reachable
            .iter()
            .map(|(account, state, pairwise)| Recipient {
                account: *account,
                state,
                pairwise,
            })
            .collect();

        let (envelopes, failed) = fan_out_to(
            &mut self.identity,
            &mut self.sessions,
            &recipients,
            &content,
            now,
        );
        for (account, e) in failed {
            tracing::warn!(member = %account, error = %e, "could not address a group member");
            if !unreachable.contains(&account) {
                unreachable.push(account);
            }
        }

        // Stored before anything touches the network, exactly as a one-to-one
        // message is: a message is not lost because delivery failed.
        if store_it {
            self.store.append_message(&vega_core::StoredMessage {
                seq: self.store.next_message_seq()?,
                conversation,
                from_account: self.identity.account_id,
                from_device: self.identity.device_id,
                outgoing: true,
                received_at: now,
                content,
            })?;
        }

        for (account, device, envelope) in envelopes {
            self.store.queue(&vega_core::OutboxItem {
                seq: self.store.next_outbox_seq()?,
                to_account: account,
                to_device: device,
                envelope: envelope.to_bytes()?,
                queued_at: now,
                attempts: 0,
                message_id,
            })?;
        }

        self.persist()?;
        Ok(unreachable)
    }

    /// Start a group and tell everyone in it.
    pub fn create_group(
        &mut self,
        name: &str,
        members: &[AccountId],
    ) -> vega_core::Result<(GroupId, Vec<AccountId>)> {
        let now = vega_core::now();
        let (group, op) = Group::create(name, self.identity.account_id, members, now)?;
        self.store.put_group(&group)?;

        let unreachable = self.send_to_group(
            &group.members,
            Conversation::Group(group.id),
            Body::GroupOp { op },
            false,
        )?;
        Ok((group.id, unreachable))
    }

    /// Apply one of my own membership changes and broadcast it.
    ///
    /// The op goes to the members *after* the change and to anyone dropped by
    /// it, because being removed is something a person should be told rather
    /// than left to infer from silence.
    fn apply_and_broadcast(
        &mut self,
        mut group: Group,
        op: GroupOp,
    ) -> vega_core::Result<Vec<AccountId>> {
        let me = self.identity.account_id;
        let before = group.members.clone();
        group.apply(&op, me, me)?;
        self.store.put_group(&group)?;

        let mut audience = group.members.clone();
        for gone in before {
            if !audience.contains(&gone) {
                audience.push(gone);
            }
        }
        self.send_to_group(
            &audience,
            Conversation::Group(group.id),
            Body::GroupOp { op },
            false,
        )
    }

    pub fn add_to_group(
        &mut self,
        group: &GroupId,
        who: AccountId,
    ) -> vega_core::Result<Vec<AccountId>> {
        let group = self.group(group)?;
        let op = group.add(who)?;
        self.apply_and_broadcast(group, op)
    }

    pub fn remove_from_group(
        &mut self,
        group: &GroupId,
        who: AccountId,
    ) -> vega_core::Result<Vec<AccountId>> {
        let group = self.group(group)?;
        let op = group.remove(who)?;
        self.apply_and_broadcast(group, op)
    }

    pub fn rename_group(
        &mut self,
        group: &GroupId,
        name: &str,
    ) -> vega_core::Result<Vec<AccountId>> {
        let group = self.group(group)?;
        let op = group.rename(name)?;
        self.apply_and_broadcast(group, op)
    }

    pub fn leave_group(&mut self, group: &GroupId) -> vega_core::Result<Vec<AccountId>> {
        let group = self.group(group)?;
        let op = group.leave(self.identity.account_id)?;
        self.apply_and_broadcast(group, op)
    }

    /// Send a message to a group. Returns the members it could not reach.
    pub fn send_group_text(
        &mut self,
        group: &GroupId,
        text: &str,
    ) -> vega_core::Result<Vec<AccountId>> {
        let group = self.group(group)?;
        if group.departed {
            return Err(vega_core::Error::Wire(
                "you are no longer in this group".into(),
            ));
        }
        let members = group.others(&self.identity.account_id);
        self.send_to_group(
            &members,
            Conversation::Group(group.id),
            Body::GroupText {
                group: group.id,
                text: text.into(),
            },
            true,
        )
    }

    /// Take in a membership change somebody else made.
    ///
    /// The sender is already authenticated — `receive` refuses anything from an
    /// account whose signed chain does not vouch for the sending device, and an
    /// account we have no chain for is a stranger whose messages never get this
    /// far. So "is this person allowed to say this?" is all that is left, and
    /// that lives in `vega_core::group`.
    fn absorb_group_op(&mut self, op: &GroupOp, from: AccountId, now: u64) {
        let me = self.identity.account_id;

        let existing = match self.store.get_group(&op.group) {
            Ok(found) => found,
            Err(e) => {
                tracing::warn!(error = %e, "could not read a group");
                return;
            }
        };

        let updated = match existing {
            Some(mut group) => match group.apply(op, from, me) {
                Ok(true) => group,
                // Stale or already applied: ordinary on a network that delivers
                // the same op over two tiers.
                Ok(false) => return,
                Err(e) => {
                    tracing::warn!(
                        from = %from,
                        group = %op.group.short(),
                        error = %e,
                        "refused a group change"
                    );
                    return;
                }
            },
            // A group we have never heard of. Being added to one is how this
            // normally happens, and it is the sender's own contact status that
            // makes it safe to accept at all — but a contact can mint these,
            // and nothing about a group needs our agreement, so the count one
            // account may create here is bounded like everything else they
            // control.
            None if self.groups_created_by(&from) >= vega_core::MAX_GROUPS_PER_CREATOR => {
                tracing::warn!(
                    from = %from,
                    "refused a new group: that contact has created too many"
                );
                return;
            }
            None => match Group::from_op(op, from, me, now) {
                Ok(group) => {
                    tracing::info!(
                        group = %group.id.short(),
                        name = %group.name,
                        from = %from,
                        "added to a group"
                    );
                    group
                }
                Err(e) => {
                    tracing::warn!(from = %from, error = %e, "refused a group introduction");
                    return;
                }
            },
        };

        if let Err(e) = self.store.put_group(&updated) {
            tracing::warn!(error = %e, "could not store a group");
        }
    }

    /// How many groups this account has already created on this device.
    fn groups_created_by(&self, creator: &AccountId) -> usize {
        self.store
            .list_groups()
            .map(|groups| groups.iter().filter(|g| g.creator == *creator).count())
            // Unreadable is not "none": treating a failed read as zero would
            // turn a storage fault into an unbounded write path.
            .unwrap_or(usize::MAX)
    }

    fn group(&self, id: &GroupId) -> vega_core::Result<Group> {
        self.store
            .get_group(id)?
            .ok_or_else(|| vega_core::Error::Wire(format!("no such group: {}", id.short())))
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
            conversation: Conversation::Direct(*to),
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
            conversation: Conversation::Direct(*to),
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
            Body::Text { .. } => (Conversation::Direct(opened.from_account), false),

            // A group id *is* sender-chosen, so it gets the same treatment the
            // conversation field gets: it is only believed once our own copy of
            // the group says this sender is in it. Without that check any
            // contact could drop messages into any thread whose id they learned.
            Body::GroupText { group, .. } => {
                match self.store.get_group(group) {
                    // Left, or removed. The others may still be talking; it is
                    // no longer a thread we are in, and storing what is said in
                    // it would be both a surprise and somewhere to write
                    // without limit.
                    Ok(Some(state)) if state.departed => {
                        tracing::debug!(
                            group = %group.short(),
                            "a message for a group we have left"
                        );
                        return Received::NotOurs;
                    }
                    Ok(Some(state)) if state.is_member(&opened.from_account) => {}
                    Ok(Some(_)) => {
                        tracing::warn!(
                            from = %opened.from_account,
                            group = %group.short(),
                            "discarded a group message from a non-member"
                        );
                        return Received::NotOurs;
                    }
                    // A group we have never been told about. The op that would
                    // introduce it either has not arrived yet or never will.
                    Ok(None) => {
                        tracing::debug!(
                            from = %opened.from_account,
                            group = %group.short(),
                            "a message for a group we do not have"
                        );
                        return Received::NotOurs;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "could not read a group");
                        return Received::Housekeeping;
                    }
                }
                (
                    Conversation::Group(*group),
                    opened.from_account == self.identity.account_id,
                )
            }

            // Membership. Authorisation lives in `vega_core::group`; what this
            // decides is only whether the op may create a group we do not yet
            // have, which is the one case where there is no prior state to
            // check it against.
            Body::GroupOp { op } => {
                self.absorb_group_op(op, opened.from_account, now);
                let _ = self.persist();
                return Received::Housekeeping;
            }

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
                (Conversation::Direct(*to), true)
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
                (Conversation::Direct(opened.from_account), false)
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
            .conversation(&alice.identity.account_id.into(), 10)
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
            .conversation(&alice.identity.account_id.into(), 10)
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
                .conversation(&bob.identity.account_id.into(), 10)
                .unwrap()
                .is_empty(),
            "and nothing in the thread"
        );
    }

    // ---- groups ---------------------------------------------------------

    /// Three runtimes who have all exchanged invites and all have live sessions.
    ///
    /// The warm-up round is not decoration. Two senders opening a *first*
    /// session with the same third party can pick the same published one-time
    /// key — one chance in however many are published, which is the residual
    /// documented on `Sessions::establish` — and the loser's message never
    /// opens. That is real behaviour worth knowing about, and it is not what any
    /// group test here is about; a suite that fails one run in fifty teaches
    /// people to re-run it rather than to read it. So every pair exchanges one
    /// message up front, and a draw where that does not work is discarded.
    fn trio() -> (
        (Runtime, tempfile::TempDir),
        (Runtime, tempfile::TempDir),
        (Runtime, tempfile::TempDir),
    ) {
        for _ in 0..32 {
            let (mut alice, a) = runtime("alice-laptop");
            let (mut bob, b) = runtime("bob-laptop");
            let (mut carol, c) = runtime("carol-laptop");
            introduce(&mut alice, &mut bob);
            introduce(&mut alice, &mut carol);
            introduce(&mut bob, &mut carol);

            if warm_up(&mut [&mut alice, &mut bob, &mut carol]) {
                return ((alice, a), (bob, b), (carol, c));
            }
        }
        panic!("could not build three runtimes with sessions in every direction");
    }

    /// One message each way between every pair, delivered and cleared.
    ///
    /// Returns false if any of them failed to arrive, which is the prekey
    /// collision above and means this draw of identities should be thrown away.
    fn warm_up(peers: &mut [&mut Runtime]) -> bool {
        for from in 0..peers.len() {
            for to in 0..peers.len() {
                if from == to {
                    continue;
                }
                let target = peers[to].identity.account_id;
                if peers[from].send_text(&target, "hello").is_err() {
                    return false;
                }

                let envelopes = queued(peers[from]);
                let seqs: Vec<u64> = peers[from]
                    .store
                    .pending()
                    .unwrap()
                    .iter()
                    .map(|i| i.seq)
                    .collect();
                peers[from].mark_delivered(&seqs);

                let mut landed = false;
                for envelope in &envelopes {
                    if matches!(peers[to].receive(envelope), Received::Message(_)) {
                        landed = true;
                    }
                }
                if !landed {
                    return false;
                }
                // The receipt the arrival queued is not part of any test.
                let seqs: Vec<u64> = peers[to]
                    .store
                    .pending()
                    .unwrap()
                    .iter()
                    .map(|i| i.seq)
                    .collect();
                peers[to].mark_delivered(&seqs);
            }
        }
        true
    }

    /// Hand everything one runtime has queued to the others, and clear it.
    fn deliver_all(from: &mut Runtime, to: &mut [&mut Runtime]) {
        for envelope in queued(from) {
            for peer in to.iter_mut() {
                peer.receive(&envelope);
            }
        }
        let seqs: Vec<u64> = from
            .store
            .pending()
            .unwrap()
            .iter()
            .map(|i| i.seq)
            .collect();
        from.mark_delivered(&seqs);
    }

    #[test]
    fn a_group_message_reaches_every_member() {
        let ((mut alice, _a), (mut bob, _b), (mut carol, _c)) = trio();

        let (group, unreachable) = alice
            .create_group(
                "Trip",
                &[bob.identity.account_id, carol.identity.account_id],
            )
            .unwrap();
        assert!(unreachable.is_empty(), "everybody is a contact");
        deliver_all(&mut alice, &mut [&mut bob, &mut carol]);

        // Both were told about the group by the op alone.
        assert_eq!(bob.store.get_group(&group).unwrap().unwrap().name, "Trip");
        assert_eq!(
            carol
                .store
                .get_group(&group)
                .unwrap()
                .unwrap()
                .members
                .len(),
            3
        );

        alice.send_group_text(&group, "we leave at six").unwrap();
        deliver_all(&mut alice, &mut [&mut bob, &mut carol]);

        for peer in [&bob, &carol] {
            let thread = peer
                .store
                .conversation(&Conversation::Group(group), 10)
                .unwrap();
            assert_eq!(thread.len(), 1, "one message, filed under the group");
            assert_eq!(thread[0].content.text(), Some("we leave at six"));
            assert!(!thread[0].outgoing);
        }

        // And the sender has their own copy, filed the same way.
        let mine = alice
            .store
            .conversation(&Conversation::Group(group), 10)
            .unwrap();
        assert_eq!(mine.len(), 1);
        assert!(mine[0].outgoing);
    }

    #[test]
    fn a_contact_cannot_post_to_a_group_they_are_not_in() {
        let ((mut alice, _a), (mut bob, _b), (mut carol, _c)) = trio();

        // Alice and Bob only. Carol is a contact of both, and not a member.
        let (group, _) = alice
            .create_group("Trip", &[bob.identity.account_id])
            .unwrap();
        deliver_all(&mut alice, &mut [&mut bob, &mut carol]);
        assert!(
            carol.store.get_group(&group).unwrap().is_none(),
            "an op naming somebody else's group is not theirs to keep"
        );

        // Carol learns the id anyway — it is in every op Alice sent Bob — and
        // writes to it directly.
        let content = Content::new(
            bob.identity.account_id,
            vega_core::now(),
            1,
            Body::GroupText {
                group,
                text: "let me in".into(),
            },
        );
        let (their_contact, state, pairwise) = carol.address(&bob.identity.account_id).unwrap();
        let _ = their_contact;
        let (envelopes, _) = fan_out_to(
            &mut carol.identity,
            &mut carol.sessions,
            &[Recipient {
                account: bob.identity.account_id,
                state: &state,
                pairwise: &pairwise,
            }],
            &content,
            vega_core::now(),
        );
        assert!(!envelopes.is_empty());

        for (_, _, envelope) in envelopes {
            bob.receive(&envelope.to_bytes().unwrap());
        }
        assert!(
            bob.store
                .conversation(&Conversation::Group(group), 10)
                .unwrap()
                .is_empty(),
            "a non-member's message must not land in the thread"
        );
    }

    #[test]
    fn only_the_creator_may_change_the_membership() {
        let ((mut alice, _a), (mut bob, _b), (mut carol, _c)) = trio();

        let (group, _) = alice
            .create_group(
                "Trip",
                &[bob.identity.account_id, carol.identity.account_id],
            )
            .unwrap();
        deliver_all(&mut alice, &mut [&mut bob, &mut carol]);

        // Bob tries to throw Carol out.
        assert!(
            bob.remove_from_group(&group, carol.identity.account_id)
                .is_err(),
            "a member is not an admin"
        );

        // Alice does, and it takes everywhere.
        alice
            .remove_from_group(&group, carol.identity.account_id)
            .unwrap();
        deliver_all(&mut alice, &mut [&mut bob, &mut carol]);

        assert_eq!(
            bob.store.get_group(&group).unwrap().unwrap().members.len(),
            2
        );
        // Carol is told, rather than left to infer it from silence.
        let hers = carol.store.get_group(&group).unwrap().unwrap();
        assert!(hers.departed);
        assert!(carol.send_group_text(&group, "wait").is_err());
    }

    #[test]
    fn leaving_tells_the_others() {
        let ((mut alice, _a), (mut bob, _b), (mut carol, _c)) = trio();

        let (group, _) = alice
            .create_group(
                "Trip",
                &[bob.identity.account_id, carol.identity.account_id],
            )
            .unwrap();
        deliver_all(&mut alice, &mut [&mut bob, &mut carol]);

        bob.leave_group(&group).unwrap();
        deliver_all(&mut bob, &mut [&mut alice, &mut carol]);

        assert!(bob.store.get_group(&group).unwrap().unwrap().departed);
        for peer in [&alice, &carol] {
            let g = peer.store.get_group(&group).unwrap().unwrap();
            assert!(!g.is_member(&bob.identity.account_id));
            assert!(!g.departed, "the others are still in it");
        }
    }

    #[test]
    fn a_group_message_skips_a_member_who_is_not_a_contact() {
        let ((mut alice, _a), (mut bob, _b), (mut carol, _c)) = trio();

        // Alice makes a group, then Carol drops out of Bob's contacts — the
        // shape of "the creator knows them, I do not".
        let (group, _) = alice
            .create_group(
                "Trip",
                &[bob.identity.account_id, carol.identity.account_id],
            )
            .unwrap();
        deliver_all(&mut alice, &mut [&mut bob, &mut carol]);

        let (mut stranger, _s) = runtime("dave-laptop");
        let dave = stranger.identity.account_id;
        // Alice adds Dave, whom Bob has never met.
        let from_dave = stranger.my_invite("dave").unwrap();
        alice.add_contact(&from_dave).unwrap();
        let from_alice = alice.my_invite("alice").unwrap();
        stranger.add_contact(&from_alice).unwrap();

        alice.add_to_group(&group, dave).unwrap();
        deliver_all(&mut alice, &mut [&mut bob, &mut carol, &mut stranger]);

        // Bob can still talk to the group; Dave is reported, not fatal.
        let unreachable = bob.send_group_text(&group, "who is dave").unwrap();
        assert_eq!(unreachable, vec![dave]);
        deliver_all(&mut bob, &mut [&mut alice, &mut carol]);
        assert_eq!(
            alice
                .store
                .conversation(&Conversation::Group(group), 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn a_contact_cannot_fill_the_store_with_groups() {
        let ((mut alice, _a), (mut bob, _b), _c) = trio();

        // Alice mints groups naming Bob, who never agreed to any of them —
        // being added is the whole of the notification.
        for i in 0..vega_core::MAX_GROUPS_PER_CREATOR + 8 {
            alice
                .create_group(&format!("Trip {i}"), &[bob.identity.account_id])
                .unwrap();
            deliver_all(&mut alice, &mut [&mut bob]);
        }

        assert_eq!(
            bob.store.list_groups().unwrap().len(),
            vega_core::MAX_GROUPS_PER_CREATOR,
            "one contact's groups have to be bounded like everything else they control"
        );
    }

    #[test]
    fn a_group_we_left_stops_accepting_messages() {
        let ((mut alice, _a), (mut bob, _b), (mut carol, _c)) = trio();

        let (group, _) = alice
            .create_group(
                "Trip",
                &[bob.identity.account_id, carol.identity.account_id],
            )
            .unwrap();
        deliver_all(&mut alice, &mut [&mut bob, &mut carol]);

        bob.leave_group(&group).unwrap();
        deliver_all(&mut bob, &mut [&mut alice, &mut carol]);

        // Alice has not applied Bob's departure yet from Bob's point of view —
        // she carries on talking to the group as she knew it.
        alice.send_group_text(&group, "still here?").unwrap();
        for envelope in queued(&alice) {
            bob.receive(&envelope);
        }

        assert!(
            bob.store
                .conversation(&Conversation::Group(group), 10)
                .unwrap()
                .is_empty(),
            "a thread we left must not keep filling up"
        );
    }
}
