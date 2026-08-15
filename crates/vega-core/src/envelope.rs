//! What actually goes on the wire.
//!
//! Three nested layers, each visible to a different party:
//!
//! ```text
//! Envelope   { to_tag, epoch, sealed }   <- a relay sees this much
//!   Inner    { from_account, from_device, olm_ct }   <- only the recipient device
//!     Content{ text, sent_at, ... }      <- only after the ratchet opens it
//! ```

use crate::identity::{AccountId, DeviceId};
use crate::keys::DhKey;
use crate::tag::{Tag, TAG_LEN};
use serde::{Deserialize, Serialize};

pub const WIRE_VERSION: u8 = 1;

/// The outermost layer. Everything a relay or mailbox peer can read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u8,
    /// Rotating routing tag. Meaningless to anyone who is not the recipient.
    #[serde(with = "tag_hex")]
    pub to_tag: Tag,
    pub epoch: u64,
    /// A sealed box (see [`crate::seal`]) containing an [`Inner`].
    #[serde(with = "bytes_b64")]
    pub sealed: Vec<u8>,
}

impl Envelope {
    pub fn new(to_tag: Tag, epoch: u64, sealed: Vec<u8>) -> Self {
        Self {
            v: WIRE_VERSION,
            to_tag,
            epoch,
            sealed,
        }
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, crate::error::Error> {
        Ok(serde_json::to_vec(self)?)
    }

    pub fn from_bytes(b: &[u8]) -> Result<Self, crate::error::Error> {
        let env: Envelope = serde_json::from_slice(b)?;
        if env.v != WIRE_VERSION {
            return Err(crate::error::Error::Wire(format!(
                "unsupported wire version {}",
                env.v
            )));
        }
        Ok(env)
    }
}

/// Inside the seal. Reveals the sender — which is exactly why it is sealed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inner {
    pub from_account: AccountId,
    pub from_device: DeviceId,
    /// The sender's Olm identity key, needed to open a pre-key message.
    pub from_olm: DhKey,
    pub to_device: DeviceId,
    /// 0 = Olm pre-key message (opens a session), 1 = normal message.
    pub olm_type: u8,
    #[serde(with = "bytes_b64")]
    pub olm_ct: Vec<u8>,
}

/// Inside the Olm ciphertext: the message itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Content {
    /// Random, sender-assigned. Used for dedupe and for delivery receipts.
    #[serde(with = "crate::identity::hex32")]
    pub id: [u8; 32],
    /// The conversation this belongs to — the other party's account.
    pub conversation: AccountId,
    /// Sender's clock. A hint for ordering, never trusted for security.
    pub sent_at: u64,
    /// Per-conversation counter from this device. Monotonic.
    pub seq: u64,
    pub body: Body,

    /// The sender's own sigchain, attached when it has advanced since they last
    /// told us. This is how fresh one-time keys reach a contact without any
    /// server: it rides inside the ratchet, so it is private and authenticated,
    /// and it reaches exactly the people entitled to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_chain: Option<crate::sigchain::Sigchain>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Body {
    Text {
        text: String,
    },
    /// Confirms a message was decrypted, so the sender can stop retrying and
    /// any mailbox holding a copy can drop it.
    Receipt {
        #[serde(with = "crate::identity::hex32")]
        message_id: [u8; 32],
    },
    /// A copy of an outgoing message, addressed to the sender's own devices.
    /// This is how multi-device sync happens without a server.
    SelfCopy {
        to: AccountId,
        #[serde(with = "crate::identity::hex32")]
        message_id: [u8; 32],
        text: String,
    },
}

impl Content {
    pub fn new(conversation: AccountId, sent_at: u64, seq: u64, body: Body) -> Self {
        let mut id = [0u8; 32];
        use rand::RngCore;
        rand::rngs::OsRng.fill_bytes(&mut id);
        Self {
            id,
            conversation,
            sent_at,
            seq,
            body,
            sender_chain: None,
        }
    }

    /// Attach our chain so the recipient learns any new devices or prekeys.
    pub fn with_chain(mut self, chain: crate::sigchain::Sigchain) -> Self {
        self.sender_chain = Some(chain);
        self
    }

    pub fn text(&self) -> Option<&str> {
        match &self.body {
            Body::Text { text } => Some(text),
            Body::SelfCopy { text, .. } => Some(text),
            Body::Receipt { .. } => None,
        }
    }
}

mod tag_hex {
    use super::{Tag, TAG_LEN};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Tag, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&data_encoding::HEXLOWER.encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Tag, D::Error> {
        let s = String::deserialize(d)?;
        let raw = data_encoding::HEXLOWER
            .decode(s.as_bytes())
            .map_err(serde::de::Error::custom)?;
        if raw.len() != TAG_LEN {
            return Err(serde::de::Error::custom("bad tag length"));
        }
        let mut out = [0u8; TAG_LEN];
        out.copy_from_slice(&raw);
        Ok(out)
    }
}

mod bytes_b64 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&vodozemac::base64_encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        vodozemac::base64_decode(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_round_trips_through_bytes() {
        let env = Envelope::new([9u8; TAG_LEN], 42, vec![1, 2, 3, 4]);
        let bytes = env.to_bytes().unwrap();
        assert_eq!(Envelope::from_bytes(&bytes).unwrap(), env);
    }

    #[test]
    fn a_future_wire_version_is_refused() {
        let mut env = Envelope::new([0u8; TAG_LEN], 1, vec![]);
        env.v = 99;
        let bytes = serde_json::to_vec(&env).unwrap();
        assert!(Envelope::from_bytes(&bytes).is_err());
    }

    #[test]
    fn garbage_does_not_panic() {
        for junk in [&b""[..], b"{", b"null", b"[1,2,3]", &[0xff, 0xfe][..]] {
            assert!(Envelope::from_bytes(junk).is_err());
        }
    }

    #[test]
    fn message_ids_are_unique() {
        let a = Content::new(AccountId([1u8; 32]), 0, 0, Body::Text { text: "x".into() });
        let b = Content::new(AccountId([1u8; 32]), 0, 0, Body::Text { text: "x".into() });
        assert_ne!(a.id, b.id);
    }
}
