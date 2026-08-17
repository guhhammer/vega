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

/// The largest file Vega will send.
///
/// Not a storage limit — a courtesy one. Every chunk is a separate envelope that
/// somebody else's node may end up carrying or holding, and a messenger that
/// lets one person push arbitrary volume through a stranger's relay is a
/// messenger nobody will run a relay for.
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Raw file bytes per chunk.
///
/// The ceiling that matters is `vega_net::protocol::MAX_ENVELOPE_BYTES` (256
/// KiB), and the path from here to there multiplies: base64 into the content
/// JSON (×4/3), then the Olm ciphertext is base64'd into the inner JSON and the
/// sealed box base64'd again into the envelope (×1.79 measured). So a chunk
/// costs about 2.4× its own size on the wire, and 96 KiB lands near 235 KiB with
/// room to spare. `chunk_envelope_fits_on_the_wire` in vega-net is what actually
/// holds the two numbers together — this comment would rot, that test cannot.
pub const FILE_CHUNK_BYTES: usize = 96 * 1024;

/// Longest file name accepted from a peer, in bytes.
///
/// Long enough for any real name including non-ASCII, short enough that it
/// cannot be used to bloat a manifest or a directory entry.
pub const MAX_FILE_NAME_BYTES: usize = 255;

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

    /// Announces a file. The bytes follow as `chunks` separate [`Body::FileChunk`]
    /// messages, because one envelope cannot hold more than 256 KiB.
    ///
    /// This is the message a thread shows. It is not what *opens* a transfer —
    /// the chunks carry their own copy of the manifest for that.
    File {
        file: FileManifest,
    },

    /// One piece of a file, and never a message in its own right: chunks are
    /// not shown, not receipted, and not copied to the sender's own devices.
    ///
    /// The manifest rides along on every chunk, at a cost of a few hundred bytes
    /// against a 96 KiB payload. That buys the property that matters on a
    /// network with no ordering guarantee: any chunk can open the transfer, so a
    /// chunk overtaking the manifest — a retry, a mailbox handing back what it
    /// held in whatever order it liked — is ordinary rather than fatal. Without
    /// it, an early chunk would be dropped for belonging to a transfer nobody
    /// had announced yet, and the file would never complete or explain why.
    FileChunk {
        file: FileManifest,
        index: u32,
        #[serde(with = "bytes_b64")]
        data: Vec<u8>,
    },
}

/// Everything needed to open a file transfer.
///
/// Every field is the sender's word. `size` and `chunks` are checked against
/// [`MAX_FILE_BYTES`] before a byte is allocated, `name` is never used as a path
/// without [`safe_file_name`], and `hash` is what finally decides whether the
/// reassembled file is the one that was announced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileManifest {
    #[serde(with = "crate::identity::hex32")]
    pub transfer: [u8; 32],
    pub name: String,
    pub size: u64,
    /// blake3 of the whole file, verified after reassembly.
    #[serde(with = "crate::identity::hex32")]
    pub hash: [u8; 32],
    pub chunks: u32,
}

/// Turn a name a peer chose into something safe to use as one path component.
///
/// A file name arrives from whoever sent it, which makes it the most obviously
/// hostile field in the protocol: `../../.ssh/authorized_keys` is a valid JSON
/// string. Everything that could steer a write somewhere it was not meant to go
/// is removed here, and the result is always exactly one component.
pub fn safe_file_name(name: &str) -> String {
    // Both separators, on every platform: a name minted on Windows still has to
    // be safe when it is written on Linux, and vice versa.
    let last = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .trim_matches(|c: char| c.is_control() || c.is_whitespace());

    let mut cleaned: String = last
        .chars()
        // NUL truncates a path in every C API underneath us; the rest are
        // refused outright by NTFS and would only invite quoting bugs elsewhere.
        .filter(|c| !c.is_control() && !matches!(c, '\0' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect();

    // Byte-bounded, but cut on a character boundary — truncating mid-emoji would
    // produce a name that is not valid UTF-8 to anything downstream.
    if cleaned.len() > MAX_FILE_NAME_BYTES {
        let cut = cleaned
            .char_indices()
            .take_while(|(i, c)| i + c.len_utf8() <= MAX_FILE_NAME_BYTES)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        cleaned.truncate(cut);
    }

    // Windows refuses a name ending in a dot or a space, silently trimming it —
    // which would make the name on disk differ from the name that was checked.
    let cleaned = cleaned.trim_end_matches(['.', ' ']);

    // `.` and `..` are directory traversal even without a separator.
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return "file".to_string();
    }

    // Device names are reserved on Windows whatever the extension, and opening
    // one talks to hardware rather than to a file.
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let stem = cleaned.split('.').next().unwrap_or_default();
    if RESERVED
        .iter()
        .any(|r| stem.eq_ignore_ascii_case(r) && !stem.is_empty())
    {
        return format!("_{cleaned}");
    }

    cleaned.to_string()
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
            Body::Receipt { .. } | Body::File { .. } | Body::FileChunk { .. } => None,
        }
    }

    /// The manifest, for a message that announces a file.
    ///
    /// A chunk is deliberately not a file message: it carries a manifest, but it
    /// is not something a thread shows.
    pub fn file(&self) -> Option<&FileManifest> {
        match &self.body {
            Body::File { file } => Some(file),
            _ => None,
        }
    }
}

/// How many chunks a file of this size is split into.
///
/// Returns `None` for a size past [`MAX_FILE_BYTES`], so the caller cannot get
/// as far as allocating for a file it was never going to accept.
pub fn chunk_count(size: u64) -> Option<u32> {
    if size == 0 || size > MAX_FILE_BYTES {
        return None;
    }
    let per = FILE_CHUNK_BYTES as u64;
    // Ceiling division. 10 MiB over 96 KiB is 107, nowhere near the top of a
    // u32 — but the conversion is checked rather than asserted in a comment, so
    // that raising MAX_FILE_BYTES can never quietly wrap this.
    u32::try_from(size.div_ceil(per)).ok()
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

    /// The name in a manifest is written by whoever sent it. These are the
    /// shapes that turn a file name into a write somewhere else entirely.
    #[test]
    fn a_hostile_file_name_becomes_one_harmless_component() {
        for hostile in [
            "../../.ssh/authorized_keys",
            "..\\..\\Windows\\System32\\drivers\\etc\\hosts",
            "/etc/passwd",
            "..",
            ".",
            "",
            "   ",
            "with\0nul",
            "trailing dots...",
            "CON",
            "nul.txt",
        ] {
            let safe = safe_file_name(hostile);
            assert!(!safe.is_empty(), "{hostile:?} produced an empty name");
            assert!(!safe.contains('/'), "{hostile:?} kept a separator");
            assert!(!safe.contains('\\'), "{hostile:?} kept a separator");
            assert!(!safe.contains('\0'), "{hostile:?} kept a NUL");
            assert_ne!(safe, "..");
            assert_ne!(safe, ".");
            assert_eq!(
                std::path::Path::new(&safe).components().count(),
                1,
                "{hostile:?} produced more than one path component"
            );
        }

        // Reserved device names survive as files rather than as devices.
        assert_eq!(safe_file_name("CON"), "_CON");
        assert_eq!(safe_file_name("nul.txt"), "_nul.txt");
        // The traversal is stripped, the actual name is kept.
        assert_eq!(
            safe_file_name("../../.ssh/authorized_keys"),
            "authorized_keys"
        );
    }

    /// Names are UTF-8 like everything else here, and the length cap is in bytes
    /// while the characters it cuts are not one byte each.
    #[test]
    fn a_long_emoji_name_is_cut_on_a_character_boundary() {
        let name = format!("{}.png", "🙂".repeat(200));
        let safe = safe_file_name(&name);
        assert!(safe.len() <= MAX_FILE_NAME_BYTES);
        // The real assertion: it is still a valid Rust string, which it could
        // not be if the cut had landed inside one of those four-byte characters.
        assert!(safe.chars().all(|c| c == '🙂'));
        assert_eq!(safe_file_name("café ☕.txt"), "café ☕.txt");
    }

    #[test]
    fn chunk_counts_cover_the_edges() {
        assert_eq!(chunk_count(0), None, "an empty file has nothing to send");
        assert_eq!(chunk_count(1), Some(1));
        assert_eq!(chunk_count(FILE_CHUNK_BYTES as u64), Some(1));
        assert_eq!(chunk_count(FILE_CHUNK_BYTES as u64 + 1), Some(2));
        assert_eq!(chunk_count(MAX_FILE_BYTES), Some(107));
        assert_eq!(chunk_count(MAX_FILE_BYTES + 1), None);
        assert_eq!(chunk_count(u64::MAX), None);
    }

    #[test]
    fn a_file_body_round_trips_with_its_chunks() {
        let manifest = FileManifest {
            transfer: [7u8; 32],
            name: "holiday 🏖️.jpg".into(),
            size: 200_000,
            hash: [9u8; 32],
            chunks: 2,
        };
        let content = Content::new(
            AccountId([3u8; 32]),
            0,
            0,
            Body::File {
                file: manifest.clone(),
            },
        );
        let back: Content = serde_json::from_slice(&serde_json::to_vec(&content).unwrap()).unwrap();
        let info = back.file().expect("a file body reports its manifest");
        assert_eq!(info, &manifest);
        assert_eq!(info.name, "holiday 🏖️.jpg");
        assert_eq!(back.text(), None, "a file is not text");

        let chunk = Content::new(
            AccountId([3u8; 32]),
            0,
            1,
            Body::FileChunk {
                file: manifest.clone(),
                index: 1,
                data: vec![0xff; 1024],
            },
        );
        let back: Content = serde_json::from_slice(&serde_json::to_vec(&chunk).unwrap()).unwrap();
        assert!(
            back.file().is_none(),
            "a chunk carries a manifest but is not a file message"
        );
        match back.body {
            Body::FileChunk { data, index, file } => {
                assert_eq!(index, 1);
                assert_eq!(data, vec![0xff; 1024]);
                assert_eq!(file, manifest, "the chunk must be able to open a transfer");
            }
            _ => panic!("wrong body"),
        }
    }

    #[test]
    fn message_ids_are_unique() {
        let a = Content::new(AccountId([1u8; 32]), 0, 0, Body::Text { text: "x".into() });
        let b = Content::new(AccountId([1u8; 32]), 0, 0, Body::Text { text: "x".into() });
        assert_ne!(a.id, b.id);
    }
}
