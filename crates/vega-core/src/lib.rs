//! Vega core: identity, the device sigchain, and message cryptography.
//!
//! This crate knows nothing about networking. It turns a message plus a
//! recipient into a set of sealed envelopes, and sealed envelopes back into
//! messages — which transport carries them is [`vega_net`]'s problem.
//!
//! The layering, outermost first:
//!
//! - [`envelope::Envelope`] — routing tag and sealed box. What a relay sees.
//! - [`seal`] — one-shot ECIES hiding who the sender is.
//! - [`session`] — Olm double ratchet, per device pair.
//! - [`sigchain`] — which devices legitimately belong to an account.
//!
//! Nothing here panics on untrusted input, and nothing here talks to a server.

pub mod codec;
pub mod envelope;
pub mod error;
pub mod identity;
pub mod keys;
pub mod seal;
pub mod session;
pub mod sigchain;
pub mod store;
pub mod tag;

pub use envelope::{
    chunk_count, safe_file_name, Body, Content, Envelope, FileManifest, FILE_CHUNK_BYTES,
    MAX_FILE_BYTES,
};
pub use error::{Error, Result};
pub use identity::{AccountId, DeviceId, DeviceRecord, Identity, PrekeyBundle};
pub use keys::{DhKey, Sig, VerifyKey};
pub use session::{fan_out, Directory, Opened, Recipient, Sessions};
pub use sigchain::{ChainState, Sigchain};
pub use store::{Contact, OutboxItem, Store, StoredMessage, Transfer};
pub use tag::{epoch_at, Pairwise, Tag, EPOCH_SECS};

/// Seconds since the unix epoch.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Bring a brand new account into existence: identity, chain, first prekeys.
pub fn bootstrap_account(label: &str) -> Result<(Identity, Sigchain)> {
    let now = now();
    let mut identity = Identity::create(label);
    let mut chain = Sigchain::genesis(&identity, label, now)?;
    chain.append_signed_by_root(
        &identity,
        sigchain::Body::AddDevice(identity.device_record(now)),
        now,
    )?;
    let bundle = identity.replenish_prekeys(50);
    chain.append_signed_by_device(
        &identity,
        sigchain::Body::PublishPrekeys {
            device_id: identity.device_id,
            bundle,
        },
        now,
    )?;
    Ok((identity, chain))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bootstrapped_account_is_immediately_usable() {
        let (identity, chain) = bootstrap_account("laptop").unwrap();
        let state = chain.validate().unwrap();
        assert_eq!(state.account_id, identity.account_id);
        assert_eq!(state.devices.len(), 1);
        assert!(!state.prekeys[&identity.device_id].one_time.is_empty());
    }
}
