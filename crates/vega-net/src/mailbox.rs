//! Tier 4: holding mail for a peer who is offline.
//!
//! An open "store this for me" endpoint is a free disk-filling service, so every
//! limit here exists to make abuse boring rather than impossible: a hard cap per
//! tag, a hard cap overall, a maximum lifetime, and eviction of the oldest entry
//! when a tag is full. A mailbox peer cannot tell who sent an envelope, who it
//! is for, or what it says — which is also why it cannot filter on any of that.

use crate::protocol::{MAX_PARKED_PER_TAG, MAX_PARKED_TOTAL, MAX_PARK_SECS};
use std::collections::HashMap;
use vega_core::tag::{CollectToken, Tag};

#[derive(Debug, Clone)]
struct Parked {
    envelope: Vec<u8>,
    expires_at: u64,
    token: CollectToken,
}

/// Bounded, in-memory store of envelopes awaiting collection.
///
/// In memory on purpose for now: mail that does not survive a restart is a
/// weaker promise, but it also means a relay never accumulates other people's
/// ciphertext on disk. Persisting this is a decision to make deliberately.
#[derive(Debug, Default)]
pub struct Mailbox {
    by_tag: HashMap<Tag, Vec<Parked>>,
    total: usize,
}

/// Why a park request was turned down. Returned to the sender so it can try
/// another peer rather than silently assuming success.
#[derive(Debug, PartialEq, Eq)]
pub enum ParkOutcome {
    Accepted,
    Full,
    TooLarge,
    BadExpiry,
}

impl Mailbox {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.total
    }

    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// Accept an envelope for later collection.
    pub fn park(
        &mut self,
        tag: Tag,
        token: CollectToken,
        envelope: Vec<u8>,
        expires_at: u64,
        now: u64,
        max_bytes: usize,
    ) -> ParkOutcome {
        if envelope.len() > max_bytes {
            return ParkOutcome::TooLarge;
        }
        if expires_at <= now || expires_at > now + MAX_PARK_SECS {
            return ParkOutcome::BadExpiry;
        }

        self.expire(now);

        if self.total >= MAX_PARKED_TOTAL {
            return ParkOutcome::Full;
        }

        let slot = self.by_tag.entry(tag).or_default();
        if slot.len() >= MAX_PARKED_PER_TAG {
            // Full for this tag: drop the oldest rather than refuse. A recipient
            // who has been away a long time is better served by recent mail.
            slot.remove(0);
            self.total -= 1;
        }
        slot.push(Parked {
            envelope,
            expires_at,
            token,
        });
        self.total += 1;
        ParkOutcome::Accepted
    }

    /// Hand over and remove everything held under these tags, for a claimant who
    /// can prove they belong to the conversation.
    ///
    /// A wrong token takes nothing and destroys nothing — an attacker who
    /// guesses gets an empty answer, and the real recipient's mail is still
    /// there when they come to collect it.
    pub fn collect(&mut self, claims: &[(Tag, CollectToken)], now: u64) -> Vec<Vec<u8>> {
        self.expire(now);
        let mut out = Vec::new();

        for (tag, token) in claims {
            let Some(items) = self.by_tag.get_mut(tag) else {
                continue;
            };

            let (mine, theirs): (Vec<_>, Vec<_>) = items
                .drain(..)
                .partition(|p| constant_time_eq(&p.token, token));

            self.total -= mine.len();
            out.extend(mine.into_iter().map(|p| p.envelope));

            *items = theirs;
            if items.is_empty() {
                self.by_tag.remove(tag);
            }
        }
        out
    }

    /// Drop anything past its expiry.
    pub fn expire(&mut self, now: u64) {
        let mut removed = 0;
        self.by_tag.retain(|_, items| {
            let before = items.len();
            items.retain(|p| p.expires_at > now);
            removed += before - items.len();
            !items.is_empty()
        });
        self.total -= removed;
    }
}

/// Compare without leaking, through timing, how much of a token was right.
fn constant_time_eq(a: &CollectToken, b: &CollectToken) -> bool {
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000;
    const MAX: usize = 1024;

    fn tag(n: u8) -> Tag {
        [n; 16]
    }

    fn token(n: u8) -> CollectToken {
        [n; 32]
    }

    #[test]
    fn parked_mail_comes_back_once() {
        let mut m = Mailbox::new();
        assert_eq!(
            m.park(tag(1), token(1), b"hello".to_vec(), NOW + 60, NOW, MAX),
            ParkOutcome::Accepted
        );
        assert_eq!(
            m.collect(&[(tag(1), token(1))], NOW),
            vec![b"hello".to_vec()]
        );
        // Collection is destructive — a second collect gets nothing.
        assert!(m.collect(&[(tag(1), token(1))], NOW).is_empty());
        assert!(m.is_empty());
    }

    #[test]
    fn the_wrong_token_collects_nothing_and_destroys_nothing() {
        let mut m = Mailbox::new();
        m.park(tag(1), token(1), b"private".to_vec(), NOW + 60, NOW, MAX);

        // A relay that carried the message knows the tag but not the secret.
        assert!(m.collect(&[(tag(1), token(9))], NOW).is_empty());
        assert_eq!(m.len(), 1, "a failed claim must not delete the mail");

        // The real recipient still gets it.
        assert_eq!(
            m.collect(&[(tag(1), token(1))], NOW),
            vec![b"private".to_vec()]
        );
    }

    #[test]
    fn only_the_addressed_tag_is_returned() {
        let mut m = Mailbox::new();
        m.park(tag(1), token(1), b"for-one".to_vec(), NOW + 60, NOW, MAX);
        m.park(tag(2), token(2), b"for-two".to_vec(), NOW + 60, NOW, MAX);
        assert_eq!(
            m.collect(&[(tag(2), token(2))], NOW),
            vec![b"for-two".to_vec()]
        );
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn oversized_envelopes_are_refused() {
        let mut m = Mailbox::new();
        assert_eq!(
            m.park(tag(1), token(1), vec![0u8; MAX + 1], NOW + 60, NOW, MAX),
            ParkOutcome::TooLarge
        );
        assert!(m.is_empty());
    }

    #[test]
    fn expired_mail_is_dropped() {
        let mut m = Mailbox::new();
        m.park(tag(1), token(1), b"stale".to_vec(), NOW + 10, NOW, MAX);
        assert!(m.collect(&[(tag(1), token(1))], NOW + 11).is_empty());
        assert!(m.is_empty());
    }

    #[test]
    fn a_ridiculous_expiry_is_refused() {
        let mut m = Mailbox::new();
        // Past, and absurdly far future — both are attempts to pin storage.
        assert_eq!(
            m.park(tag(1), token(1), b"x".to_vec(), NOW - 1, NOW, MAX),
            ParkOutcome::BadExpiry
        );
        assert_eq!(
            m.park(
                tag(1),
                token(1),
                b"x".to_vec(),
                NOW + MAX_PARK_SECS + 1,
                NOW,
                MAX
            ),
            ParkOutcome::BadExpiry
        );
        assert!(m.is_empty());
    }

    #[test]
    fn one_tag_cannot_grow_without_bound() {
        let mut m = Mailbox::new();
        for i in 0..MAX_PARKED_PER_TAG + 20 {
            m.park(
                tag(1),
                token(1),
                vec![u8::try_from(i % 256).unwrap()],
                NOW + 60,
                NOW,
                MAX,
            );
        }
        assert_eq!(m.len(), MAX_PARKED_PER_TAG);
        // The most recent messages are the ones kept.
        let got = m.collect(&[(tag(1), token(1))], NOW);
        assert_eq!(got.len(), MAX_PARKED_PER_TAG);
        assert_eq!(
            got.last().unwrap()[0],
            u8::try_from((MAX_PARKED_PER_TAG + 19) % 256).unwrap()
        );
    }

    #[test]
    fn the_node_wide_cap_holds_against_many_tags() {
        let mut m = Mailbox::new();
        let mut accepted = 0;
        for t in 0..=255u8 {
            for _ in 0..MAX_PARKED_PER_TAG {
                if m.park(tag(t), token(t), b"x".to_vec(), NOW + 60, NOW, MAX)
                    == ParkOutcome::Accepted
                {
                    accepted += 1;
                }
            }
        }
        assert!(accepted >= MAX_PARKED_TOTAL);
        assert!(m.len() <= MAX_PARKED_TOTAL);
    }

    #[test]
    fn totals_stay_consistent_across_expiry_and_collection() {
        let mut m = Mailbox::new();
        m.park(tag(1), token(1), b"a".to_vec(), NOW + 10, NOW, MAX);
        m.park(tag(1), token(1), b"b".to_vec(), NOW + 100, NOW, MAX);
        m.park(tag(2), token(2), b"c".to_vec(), NOW + 10, NOW, MAX);
        assert_eq!(m.len(), 3);

        m.expire(NOW + 50);
        assert_eq!(m.len(), 1);
        assert_eq!(m.collect(&[(tag(1), token(1))], NOW + 50).len(), 1);
        assert_eq!(m.len(), 0);
    }
}
