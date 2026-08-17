//! End-to-end: two real nodes, real sockets, real ciphertext.
//!
//! These are the tests that decide whether the design works, as opposed to
//! whether the units compile. Everything runs over loopback QUIC with mDNS
//! turned off, so the result is deterministic rather than dependent on whatever
//! else is on the tester's network.

use std::time::Duration;
use tokio::sync::mpsc::Receiver;
use vega_core::{
    bootstrap_account, envelope::Body, fan_out, AccountId, ChainState, Content, Directory,
    Envelope, Recipient, Sessions,
};
use vega_net::{NetEvent, Node, NodeConfig, NodeHandle, PeerId};

const PATIENCE: Duration = Duration::from_secs(20);

/// The chains a peer has accepted through an invite.
struct Known(Vec<ChainState>);

impl Directory for Known {
    fn chain_for(&self, account: &AccountId) -> Option<ChainState> {
        self.0.iter().find(|s| s.account_id == *account).cloned()
    }
}

/// A node that only talks to who it is told to talk to.
fn isolated() -> NodeConfig {
    NodeConfig {
        listen: vec!["/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap()],
        bootstrap: vec![],
        enable_mdns: false,
        enable_upnp: false,
        act_as_relay: false,
        act_as_mailbox: true,
        // These two are the whole DHT in these tests; if neither serves, a
        // published record has nowhere to land.
        kad_mode: vega_net::KadMode::Server,
    }
}

/// Wait for the first event matching `pick`, or fail the test.
async fn wait_for<T>(
    events: &mut Receiver<NetEvent>,
    what: &str,
    mut pick: impl FnMut(&NetEvent) -> Option<T>,
) -> T {
    let found = tokio::time::timeout(PATIENCE, async {
        while let Some(event) = events.recv().await {
            if let Some(v) = pick(&event) {
                return Some(v);
            }
        }
        None
    })
    .await;

    match found {
        Ok(Some(v)) => v,
        Ok(None) => panic!("event stream ended while waiting for {what}"),
        Err(_) => panic!("timed out waiting for {what}"),
    }
}

async fn listen_addr(events: &mut Receiver<NetEvent>) -> vega_net::Multiaddr {
    wait_for(events, "a listen address", |e| match e {
        NetEvent::Listening(a) => Some(a.clone()),
        _ => None,
    })
    .await
}

/// Connect `b` to `a`, returning a's peer id.
async fn connect(
    a: (&NodeHandle, &mut Receiver<NetEvent>),
    b: (&NodeHandle, &mut Receiver<NetEvent>),
) -> PeerId {
    let (a_handle, a_events) = a;
    let (b_handle, b_events) = b;

    let addr = listen_addr(a_events).await;
    let a_id = a_handle.local_peer_id().await.unwrap();

    b_handle
        .dial(vega_net::with_peer(addr, a_id))
        .await
        .expect("dial should be accepted");

    wait_for(b_events, "an outbound connection", |e| match e {
        NetEvent::PeerConnected(p) if *p == a_id => Some(()),
        _ => None,
    })
    .await;

    a_id
}

#[tokio::test]
async fn an_envelope_crosses_the_wire_intact() {
    let (a, mut a_events) = Node::spawn(isolated()).unwrap();
    let (b, mut b_events) = Node::spawn(isolated()).unwrap();

    let a_id = connect((&a, &mut a_events), (&b, &mut b_events)).await;

    let payload = b"opaque ciphertext".to_vec();
    b.deliver(a_id, payload.clone()).await.unwrap();

    let got = wait_for(&mut a_events, "the delivered envelope", |e| match e {
        NetEvent::Envelope { bytes, .. } => Some(bytes.clone()),
        _ => None,
    })
    .await;

    assert_eq!(got, payload);
}

#[tokio::test]
async fn two_accounts_exchange_an_encrypted_message_over_the_network() {
    // Alice and Bob exist only as key material and a signed device list.
    let (mut alice, alice_chain) = bootstrap_account("alice-laptop").unwrap();
    let (mut bob, bob_chain) = bootstrap_account("bob-laptop").unwrap();
    let mut alice_sessions = Sessions::new();
    let mut bob_sessions = Sessions::new();

    let (a_net, mut a_events) = Node::spawn(isolated()).unwrap();
    let (b_net, mut b_events) = Node::spawn(isolated()).unwrap();
    let bob_peer = connect((&b_net, &mut b_events), (&a_net, &mut a_events)).await;

    // Contact exchange: each side has the other's verified chain.
    let bob_state = bob_chain.validate().unwrap();
    let pairwise = alice.pairwise_with(&bob_state.contact, &bob.account_id);

    let now = vega_core::now();
    let content = Content::new(
        bob.account_id,
        now,
        1,
        Body::Text {
            text: "the ladder works".into(),
        },
    );

    let envelopes = fan_out(
        &mut alice,
        &mut alice_sessions,
        &Recipient {
            account: bob.account_id,
            state: &bob_state,
            pairwise: &pairwise,
        },
        None,
        &content,
        now,
    )
    .unwrap();
    assert_eq!(envelopes.len(), 1, "bob has exactly one device");

    a_net
        .deliver(bob_peer, envelopes[0].1.to_bytes().unwrap())
        .await
        .unwrap();

    let bytes = wait_for(&mut b_events, "alice's envelope", |e| match e {
        NetEvent::Envelope { bytes, .. } => Some(bytes.clone()),
        _ => None,
    })
    .await;

    let envelope = Envelope::from_bytes(&bytes).unwrap();
    // Bob added Alice as a contact, so he holds the chain that vouches for her.
    let bob_knows = Known(vec![alice_chain.validate().unwrap(), bob_state.clone()]);
    let opened = bob_sessions
        .decrypt(&mut bob, &envelope, &bob_knows)
        .unwrap();

    assert_eq!(opened.from_account, alice.account_id);
    assert_eq!(opened.content.text(), Some("the ladder works"));

    // And the sender is not recoverable from the wire form.
    let raw = String::from_utf8_lossy(&bytes);
    assert!(
        !raw.contains(&alice.account_id.to_display()),
        "the account id must not appear in the envelope"
    );
}

/// The one place that holds `FILE_CHUNK_BYTES` and `MAX_ENVELOPE_BYTES`
/// together.
///
/// vega-core picks the chunk size and cannot see the wire limit; vega-net
/// enforces the wire limit and does not know what a file is. Nothing but this
/// test connects the two, and without it a chunk size raised for throughput
/// would produce envelopes that every peer silently refuses — a file transfer
/// that never completes and never says why.
#[tokio::test]
async fn a_full_chunk_envelope_fits_on_the_wire() {
    let (mut alice, _alice_chain) = bootstrap_account("alice-laptop").unwrap();
    let (bob, bob_chain) = bootstrap_account("bob-laptop").unwrap();
    let mut alice_sessions = Sessions::new();

    let bob_state = bob_chain.validate().unwrap();
    let pairwise = alice.pairwise_with(&bob_state.contact, &bob.account_id);
    let now = vega_core::now();

    // A chunk carrying every byte it is allowed to carry, of incompressible
    // data — a run of zeroes would be a fine chunk and a useless measurement.
    let data: Vec<u8> = (0..vega_core::FILE_CHUNK_BYTES)
        .map(|i| i.wrapping_mul(31).to_le_bytes()[0] ^ (i >> 3).to_le_bytes()[1])
        .collect();
    let content = Content::new(
        bob.account_id,
        now,
        1,
        Body::FileChunk {
            // A worst-case manifest: the longest name a peer may send, since
            // that is what rides along on every chunk.
            file: vega_core::FileManifest {
                transfer: [0xab; 32],
                name: "n".repeat(255),
                size: vega_core::MAX_FILE_BYTES,
                hash: [0xcd; 32],
                chunks: 107,
            },
            index: 0,
            data,
        },
    );

    let envelopes = fan_out(
        &mut alice,
        &mut alice_sessions,
        &Recipient {
            account: bob.account_id,
            state: &bob_state,
            pairwise: &pairwise,
        },
        None,
        &content,
        now,
    )
    .unwrap();

    let wire = envelopes[0].1.to_bytes().unwrap();
    assert!(
        wire.len() <= vega_net::protocol::MAX_ENVELOPE_BYTES,
        "a full chunk makes a {} byte envelope, over the {} byte limit — \
         lower FILE_CHUNK_BYTES",
        wire.len(),
        vega_net::protocol::MAX_ENVELOPE_BYTES
    );

    // A pre-key envelope is the largest form: it carries the material that opens
    // a session. Later chunks on an established session are smaller, so proving
    // the first one fits proves they all do.
    let headroom = vega_net::protocol::MAX_ENVELOPE_BYTES - wire.len();
    assert!(
        headroom >= 8 * 1024,
        "only {headroom} bytes of headroom under the wire limit; too tight to \
         absorb a protocol field being added later"
    );
}

/// A file gets exactly the protection a sentence does, and this is the test
/// that says so.
///
/// Files are chunked into ordinary `Content` messages, so in principle they ride
/// the same ratchet and the same seal. In principle is not good enough for the
/// question "is what I send encrypted": the bytes are checked here against the
/// actual wire form, and a peer who knows both parties is asked to open one.
#[tokio::test]
async fn a_file_is_opaque_on_the_wire() {
    let (mut alice, alice_chain) = bootstrap_account("alice").unwrap();
    let (bob, bob_chain) = bootstrap_account("bob").unwrap();
    let (mut eve, _) = bootstrap_account("eve").unwrap();
    let mut alice_sessions = Sessions::new();
    let mut eve_sessions = Sessions::new();

    let bob_state = bob_chain.validate().unwrap();
    let pairwise = alice.pairwise_with(&bob_state.contact, &bob.account_id);
    let now = vega_core::now();

    // Distinctive enough that finding it in the ciphertext could not be luck,
    // and shaped like a real file: a PNG header, then a recognisable payload.
    let secret = b"\x89PNG\r\n\x1a\n SECRET-PIXELS-DO-NOT-LEAK";
    let manifest = vega_core::FileManifest {
        transfer: [0x11; 32],
        name: "bank-statement.png".into(),
        size: secret.len() as u64,
        // Not checked here: nothing reassembles this, and what is under test is
        // what a peer carrying the envelope can see.
        hash: [0x22; 32],
        chunks: 1,
    };

    let to_bob = Recipient {
        account: bob.account_id,
        state: &bob_state,
        pairwise: &pairwise,
    };

    let mut wire = Vec::new();
    for body in [
        Body::File {
            file: manifest.clone(),
        },
        Body::FileChunk {
            file: manifest.clone(),
            index: 0,
            data: secret.to_vec(),
        },
    ] {
        let content = Content::new(bob.account_id, now, 1, body);
        let envelopes = fan_out(
            &mut alice,
            &mut alice_sessions,
            &to_bob,
            None,
            &content,
            now,
        )
        .unwrap();
        wire.push(envelopes[0].1.clone());
    }

    for envelope in &wire {
        let bytes = envelope.to_bytes().unwrap();
        let raw = String::from_utf8_lossy(&bytes);

        // The file itself.
        assert!(
            !bytes.windows(secret.len()).any(|w| w == secret),
            "the file's bytes appear in the envelope"
        );
        // What it is called. A relay learning "bank-statement.png" would be a
        // leak even with the contents sealed.
        assert!(
            !raw.contains("bank-statement"),
            "the file name appears in the envelope"
        );
        // And who sent it.
        assert!(
            !raw.contains(&alice.account_id.to_display()),
            "the sender's account id appears in the envelope"
        );

        // Eve knows both parties' chains and still cannot open it.
        let eve_knows = Known(vec![
            alice_chain.validate().unwrap(),
            bob_chain.validate().unwrap(),
        ]);
        assert!(
            eve_sessions
                .decrypt(&mut eve, envelope, &eve_knows)
                .is_err(),
            "a non-recipient opened a file envelope"
        );
    }
}

#[tokio::test]
async fn a_stranger_on_the_same_network_learns_nothing() {
    let (mut alice, alice_chain) = bootstrap_account("alice").unwrap();
    let (bob, bob_chain) = bootstrap_account("bob").unwrap();
    let (mut eve, _) = bootstrap_account("eve").unwrap();
    let mut alice_sessions = Sessions::new();
    let mut eve_sessions = Sessions::new();

    let bob_state = bob_chain.validate().unwrap();
    let pairwise = alice.pairwise_with(&bob_state.contact, &bob.account_id);
    let now = vega_core::now();

    let envelopes = fan_out(
        &mut alice,
        &mut alice_sessions,
        &Recipient {
            account: bob.account_id,
            state: &bob_state,
            pairwise: &pairwise,
        },
        None,
        &Content::new(
            bob.account_id,
            now,
            1,
            Body::Text {
                text: "not for eve".into(),
            },
        ),
        now,
    )
    .unwrap();

    // Eve is on the LAN and sees the envelope — this is the normal case, since
    // delivery offers it to every connected peer.
    // Eve even knows both parties' chains — she still cannot get past the seal.
    let eve_knows = Known(vec![
        alice_chain.validate().unwrap(),
        bob_chain.validate().unwrap(),
    ]);
    let (_, envelope) = &envelopes[0];
    assert!(
        eve_sessions
            .decrypt(&mut eve, envelope, &eve_knows)
            .is_err(),
        "a non-recipient must not be able to open an envelope"
    );

    // Nor can she recognise the routing tag as belonging to anyone she knows.
    let eve_guess = eve
        .pairwise_with(&bob_state.contact, &bob.account_id)
        .tag_for(&bob.account_id, vega_core::epoch_at(now));
    assert_ne!(envelope.to_tag, eve_guess);
}

/// A receipt acknowledges a message. Acknowledging a receipt would never
/// terminate, so the loop has to stop after exactly one round.
#[tokio::test]
async fn a_receipt_does_not_produce_another_receipt() {
    let (mut alice, alice_chain) = bootstrap_account("alice").unwrap();
    let (mut bob, bob_chain) = bootstrap_account("bob").unwrap();
    let mut alice_sessions = Sessions::new();
    let mut bob_sessions = Sessions::new();

    let alice_state = alice_chain.validate().unwrap();
    let bob_state = bob_chain.validate().unwrap();
    let both = Known(vec![alice_state.clone(), bob_state.clone()]);
    let now = vega_core::now();

    // Alice sends; Bob opens it.
    let bob_account = bob.account_id;
    let alice_to_bob = alice.pairwise_with(&bob_state.contact, &bob_account);
    let text = fan_out(
        &mut alice,
        &mut alice_sessions,
        &Recipient {
            account: bob_account,
            state: &bob_state,
            pairwise: &alice_to_bob,
        },
        None,
        &Content::new(
            bob_account,
            now,
            1,
            Body::Text {
                text: "arrived".into(),
            },
        ),
        now,
    )
    .unwrap();
    let opened = bob_sessions.decrypt(&mut bob, &text[0].1, &both).unwrap();

    // Bob acknowledges it.
    let alice_account = alice.account_id;
    let bob_to_alice = bob.pairwise_with(&alice_state.contact, &alice_account);
    let receipt = fan_out(
        &mut bob,
        &mut bob_sessions,
        &Recipient {
            account: alice_account,
            state: &alice_state,
            pairwise: &bob_to_alice,
        },
        None,
        &Content::new(
            alice_account,
            now,
            2,
            Body::Receipt {
                message_id: opened.content.id,
            },
        ),
        now,
    )
    .unwrap();

    let back = alice_sessions
        .decrypt(&mut alice, &receipt[0].1, &both)
        .unwrap();

    // A receipt names the message it clears, and carries no text — so the
    // handler that would generate another one has nothing to act on.
    match back.content.body {
        Body::Receipt { message_id } => assert_eq!(message_id, opened.content.id),
        other => panic!("expected a receipt, got {other:?}"),
    }
    assert_eq!(back.content.text(), None);
}

#[tokio::test]
async fn mail_parked_with_a_peer_is_collected_later() {
    let (a, mut a_events) = Node::spawn(isolated()).unwrap();
    let (b, mut b_events) = Node::spawn(isolated()).unwrap();
    let a_id = connect((&a, &mut a_events), (&b, &mut b_events)).await;

    let tag = [7u8; 16];
    let token = [42u8; 32];
    let envelope = b"held for later".to_vec();
    let expires = vega_core::now() + 600;

    // B parks mail with A while the recipient is offline...
    b.park(a_id, tag, token, envelope.clone(), expires)
        .await
        .unwrap();

    // A peer that only saw the tag on the wire cannot take it.
    assert!(b
        .collect(a_id, vec![(tag, [0u8; 32])])
        .await
        .unwrap()
        .is_empty());

    // ...and the real recipient collects it once back.
    let collected = b.collect(a_id, vec![(tag, token)]).await.unwrap();
    assert_eq!(collected, vec![envelope]);

    // Collection is destructive, so it is not served twice.
    assert!(b
        .collect(a_id, vec![(tag, token)])
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn a_rendezvous_record_survives_a_round_trip_through_the_dht() {
    // Two nodes, one bootstrapped off the other, so they share a routing table.
    let (a, mut a_events) = Node::spawn(isolated()).unwrap();
    let a_addr = listen_addr(&mut a_events).await;
    let a_id = a.local_peer_id().await.unwrap();

    let mut config = isolated();
    config.bootstrap = vec![vega_net::with_peer(a_addr.clone(), a_id)];
    let (b, mut b_events) = Node::spawn(config).unwrap();

    wait_for(&mut b_events, "the bootstrap connection", |e| match e {
        NetEvent::PeerConnected(p) if *p == a_id => Some(()),
        _ => None,
    })
    .await;

    let key = [42u8; 32];
    let record = vega_net::seal_record(
        &[9u8; 32],
        &vega_net::AddressRecord::new(&a_id, std::slice::from_ref(&a_addr), vega_core::now()),
    )
    .unwrap();

    b.publish(key, record.clone()).await.unwrap();

    let found = tokio::time::timeout(PATIENCE, async {
        loop {
            if let Ok(values) = a.lookup(key).await {
                if !values.is_empty() {
                    return values;
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .expect("the record should become findable");

    assert!(found.contains(&record));

    // And it is unreadable without the pairwise-derived key.
    assert!(vega_net::open_record(&[0u8; 32], &found[0]).is_err());
    let opened = vega_net::open_record(&[9u8; 32], &found[0]).unwrap();
    assert_eq!(opened.peer().unwrap(), a_id);
}
