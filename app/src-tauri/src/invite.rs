//! Contact exchange.
//!
//! This is the trust root of the whole system and it is deliberately manual.
//! An invite carries everything needed to talk to someone — their account id,
//! their account contact key, and their signed device roster — so that no
//! server is ever asked "who is this person?".
//!
//! Anyone who can substitute an invite in transit becomes the conversation, so
//! invites are meant to travel over a channel the two people already trust: a
//! QR code across a table, a link over a channel they can verify, a code read
//! aloud. Safety-number comparison afterwards is what catches a substitution.

use serde::{Deserialize, Serialize};
use vega_core::{AccountId, DhKey, Sigchain};

const PREFIX: &str = "vega1:";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invite {
    pub account_id: AccountId,
    pub display_name: String,
    pub contact_key: DhKey,
    /// The full signed chain, so the recipient can verify the device roster
    /// offline rather than trusting the invite's word for it.
    pub chain: Sigchain,
}

impl Invite {
    pub fn encode(&self) -> Result<String, vega_core::Error> {
        let json = serde_json::to_vec(self)?;
        Ok(format!(
            "{PREFIX}{}",
            data_encoding::BASE64URL_NOPAD.encode(&json)
        ))
    }

    pub fn decode(s: &str) -> Result<Self, vega_core::Error> {
        let body = s
            .trim()
            .strip_prefix(PREFIX)
            .ok_or_else(|| vega_core::Error::Wire("not a Vega invite".into()))?;

        let json = data_encoding::BASE64URL_NOPAD
            .decode(body.as_bytes())
            .map_err(|e| vega_core::Error::Wire(e.to_string()))?;
        let invite: Invite = serde_json::from_slice(&json)?;

        // Everything below is verified before the invite is allowed to become a
        // contact — an invite is untrusted input from an unknown party.
        let state = invite.chain.validate()?;

        if state.account_id != invite.account_id {
            return Err(vega_core::Error::Wire(
                "invite's account id does not match its chain".into(),
            ));
        }
        if state.contact != invite.contact_key {
            return Err(vega_core::Error::Wire(
                "invite's contact key does not match its chain".into(),
            ));
        }
        if state.devices.is_empty() {
            return Err(vega_core::Error::Wire(
                "invite lists no devices to send to".into(),
            ));
        }

        Ok(invite)
    }

    /// The fingerprint both sides compare, before it is rendered.
    ///
    /// Ordering the two ids is what makes it symmetric: each person hashes the
    /// same pair in the same order and gets the same answer, with no round trip
    /// and nothing to agree on first.
    fn safety_digest(a: &AccountId, b: &AccountId) -> [u8; 32] {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let mut h = blake3::Hasher::new();
        h.update(b"vega:safety-number:v1");
        h.update(lo.as_bytes());
        h.update(hi.as_bytes());
        *h.finalize().as_bytes()
    }

    /// The short string two people read to each other to confirm no key was
    /// substituted. Both sides must see the same digits.
    pub fn safety_number(a: &AccountId, b: &AccountId) -> String {
        let digest = Self::safety_digest(a, b);

        digest[..15]
            .chunks(3)
            .map(|c| {
                let n = u32::from(c[0]) << 16 | u32::from(c[1]) << 8 | u32::from(c[2]);
                format!("{:05}", n % 100_000)
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The same fingerprint as words, for saying out loud.
    ///
    /// One byte of the digest per word, ten words: 80 bits, which is the usual
    /// floor for a fingerprint somebody has to be able to forge a collision
    /// against rather than merely guess. An attacker would have to grind an
    /// account whose safety phrase with you matches word for word.
    ///
    /// It is the *same digest* the digits come from, so the two renderings are
    /// one check, not two. Comparing either is enough; comparing both adds
    /// nothing beyond the 80 bits and the 83 already there.
    pub fn safety_words(a: &AccountId, b: &AccountId) -> String {
        Self::safety_digest(a, b)[..10]
            .iter()
            .map(|byte| crate::words::WORDS[*byte as usize])
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// One account's own fingerprint, as words — what somebody reads out while
    /// sending an invite.
    ///
    /// [`Self::safety_words`] needs both ids, so it cannot be shown until the
    /// two people already have each other. That is one step too late to catch
    /// an invite that was swapped on the way: by then the substitution is the
    /// conversation. This is the same idea applied to a single account. The
    /// sender reads these ten words aloud, the receiver sees the ten words
    /// computed from the invite that actually arrived, and anything swapped in
    /// transit is a different phrase before a single message has been sent.
    ///
    /// Ten words over the id cover the whole invite rather than just the id.
    /// An account id is `blake3(domain ‖ account key)`, the sigchain is signed
    /// by that key, and [`Self::decode`] refuses an invite whose chain, contact
    /// key or device roster do not match the id it claims — so pinning the id
    /// pins everything that travels with it.
    ///
    /// A domain tag of its own, so an identity phrase and a safety phrase can
    /// never come out identical and be mistaken for each other.
    pub fn identity_words(account: &AccountId) -> String {
        let mut h = blake3::Hasher::new();
        h.update(b"vega:identity-words:v1");
        h.update(account.as_bytes());

        h.finalize().as_bytes()[..10]
            .iter()
            .map(|byte| crate::words::WORDS[*byte as usize])
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vega_core::bootstrap_account;

    fn invite() -> Invite {
        let (identity, chain) = bootstrap_account("laptop").unwrap();
        Invite {
            account_id: identity.account_id,
            display_name: "Ada".into(),
            contact_key: identity.contact_public(),
            chain,
        }
    }

    #[test]
    fn round_trips() {
        let i = invite();
        let decoded = Invite::decode(&i.encode().unwrap()).unwrap();
        assert_eq!(decoded.account_id, i.account_id);
        assert_eq!(decoded.contact_key, i.contact_key);
    }

    #[test]
    fn junk_is_refused_without_panicking() {
        for junk in ["", "hello", "vega1:", "vega1:!!!!", "vega1:aGVsbG8"] {
            assert!(Invite::decode(junk).is_err());
        }
    }

    #[test]
    fn an_invite_claiming_the_wrong_account_is_refused() {
        let mut i = invite();
        i.account_id = bootstrap_account("other").unwrap().0.account_id;
        assert!(Invite::decode(&i.encode().unwrap()).is_err());
    }

    #[test]
    fn an_invite_claiming_the_wrong_contact_key_is_refused() {
        let mut i = invite();
        i.contact_key = bootstrap_account("other").unwrap().0.contact_public();
        assert!(Invite::decode(&i.encode().unwrap()).is_err());
    }

    #[test]
    fn a_tampered_chain_is_refused() {
        let i = invite();
        let encoded = i.encode().unwrap();
        let raw = data_encoding::BASE64URL_NOPAD
            .decode(encoded.strip_prefix(PREFIX).unwrap().as_bytes())
            .unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        value["chain"]["entries"][0]["ts"] = serde_json::json!(1);
        let repacked = format!(
            "{PREFIX}{}",
            data_encoding::BASE64URL_NOPAD.encode(&serde_json::to_vec(&value).unwrap())
        );
        assert!(Invite::decode(&repacked).is_err());
    }

    #[test]
    fn both_sides_compute_the_same_safety_number() {
        let a = bootstrap_account("a").unwrap().0.account_id;
        let b = bootstrap_account("b").unwrap().0.account_id;
        assert_eq!(Invite::safety_number(&a, &b), Invite::safety_number(&b, &a));
        assert_ne!(Invite::safety_number(&a, &b), Invite::safety_number(&a, &a));
        assert_eq!(Invite::safety_number(&a, &b).split(' ').count(), 5);
    }

    #[test]
    fn the_words_are_the_same_check_as_the_digits() {
        let a = bootstrap_account("a").unwrap().0.account_id;
        let b = bootstrap_account("b").unwrap().0.account_id;
        let c = bootstrap_account("c").unwrap().0.account_id;

        // Symmetric, like the digits: whoever reads them first, both hear the
        // same phrase.
        assert_eq!(Invite::safety_words(&a, &b), Invite::safety_words(&b, &a));
        assert_eq!(Invite::safety_words(&a, &b).split(' ').count(), 10);

        // A different person is a different phrase. This is the whole point:
        // an invite swapped in transit changes one of the two ids.
        assert_ne!(Invite::safety_words(&a, &b), Invite::safety_words(&a, &c));

        // Every word comes from the list, so nothing unpronounceable can appear.
        for word in Invite::safety_words(&a, &b).split(' ') {
            assert!(
                crate::words::WORDS.contains(&word),
                "{word} is not on the list"
            );
        }

        // The digits and the words move together, because they are two views of
        // one digest. If a future change broke that, comparing one and not the
        // other would silently stop meaning the same thing.
        assert_eq!(
            Invite::safety_number(&a, &b) == Invite::safety_number(&a, &c),
            Invite::safety_words(&a, &b) == Invite::safety_words(&a, &c)
        );
    }

    #[test]
    fn an_account_reads_out_as_its_own_phrase() {
        let a = bootstrap_account("a").unwrap().0.account_id;
        let b = bootstrap_account("b").unwrap().0.account_id;

        // One id, one phrase, every time — otherwise the sender and the
        // receiver would be comparing two different readings of the same thing.
        assert_eq!(Invite::identity_words(&a), Invite::identity_words(&a));
        assert_ne!(Invite::identity_words(&a), Invite::identity_words(&b));
        assert_eq!(Invite::identity_words(&a).split(' ').count(), 10);

        for word in Invite::identity_words(&a).split(' ') {
            assert!(
                crate::words::WORDS.contains(&word),
                "{word} is not on the list"
            );
        }

        // Two different checks with two different domain tags. An identity
        // phrase compared against a safety phrase must not be able to match by
        // construction, or somebody comparing the wrong pair would be reassured
        // by nothing at all.
        assert_ne!(Invite::identity_words(&a), Invite::safety_words(&a, &a));
    }

    /// The phrase has to survive the trip an invite actually makes, since it is
    /// what says the trip was clean.
    #[test]
    fn the_phrase_follows_the_invite_through_encoding() {
        let i = invite();
        let decoded = Invite::decode(&i.encode().unwrap()).unwrap();
        assert_eq!(
            Invite::identity_words(&decoded.account_id),
            Invite::identity_words(&i.account_id)
        );
    }
}
