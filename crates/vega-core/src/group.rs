//! Groups.
//!
//! A group is a name, a member list, and an epoch. It is not a key: messages to
//! a group are ordinary sealed messages, one per member device, over the same
//! Olm ratchet a one-to-one conversation uses. That costs bandwidth linearly
//! and buys the properties that come with it — forward secrecy and
//! post-compromise security per member, and no new key material to distribute,
//! rotate, or lose.
//!
//! ## Who may change a group
//!
//! **The creator, and nobody else.** Anyone may leave; only the creator may
//! add, remove or rename. That is a real limitation and it is deliberate: with
//! no server there is nothing to serialise two admins' concurrent edits, and
//! the alternative to one writer is inventing a consensus protocol or accepting
//! that two people can produce two different member lists that both look valid.
//!
//! ## How a change travels
//!
//! Every [`GroupOp`] carries the **whole resulting state**, not a delta. So an
//! op is idempotent, order-tolerant, and self-healing: a member who was offline
//! for three changes and receives only the fourth ends up with exactly the right
//! member list. The epoch decides what is newer; anything at or below what we
//! hold is ignored.
//!
//! ## What this does not protect against
//!
//! The creator is trusted to describe the group honestly. Nothing here stops
//! them telling Alice the group is {Alice, Bob} while telling Bob it is {Bob,
//! Carol} — there is no shared transcript to compare against, and building one
//! without a server is the hard part of group messaging rather than a detail
//! left out. What it does stop is a *non-creator* rewriting membership, which is
//! the attack that needs no cooperation from anyone.

use crate::error::{Error, Result};
use crate::identity::{id_type, AccountId};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};

/// The most members a group may hold.
///
/// Every message is sent once per member *device*, so this is the fan-out
/// amplification bound: the same reason the sigchain caps devices at 32. A
/// hostile creator who could name ten thousand members would have every member
/// generating ten thousand envelopes per message.
pub const MAX_GROUP_MEMBERS: usize = 32;

/// Longest group name, in bytes. Long enough for any real name, short enough
/// that it cannot be used to bloat every op that carries it.
pub const MAX_GROUP_NAME_BYTES: usize = 128;

/// How far ahead of what we hold an op's epoch may be.
///
/// An epoch counts membership changes, so a real group will not pass a few
/// thousand in a lifetime and this leaves room for a member who was away for a
/// very long time. What it rules out is a creator sending `u64::MAX`: the next
/// op anyone built from that state would compute `epoch + 1` and wrap — leaving
/// a group whose members can no longer leave it, because every op they produce
/// looks stale. The bound keeps the increment far from the edge, and
/// `saturating_add` below means arithmetic cannot wrap even if this ever
/// changes.
pub const MAX_EPOCH_JUMP: u64 = 1_000_000;

/// The most groups one account may create on this device.
///
/// Nothing about a group needs the recipient's agreement — being added *is* the
/// notification — which means a contact can mint them, and each one is a stored
/// row with a name and up to [`MAX_GROUP_MEMBERS`] members in it. Every other
/// unbounded thing a contact controls is capped (devices, chain length, members
/// per group); this is the same reasoning applied to the count itself. Reached
/// only by somebody doing it on purpose.
pub const MAX_GROUPS_PER_CREATOR: usize = 64;

id_type!(
    GroupId,
    "Stable identifier for a group: 32 random bytes, chosen once by its creator."
);

impl GroupId {
    /// A fresh id.
    ///
    /// Random rather than derived from the member list, so that adding somebody
    /// does not produce a different group, and two groups with the same people
    /// in them stay distinct.
    pub fn random() -> Self {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        Self::from_raw(bytes)
    }
}

/// What an op did, for the line the UI shows. Never used to decide anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GroupChange {
    Created,
    Renamed,
    Added { who: AccountId },
    Removed { who: AccountId },
    Left { who: AccountId },
}

/// One membership change, carrying the whole state it results in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupOp {
    pub group: GroupId,
    /// Increments with every accepted change. Decides what is newer.
    pub epoch: u64,
    pub name: String,
    /// Who owns this group. Fixed at creation and checked on every op after it.
    pub creator: AccountId,
    /// The full member list this op results in, creator included.
    pub members: Vec<AccountId>,
    pub change: GroupChange,
}

/// A group as this device understands it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: GroupId,
    pub name: String,
    pub creator: AccountId,
    pub members: Vec<AccountId>,
    pub epoch: u64,
    pub created_at: u64,
    /// Set when we are no longer in `members`. The history stays readable; the
    /// group simply stops accepting anything new.
    pub departed: bool,
    /// Highest message sequence seen when this thread was last on screen.
    ///
    /// Local, like a contact's — it never leaves this device, and it is on the
    /// group rather than in a table of its own because a group with no unread
    /// marker is a group that badges itself forever.
    #[serde(default)]
    pub read_seq: u64,
}

impl Group {
    /// Start a group. `members` need not include the creator; it is added.
    ///
    /// Returns the group and the op that describes it, which the caller sends
    /// to every member — including, harmlessly, to their own other devices.
    pub fn create(
        name: &str,
        creator: AccountId,
        members: &[AccountId],
        now: u64,
    ) -> Result<(Self, GroupOp)> {
        let name = check_name(name)?;

        let mut all = vec![creator];
        for m in members {
            if !all.contains(m) {
                all.push(*m);
            }
        }
        check_members(&all)?;

        let group = Self {
            id: GroupId::random(),
            name: name.clone(),
            creator,
            members: all.clone(),
            epoch: 1,
            created_at: now,
            departed: false,
            read_seq: 0,
        };
        let op = GroupOp {
            group: group.id,
            epoch: 1,
            name,
            creator,
            members: all,
            change: GroupChange::Created,
        };
        Ok((group, op))
    }

    /// Build the group this op describes, for an op about a group we have never
    /// seen.
    ///
    /// The caller must already have established that `from` is a contact — an
    /// op from a stranger has no business creating anything. This checks the
    /// rest: that the sender is the creator they claim to be, and that we are
    /// actually in the group being described.
    pub fn from_op(op: &GroupOp, from: AccountId, me: AccountId, now: u64) -> Result<Self> {
        if from != op.creator {
            return Err(Error::BadSignature(
                "only a group's creator may introduce it",
            ));
        }
        if !op.members.contains(&me) {
            return Err(Error::Wire("a group we are not in".into()));
        }
        check_name(&op.name)?;
        check_members(&op.members)?;
        check_epoch(0, op.epoch)?;
        if !op.members.contains(&op.creator) {
            return Err(Error::Wire("a group whose creator is not in it".into()));
        }

        Ok(Self {
            id: op.group,
            name: op.name.clone(),
            creator: op.creator,
            members: op.members.clone(),
            epoch: op.epoch,
            created_at: now,
            departed: false,
            read_seq: 0,
        })
    }

    pub fn is_member(&self, account: &AccountId) -> bool {
        self.members.contains(account)
    }

    /// Everyone but me — the people a message actually goes to.
    pub fn others(&self, me: &AccountId) -> Vec<AccountId> {
        self.members.iter().copied().filter(|m| m != me).collect()
    }

    /// Apply an op that arrived from `from`.
    ///
    /// `Ok(false)` means the op was valid but stale or already applied, which is
    /// ordinary: ops are broadcast to every member and reach us over more than
    /// one tier. `Err` means it was not the sender's to make.
    pub fn apply(&mut self, op: &GroupOp, from: AccountId, me: AccountId) -> Result<bool> {
        if op.group != self.id {
            return Err(Error::Wire("an op for another group".into()));
        }
        if op.creator != self.creator {
            return Err(Error::BadSignature("a group's creator cannot change"));
        }
        // Not an error: the same op arriving twice, or an older one overtaking a
        // newer one, is what a network with no ordering does all day.
        if op.epoch <= self.epoch {
            return Ok(false);
        }
        check_epoch(self.epoch, op.epoch)?;

        check_name(&op.name)?;
        check_members(&op.members)?;

        match &op.change {
            // Leaving is the one change that is not the creator's to make, and
            // the only one anybody may make about themselves.
            GroupChange::Left { who } => {
                if from != *who {
                    return Err(Error::BadSignature("only you may leave on your own behalf"));
                }
                if *who == self.creator {
                    return Err(Error::Wire(
                        "a group's creator cannot leave it; there would be nobody left to change it"
                            .into(),
                    ));
                }
                if !self.members.contains(who) {
                    return Ok(false);
                }
                // The departing member describes the result, so check it says
                // what it should rather than taking their word for the roster.
                let expected: Vec<AccountId> =
                    self.members.iter().copied().filter(|m| m != who).collect();
                if op.members != expected {
                    return Err(Error::Wire(
                        "a leave that rewrote the member list as well".into(),
                    ));
                }
            }

            // Everything else is the creator's alone.
            _ => {
                if from != self.creator {
                    return Err(Error::BadSignature(
                        "only a group's creator may change its membership",
                    ));
                }
                if !op.members.contains(&self.creator) {
                    return Err(Error::Wire("a group whose creator is not in it".into()));
                }
            }
        }

        self.name = op.name.clone();
        self.members = op.members.clone();
        self.epoch = op.epoch;
        // Being dropped from the roster is how removal reaches us; there is no
        // separate message for it, and the op that does it is the last one we
        // will be sent.
        self.departed = !self.members.contains(&me);
        Ok(true)
    }

    /// The op for adding somebody, and the state it produces.
    pub fn add(&self, who: AccountId) -> Result<GroupOp> {
        if self.members.contains(&who) {
            return Err(Error::Wire("already in this group".into()));
        }
        let mut members = self.members.clone();
        members.push(who);
        check_members(&members)?;
        Ok(self.op(members, self.name.clone(), GroupChange::Added { who }))
    }

    /// The op for removing somebody.
    pub fn remove(&self, who: AccountId) -> Result<GroupOp> {
        if who == self.creator {
            return Err(Error::Wire("a group's creator cannot be removed".into()));
        }
        if !self.members.contains(&who) {
            return Err(Error::Wire("not in this group".into()));
        }
        let members = self.members.iter().copied().filter(|m| *m != who).collect();
        Ok(self.op(members, self.name.clone(), GroupChange::Removed { who }))
    }

    /// The op for renaming.
    pub fn rename(&self, name: &str) -> Result<GroupOp> {
        let name = check_name(name)?;
        Ok(self.op(self.members.clone(), name, GroupChange::Renamed))
    }

    /// The op for leaving. Not available to the creator.
    pub fn leave(&self, me: AccountId) -> Result<GroupOp> {
        if me == self.creator {
            return Err(Error::Wire(
                "you created this group, so you cannot leave it — remove the others instead".into(),
            ));
        }
        if !self.members.contains(&me) {
            return Err(Error::Wire("not in this group".into()));
        }
        let members = self.members.iter().copied().filter(|m| *m != me).collect();
        Ok(self.op(members, self.name.clone(), GroupChange::Left { who: me }))
    }

    fn op(&self, members: Vec<AccountId>, name: String, change: GroupChange) -> GroupOp {
        GroupOp {
            group: self.id,
            epoch: self.epoch.saturating_add(1),
            name,
            creator: self.creator,
            members,
            change,
        }
    }
}

/// Refuse an epoch that is implausibly far ahead of what we hold.
fn check_epoch(have: u64, offered: u64) -> Result<()> {
    if offered > have.saturating_add(MAX_EPOCH_JUMP) {
        return Err(Error::Wire("that group's epoch is implausible".into()));
    }
    Ok(())
}

fn check_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(Error::Wire("a group needs a name".into()));
    }
    if trimmed.len() > MAX_GROUP_NAME_BYTES {
        return Err(Error::Wire("that group name is too long".into()));
    }
    // Control characters in a name that is rendered in a list, and carried in
    // every op, are never anything but trouble.
    Ok(trimmed.chars().filter(|c| !c.is_control()).collect())
}

fn check_members(members: &[AccountId]) -> Result<()> {
    if members.is_empty() {
        return Err(Error::Wire("a group needs members".into()));
    }
    if members.len() > MAX_GROUP_MEMBERS {
        return Err(Error::Wire(format!(
            "a group holds at most {MAX_GROUP_MEMBERS} members"
        )));
    }
    let mut seen = members.to_vec();
    seen.sort_unstable();
    seen.dedup();
    if seen.len() != members.len() {
        return Err(Error::Wire("that member list repeats somebody".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(n: u8) -> AccountId {
        AccountId::from_raw([n; 32])
    }

    const NOW: u64 = 1_755_000_000;

    fn group_of(creator: AccountId, others: &[AccountId]) -> Group {
        Group::create("Trip", creator, others, NOW).unwrap().0
    }

    #[test]
    fn creating_puts_the_creator_in_it() {
        let (g, op) = Group::create("Trip", account(1), &[account(2)], NOW).unwrap();
        assert!(g.is_member(&account(1)));
        assert!(g.is_member(&account(2)));
        assert_eq!(g.epoch, 1);
        assert_eq!(op.members, g.members);
    }

    #[test]
    fn the_creator_is_not_duplicated_when_named_twice() {
        let (g, _) = Group::create("Trip", account(1), &[account(1), account(2)], NOW).unwrap();
        assert_eq!(g.members.len(), 2);
    }

    #[test]
    fn a_member_cannot_change_the_membership() {
        let mut g = group_of(account(1), &[account(2), account(3)]);
        let forged = GroupOp {
            group: g.id,
            epoch: 2,
            name: g.name.clone(),
            creator: g.creator,
            members: vec![account(1), account(2)],
            change: GroupChange::Removed { who: account(3) },
        };
        // Member 2 is in the group, and that is not enough.
        assert!(g.apply(&forged, account(2), account(1)).is_err());
        assert_eq!(g.members.len(), 3);
    }

    #[test]
    fn the_creator_may() {
        let mut g = group_of(account(1), &[account(2), account(3)]);
        let op = g.remove(account(3)).unwrap();
        assert!(g.apply(&op, account(1), account(1)).unwrap());
        assert_eq!(g.members, vec![account(1), account(2)]);
        assert_eq!(g.epoch, 2);
    }

    #[test]
    fn leaving_is_only_ever_about_yourself() {
        let mut g = group_of(account(1), &[account(2), account(3)]);
        let op = GroupOp {
            group: g.id,
            epoch: 2,
            name: g.name.clone(),
            creator: g.creator,
            members: vec![account(1), account(2)],
            change: GroupChange::Left { who: account(3) },
        };
        // Two claiming that three left.
        assert!(g.apply(&op, account(2), account(1)).is_err());
        // Three saying so themselves.
        assert!(g.apply(&op, account(3), account(1)).unwrap());
        assert_eq!(g.members, vec![account(1), account(2)]);
    }

    #[test]
    fn a_leave_may_not_rewrite_the_rest_of_the_roster() {
        let mut g = group_of(account(1), &[account(2), account(3)]);
        let op = GroupOp {
            group: g.id,
            epoch: 2,
            name: g.name.clone(),
            creator: g.creator,
            // Leaving *and* dropping account 2 on the way out.
            members: vec![account(1)],
            change: GroupChange::Left { who: account(3) },
        };
        assert!(g.apply(&op, account(3), account(1)).is_err());
        assert_eq!(g.members.len(), 3);
    }

    #[test]
    fn the_creator_cannot_leave() {
        let g = group_of(account(1), &[account(2)]);
        assert!(g.leave(account(1)).is_err());
    }

    #[test]
    fn a_stale_op_changes_nothing() {
        let mut g = group_of(account(1), &[account(2), account(3)]);
        let op = g.remove(account(3)).unwrap();
        assert!(g.apply(&op, account(1), account(1)).unwrap());
        // The same op again, and an older one behind it.
        assert!(!g.apply(&op, account(1), account(1)).unwrap());
        let stale = GroupOp { epoch: 1, ..op };
        assert!(!g.apply(&stale, account(1), account(1)).unwrap());
        assert_eq!(g.members.len(), 2);
    }

    #[test]
    fn a_missed_epoch_is_not_a_gap_to_recover_from() {
        // Everything an op needs is in the op, so the member who missed epochs
        // 2 and 3 lands on the same state as everyone else.
        let mut behind = group_of(account(1), &[account(2), account(3)]);
        let id = behind.id;
        let op = GroupOp {
            group: id,
            epoch: 4,
            name: "Trip".into(),
            creator: account(1),
            members: vec![account(1), account(2), account(4), account(5)],
            change: GroupChange::Added { who: account(5) },
        };
        assert!(behind.apply(&op, account(1), account(1)).unwrap());
        assert_eq!(behind.members.len(), 4);
        assert_eq!(behind.epoch, 4);
    }

    #[test]
    fn being_dropped_from_the_roster_is_how_removal_arrives() {
        let mut g = group_of(account(1), &[account(2)]);
        let op = GroupOp {
            group: g.id,
            epoch: 2,
            name: g.name.clone(),
            creator: account(1),
            members: vec![account(1)],
            change: GroupChange::Removed { who: account(2) },
        };
        // Applied on account 2's device: we are the one removed.
        assert!(g.apply(&op, account(1), account(2)).unwrap());
        assert!(g.departed);
    }

    #[test]
    fn the_creator_cannot_be_swapped_out() {
        let mut g = group_of(account(1), &[account(2)]);
        let op = GroupOp {
            group: g.id,
            epoch: 2,
            name: g.name.clone(),
            creator: account(2),
            members: vec![account(2)],
            change: GroupChange::Removed { who: account(1) },
        };
        assert!(g.apply(&op, account(2), account(1)).is_err());
    }

    #[test]
    fn an_introduction_has_to_include_us() {
        let (_, op) = Group::create("Trip", account(1), &[account(2)], NOW).unwrap();
        assert!(Group::from_op(&op, account(1), account(2), NOW).is_ok());
        // Account 3 was never named in it.
        assert!(Group::from_op(&op, account(1), account(3), NOW).is_err());
        // And a contact who is not the creator cannot introduce it either.
        assert!(Group::from_op(&op, account(2), account(2), NOW).is_err());
    }

    #[test]
    fn the_member_cap_holds() {
        let over = u8::try_from(MAX_GROUP_MEMBERS).unwrap() + 1;
        let many: Vec<AccountId> = (0..over).map(account).collect();
        assert!(Group::create("Trip", account(200), &many, NOW).is_err());
    }

    #[test]
    fn names_are_bounded_and_stripped() {
        assert!(Group::create("   ", account(1), &[], NOW).is_err());
        assert!(
            Group::create(&"x".repeat(MAX_GROUP_NAME_BYTES + 1), account(1), &[], NOW).is_err()
        );
        let (g, _) = Group::create(" Trip\u{7} ", account(1), &[], NOW).unwrap();
        assert_eq!(g.name, "Trip");
    }

    #[test]
    fn an_implausible_epoch_is_refused() {
        // A creator who sets the epoch near the ceiling would otherwise leave a
        // group whose members can never leave: every op they build computes
        // epoch + 1, which wraps to something that looks stale to everyone.
        let mut g = group_of(account(1), &[account(2)]);
        let op = GroupOp {
            group: g.id,
            epoch: u64::MAX,
            name: g.name.clone(),
            creator: account(1),
            members: vec![account(1), account(2)],
            change: GroupChange::Renamed,
        };
        assert!(g.apply(&op, account(1), account(1)).is_err());
        assert_eq!(g.epoch, 1);

        // And the same on an introduction, where there is no prior state.
        assert!(Group::from_op(&op, account(1), account(2), NOW).is_err());
    }

    #[test]
    fn the_epoch_increment_cannot_wrap() {
        // Belt and braces: even if a state with a huge epoch were reached some
        // other way, building an op from it must not wrap around to zero.
        let mut g = group_of(account(1), &[account(2)]);
        g.epoch = u64::MAX;
        assert_eq!(g.rename("Trip 2").unwrap().epoch, u64::MAX);
        assert_eq!(g.leave(account(2)).unwrap().epoch, u64::MAX);
    }
}
