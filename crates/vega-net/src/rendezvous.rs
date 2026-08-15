//! Tier 2: finding a peer without telling the network who you are looking for.
//!
//! A record is published under `HKDF(pairwise_secret, epoch)` and encrypted
//! under a sibling key. A DHT node storing it sees a random-looking key holding
//! random-looking bytes; only a contact can compute either. That is what stops
//! the DHT from becoming a scrapable map of who talks to whom.

use crate::error::{Error, Result};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use libp2p::{Multiaddr, PeerId};
use rand::RngCore;
use serde::{Deserialize, Serialize};

/// Records live an hour. Short enough that a stale address is not chased for
/// long, long enough that a peer is not republishing constantly on battery.
pub const RECORD_TTL_SECS: u64 = 3600;

/// Where a peer can currently be reached, plus the ephemeral network identity
/// it is using this epoch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressRecord {
    /// The libp2p identity for this epoch. Deliberately not derived from the
    /// account key — see the note in `node.rs`.
    pub peer_id: String,
    pub addrs: Vec<String>,
    pub published_at: u64,
    pub expires_at: u64,
    /// The publisher's sigchain. A contact who looks us up to find an address
    /// also picks up any devices and one-time keys published since they last
    /// heard from us — which is what keeps prekeys from running dry with no
    /// server to fetch them from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain: Option<vega_core::Sigchain>,
}

impl AddressRecord {
    pub fn new(peer_id: &PeerId, addrs: &[Multiaddr], now: u64) -> Self {
        Self {
            peer_id: peer_id.to_string(),
            addrs: addrs.iter().map(|a| a.to_string()).collect(),
            published_at: now,
            expires_at: now + RECORD_TTL_SECS,
            chain: None,
        }
    }

    pub fn with_chain(mut self, chain: vega_core::Sigchain) -> Self {
        self.chain = Some(chain);
        self
    }

    pub fn peer(&self) -> Result<PeerId> {
        self.peer_id
            .parse()
            .map_err(|_| Error::Protocol("record carries a malformed peer id".into()))
    }

    /// Addresses that parse. A record with some junk in it is still useful —
    /// drop the bad entries rather than the whole record.
    pub fn multiaddrs(&self) -> Vec<Multiaddr> {
        self.addrs.iter().filter_map(|a| a.parse().ok()).collect()
    }

    pub fn is_fresh(&self, now: u64) -> bool {
        now < self.expires_at
    }
}

/// Encrypt a record under the pairwise-derived record key.
pub fn seal_record(key: &[u8; 32], record: &AddressRecord) -> Result<Vec<u8>> {
    let plaintext = serde_json::to_vec(record)?;
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));

    let mut nonce = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);

    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &plaintext,
                aad: b"vega:rendezvous-record:v1",
            },
        )
        .map_err(|_| Error::Protocol("failed to seal rendezvous record".into()))?;

    let mut out = Vec::with_capacity(12 + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Decrypt a record. Returns an error rather than panicking on any garbage the
/// DHT hands back, which may be attacker-chosen.
pub fn open_record(key: &[u8; 32], bytes: &[u8]) -> Result<AddressRecord> {
    if bytes.len() < 12 + 16 {
        return Err(Error::Protocol("rendezvous record too short".into()));
    }
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(&bytes[..12]),
            Payload {
                msg: &bytes[12..],
                aad: b"vega:rendezvous-record:v1",
            },
        )
        .map_err(|_| Error::Protocol("rendezvous record does not decrypt".into()))?;
    Ok(serde_json::from_slice(&plaintext)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> AddressRecord {
        AddressRecord::new(
            &PeerId::random(),
            &["/ip4/192.168.1.10/udp/15000/quic-v1".parse().unwrap()],
            1_000,
        )
    }

    #[test]
    fn round_trips_under_the_right_key() {
        let key = [3u8; 32];
        let r = record();
        let sealed = seal_record(&key, &r).unwrap();
        assert_eq!(open_record(&key, &sealed).unwrap(), r);
    }

    #[test]
    fn a_non_contact_cannot_read_it() {
        let sealed = seal_record(&[3u8; 32], &record()).unwrap();
        assert!(open_record(&[4u8; 32], &sealed).is_err());
    }

    #[test]
    fn tampering_is_detected() {
        let key = [3u8; 32];
        let mut sealed = seal_record(&key, &record()).unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        assert!(open_record(&key, &sealed).is_err());
    }

    #[test]
    fn arbitrary_dht_bytes_do_not_panic() {
        let key = [3u8; 32];
        for junk in [vec![], vec![0u8; 8], vec![0xAB; 64], vec![0u8; 4096]] {
            assert!(open_record(&key, &junk).is_err());
        }
    }

    #[test]
    fn two_publications_of_the_same_record_differ() {
        let key = [3u8; 32];
        let r = record();
        // Fresh nonce each time — otherwise an observer could tell that a peer
        // republished an unchanged address.
        assert_ne!(
            seal_record(&key, &r).unwrap(),
            seal_record(&key, &r).unwrap()
        );
    }

    #[test]
    fn freshness_follows_the_ttl() {
        let r = record();
        assert!(r.is_fresh(1_000));
        assert!(r.is_fresh(1_000 + RECORD_TTL_SECS - 1));
        assert!(!r.is_fresh(1_000 + RECORD_TTL_SECS));
    }

    #[test]
    fn malformed_addresses_are_dropped_not_fatal() {
        let mut r = record();
        r.addrs.push("not-a-multiaddr".into());
        assert_eq!(r.multiaddrs().len(), 1);
    }
}
