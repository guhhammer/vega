//! The Tauri shell: commands the UI calls, and the tasks that keep the node running.

mod invite;
mod keystore;
mod runtime;

use runtime::{Received, Runtime};
use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Mutex;
use vega_core::{AccountId, Store};
use vega_net::{NetEvent, Node, NodeConfig, NodeHandle};

/// Event names the frontend listens for.
const EVT_MESSAGE: &str = "vega://message";
const EVT_NETWORK: &str = "vega://network";

type Shared = Arc<Mutex<Runtime>>;

/// Everything the background tasks need. The node handle lives here rather than
/// inside `Runtime` so that network calls never happen while the lock is held.
#[derive(Clone)]
struct Ctx {
    shared: Shared,
    net: NodeHandle,
    /// Where completed file transfers are written. Kept here rather than derived
    /// on demand so that exactly one place decides where files go.
    files: Arc<std::path::PathBuf>,
}

/// Where a transfer's bytes live once they are whole.
///
/// One directory per transfer id: two people sending `photo.jpg` must not
/// overwrite each other, and a transfer id is the only name in the protocol
/// guaranteed to be unique. The file inside keeps the name it was sent under,
/// which is what makes the path worth showing to somebody.
fn file_path(root: &std::path::Path, transfer: &[u8; 32], name: &str) -> std::path::PathBuf {
    root.join(data_encoding::HEXLOWER.encode(transfer))
        // Sanitised on arrival already. Done again here because this is the call
        // that turns a name into a filesystem write, and a check at the point of
        // use survives a caller being added later that forgot the first one.
        .join(vega_core::safe_file_name(name))
}

fn write_file(
    root: &std::path::Path,
    transfer: &[u8; 32],
    name: &str,
    bytes: &[u8],
) -> std::io::Result<std::path::PathBuf> {
    let path = file_path(root, transfer, name);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, bytes)?;
    Ok(path)
}

// ---------------------------------------------------------------- views

#[derive(Debug, Serialize)]
pub struct MeView {
    account_id: String,
    short_id: String,
    device_label: String,
    device_id: String,
}

#[derive(Debug, Serialize)]
pub struct ContactView {
    account_id: String,
    short_id: String,
    display_name: String,
    verified: bool,
    safety_number: String,
}

#[derive(Debug, Serialize)]
pub struct MessageView {
    id: String,
    text: String,
    outgoing: bool,
    at: u64,
    /// Present when this message is a file rather than text.
    file: Option<FileView>,
}

/// A file message, as far as the UI needs to know.
#[derive(Debug, Serialize)]
pub struct FileView {
    name: String,
    size: u64,
    /// Where it is on disk, once all of it is. `None` while it is still coming.
    path: Option<String>,
    /// Chunks held out of chunks expected — the progress bar, and the reason
    /// this view is computed per read rather than stored with the message.
    have: u32,
    chunks: u32,
}

#[derive(Debug, Serialize)]
pub struct NetworkView {
    peer_id: String,
    listeners: Vec<String>,
    connected: usize,
}

// ---------------------------------------------------------------- commands

/// Errors cross into JavaScript as plain strings; the UI has no use for the
/// Rust type, and leaking internals into a web view is a bad habit.
fn fail(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command]
async fn me(ctx: State<'_, Ctx>) -> Result<MeView, String> {
    let rt = ctx.shared.lock().await;
    Ok(MeView {
        account_id: rt.identity.account_id.to_display(),
        short_id: rt.identity.account_id.short(),
        device_label: rt.identity.label.clone(),
        device_id: rt.identity.device_id.short(),
    })
}

#[tauri::command]
async fn my_invite(display_name: String, ctx: State<'_, Ctx>) -> Result<String, String> {
    let rt = ctx.shared.lock().await;
    rt.my_invite(&display_name).map_err(fail)
}

#[tauri::command]
async fn add_contact(invite: String, ctx: State<'_, Ctx>) -> Result<ContactView, String> {
    let mut rt = ctx.shared.lock().await;
    let id = rt.add_contact(&invite).map_err(fail)?;
    let contact = rt
        .store
        .get_contact(&id)
        .map_err(fail)?
        .ok_or("contact vanished immediately after being added")?;
    Ok(view_of(&contact, &rt.identity.account_id))
}

#[tauri::command]
async fn list_contacts(ctx: State<'_, Ctx>) -> Result<Vec<ContactView>, String> {
    let rt = ctx.shared.lock().await;
    let me = rt.identity.account_id;
    Ok(rt
        .store
        .list_contacts()
        .map_err(fail)?
        .iter()
        .map(|c| view_of(c, &me))
        .collect())
}

#[tauri::command]
async fn send_message(to: String, text: String, ctx: State<'_, Ctx>) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("nothing to send".into());
    }
    let account = AccountId::from_display(&to).map_err(fail)?;
    {
        let mut rt = ctx.shared.lock().await;
        rt.send_text(&account, &text).map_err(fail)?;
    }
    // Deliberately after the lock is released.
    flush(&ctx).await;
    Ok(())
}

/// Send a file. `data` is the file base64-encoded, because that is what survives
/// the JSON hop from the web view without a plugin or a filesystem permission —
/// the picker runs in the page, so Vega never gets to read a path of its own.
#[tauri::command]
async fn send_file(
    to: String,
    name: String,
    data: String,
    ctx: State<'_, Ctx>,
) -> Result<(), String> {
    let bytes = data_encoding::BASE64
        .decode(data.as_bytes())
        .map_err(|_| "that file did not survive the trip from the picker".to_string())?;

    let account = AccountId::from_display(&to).map_err(fail)?;
    let transfer = {
        let mut rt = ctx.shared.lock().await;
        rt.send_file(&account, &name, &bytes).map_err(fail)?
    };

    // The sender's own copy, under the same name and path the recipient will
    // use, so a file looks the same in both threads.
    if let Err(e) = write_file(&ctx.files, &transfer, &name, &bytes) {
        // Queued and on its way regardless; only our local copy is missing.
        tracing::warn!(error = %e, "could not keep a local copy of a sent file");
    }

    flush(&ctx).await;
    Ok(())
}

#[tauri::command]
async fn conversation(with: String, ctx: State<'_, Ctx>) -> Result<Vec<MessageView>, String> {
    let account = AccountId::from_display(&with).map_err(fail)?;
    let rt = ctx.shared.lock().await;
    Ok(rt
        .store
        .conversation(&account, 500)
        .map_err(fail)?
        .into_iter()
        .filter_map(|m| {
            let id = data_encoding::HEXLOWER.encode(&m.content.id[..8]);
            let common = |text: String, file: Option<FileView>| MessageView {
                id: id.clone(),
                text,
                outgoing: m.outgoing,
                at: m.content.sent_at,
                file,
            };

            if let Some(info) = m.content.file() {
                let path = file_path(&ctx.files, &info.transfer, &info.name);
                // Progress is read here rather than stored on the message,
                // because it changes without the message changing. A transfer
                // marked done, or pruned away entirely, is finished — and in
                // both cases the file on disk is what actually settles it.
                let (have, on_disk) = match rt.store.get_transfer(&info.transfer) {
                    Ok(Some(open)) if !open.done => (open.have, false),
                    _ => {
                        let there = path.is_file();
                        (if there { info.chunks } else { 0 }, there)
                    }
                };
                return Some(common(
                    String::new(),
                    Some(FileView {
                        name: info.name.to_string(),
                        size: info.size,
                        path: on_disk.then(|| path.display().to_string()),
                        have,
                        chunks: info.chunks,
                    }),
                ));
            }

            m.content.text().map(|t| common(t.to_string(), None))
        })
        .collect())
}

#[tauri::command]
async fn network(ctx: State<'_, Ctx>) -> Result<NetworkView, String> {
    let peer_id = ctx.net.local_peer_id().await.map_err(fail)?;
    let listeners = ctx.net.listeners().await.map_err(fail)?;
    Ok(NetworkView {
        peer_id: peer_id.to_string(),
        listeners: listeners.iter().map(|a| a.to_string()).collect(),
        connected: ctx.shared.lock().await.connected_count(),
    })
}

fn view_of(c: &vega_core::Contact, me: &AccountId) -> ContactView {
    ContactView {
        account_id: c.account_id.to_display(),
        short_id: c.account_id.short(),
        display_name: c.display_name.clone(),
        verified: c.verified,
        safety_number: invite::Invite::safety_number(me, &c.account_id),
    }
}

// ---------------------------------------------------------------- startup

async fn build_ctx(data_dir: std::path::PathBuf) -> Result<Ctx, String> {
    std::fs::create_dir_all(&data_dir).map_err(fail)?;

    let key = keystore::load_or_create(&data_dir.join("device.key")).map_err(fail)?;
    if key.backing == keystore::Backing::File {
        tracing::warn!(
            "the device key is stored in a file, not the platform keyring — \
             anything running as this user can read it"
        );
    }
    let pickle_key = key.bytes;
    let store = Store::open(data_dir.join("vega.redb")).map_err(fail)?;

    // An existing identity is loaded; a first run creates one. Never both.
    let (identity, chain) = match store.load_identity(&pickle_key).map_err(fail)? {
        Some(identity) => {
            let chain = store
                .load_chain(&identity.account_id)
                .map_err(fail)?
                .ok_or("identity on disk has no sigchain — the store is inconsistent")?;
            (identity, chain)
        }
        None => {
            let label = hostname_label();
            let (identity, chain) = vega_core::bootstrap_account(&label).map_err(fail)?;
            store.save_identity(&identity, &pickle_key).map_err(fail)?;
            store
                .save_chain(&identity.account_id, &chain)
                .map_err(fail)?;
            tracing::info!(account = %identity.account_id, "created a new account");
            (identity, chain)
        }
    };

    let sessions = store.load_sessions(&pickle_key).map_err(fail)?;

    // Seeds are read from a file rather than compiled in, so adding one does
    // not need a rebuild. A seed can neither read messages nor withhold them;
    // it only makes the DHT joinable and may offer to relay.
    let seeds = read_seeds(&data_dir.join("seeds.json"));
    let mut config = NodeConfig::default().with_bootstrap(seeds.clone());
    if cfg!(target_os = "android") {
        // A phone takes from the network without carrying anyone else's traffic.
        let mobile = NodeConfig::mobile();
        config.enable_upnp = mobile.enable_upnp;
        config.act_as_relay = mobile.act_as_relay;
        config.act_as_mailbox = mobile.act_as_mailbox;
    }

    let (net, events) = Node::spawn(config).map_err(fail)?;

    // Ask each seed for a relay reservation. If we turn out to be directly
    // reachable this is redundant, and if the seed does not relay it simply
    // refuses — either way the cost of asking is one round trip.
    for seed in &seeds {
        if let Err(e) = net.reserve_relay(seed.clone()).await {
            tracing::debug!(%seed, error = %e, "no relay reservation from this seed");
        }
    }

    let mut runtime = Runtime::new(identity, chain, sessions, store, pickle_key);

    // Startup is the right moment to notice a depleted key supply, before
    // anybody tries to open a conversation with us.
    if let Err(e) = runtime.maintain_prekeys() {
        tracing::warn!(error = %e, "could not replenish one-time keys at startup");
    }
    runtime.sweep();

    // Created up front: a file arriving is not the moment to discover that the
    // directory it goes in cannot be made.
    let files = data_dir.join("files");
    std::fs::create_dir_all(&files).map_err(fail)?;

    // The event pump is started by `run` once the runtime is shared.
    EVENTS.with(|slot| *slot.borrow_mut() = Some(events));
    Ok(Ctx {
        shared: Arc::new(Mutex::new(runtime)),
        net,
        files: Arc::new(files),
    })
}

thread_local! {
    /// Handoff for the event receiver between `build_runtime` and `run`, which
    /// both execute on the setup thread.
    static EVENTS: std::cell::RefCell<Option<tokio::sync::mpsc::Receiver<NetEvent>>> =
        const { std::cell::RefCell::new(None) };
}

/// Read bootstrap addresses from `seeds.json`: a plain array of multiaddrs.
/// A missing or malformed file is not an error — it means LAN-only operation,
/// which is a perfectly good way to run this.
fn read_seeds(path: &std::path::Path) -> Vec<vega_net::Multiaddr> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let parsed: Vec<String> = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "seeds.json is not a list of addresses; ignoring it");
            return Vec::new();
        }
    };
    parsed
        .iter()
        .filter_map(|s| match s.parse() {
            Ok(addr) => Some(addr),
            Err(e) => {
                tracing::warn!(seed = %s, error = %e, "skipping malformed seed address");
                None
            }
        })
        .collect()
}

fn hostname_label() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "this device".to_string())
}

/// Hand every queued envelope to the network.
///
/// The lock is taken twice, briefly, and never across a network call: a peer
/// that takes the full request timeout to answer delays this loop, not the UI
/// and not incoming messages.
async fn flush(ctx: &Ctx) -> usize {
    let batch = { ctx.shared.lock().await.pending_deliveries() };
    if batch.is_empty() {
        return 0;
    }

    let mut delivered = Vec::new();
    for item in batch {
        let mut accepted = false;
        for peer in &item.targets {
            if ctx.net.deliver(*peer, item.envelope.clone()).await.is_ok() {
                delivered.push(item.seq);
                accepted = true;
                break;
            }
        }
        if !accepted {
            // Stays queued. The retry loop will try again, and may climb a tier.
            tracing::debug!(
                to = %item.to_account,
                tried = item.targets.len(),
                "no peer accepted this envelope"
            );
        }
    }

    let count = delivered.len();
    if count > 0 {
        ctx.shared.lock().await.mark_delivered(&delivered);
    }
    count
}

/// Climb to Tier 2 for contacts we have mail for but no way to reach.
async fn locate_stranded(ctx: &Ctx) {
    let accounts = { ctx.shared.lock().await.stranded_accounts() };

    for account in accounts {
        let plans = { ctx.shared.lock().await.lookup_plan(&account) };
        for plan in plans {
            let Ok(values) = ctx.net.lookup(plan.key).await else {
                continue;
            };
            for value in values {
                let found = { ctx.shared.lock().await.absorb_record(&plan, &value) };
                if let Some((peer, addrs)) = found {
                    for addr in addrs {
                        let _ = ctx.net.dial(vega_net::with_peer(addr, peer)).await;
                    }
                    break;
                }
            }
        }
    }
}

/// Publish where we can be reached, one record per contact.
async fn announce(ctx: &Ctx) -> usize {
    let (Ok(peer_id), Ok(addrs)) = (ctx.net.local_peer_id().await, ctx.net.listeners().await)
    else {
        return 0;
    };

    let records = { ctx.shared.lock().await.announce_records(&peer_id, &addrs) };

    let mut published = 0;
    for (key, value) in records {
        if ctx.net.publish(key, value).await.is_ok() {
            published += 1;
        }
    }
    published
}

/// Drain network events: open what is ours, tell the UI, retry the outbox.
async fn pump(app: AppHandle, ctx: Ctx, mut events: tokio::sync::mpsc::Receiver<NetEvent>) {
    while let Some(event) = events.recv().await {
        // Scoped so the lock is gone before anything below touches the network.
        let (connected, decrypted, arrived) = {
            let mut rt = ctx.shared.lock().await;
            rt.on_net_event(&event);
            let (decrypted, arrived) = match &event {
                NetEvent::Envelope { bytes, .. } => match rt.receive(bytes) {
                    Received::Message(conversation) => (Some(conversation), None),
                    Received::File {
                        conversation,
                        transfer,
                        name,
                        bytes,
                    } => (Some(conversation), Some((transfer, name, bytes))),
                    _ => (None, None),
                },
                _ => (None, None),
            };
            (rt.connected_count(), decrypted, arrived)
        };

        // Written with the lock released: a ten-megabyte write must not hold up
        // decryption of everything behind it.
        if let Some((transfer, name, bytes)) = arrived {
            match write_file(&ctx.files, &transfer, &name, &bytes) {
                Ok(path) => tracing::info!(path = %path.display(), "saved a file"),
                // The transfer is gone from the store either way, so this file
                // is lost rather than resumable. Saying so is all that is left.
                Err(e) => tracing::error!(error = %e, %name, "could not save a received file"),
            }
        }

        if let Some(conversation) = decrypted {
            let _ = app.emit(EVT_MESSAGE, conversation.to_display());
        }

        match &event {
            NetEvent::PeerConnected(_) => {
                let _ = app.emit(EVT_NETWORK, connected);
                // A new peer is the first chance to deliver anything stuck.
                flush(&ctx).await;
            }
            NetEvent::PeerDisconnected(_) => {
                let _ = app.emit(EVT_NETWORK, connected);
            }
            NetEvent::Listening(addr) => tracing::info!(%addr, "listening"),
            NetEvent::ExternalAddress(addr) => {
                tracing::info!(%addr, "reachable from outside");
                announce(&ctx).await;
            }
            NetEvent::RelayReserved(peer) => tracing::info!(%peer, "relay reservation accepted"),
            NetEvent::HolePunched(peer) => tracing::info!(%peer, "upgraded to a direct connection"),
            _ => {}
        }
    }
}

/// Retry anything the outbox still holds. Messages outlive connections, so
/// delivery has to be driven by a clock rather than by whoever is online now.
async fn retry_loop(ctx: Ctx) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(10));
    loop {
        ticker.tick().await;
        if flush(&ctx).await == 0 {
            // Nothing moved. If we have mail and no peers, go looking.
            locate_stranded(&ctx).await;
        }
    }
}

/// Slow housekeeping: trim the replay set, keep one-time keys stocked.
async fn upkeep_loop(ctx: Ctx) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(6 * 3600));
    loop {
        ticker.tick().await;
        let mut rt = ctx.shared.lock().await;
        rt.sweep();
        if let Err(e) = rt.maintain_prekeys() {
            tracing::warn!(error = %e, "could not replenish one-time keys");
        }
    }
}

/// Republish our address to each contact's rendezvous key.
///
/// Slower than the outbox on purpose: addresses change far less often than
/// messages are sent, and every announce costs one DHT write per contact.
/// Records live an hour, so republishing at twenty minutes leaves room for two
/// failures before a contact loses track of us.
async fn announce_loop(ctx: Ctx) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(20 * 60));
    loop {
        ticker.tick().await;
        let published = announce(&ctx).await;
        if published > 0 {
            tracing::debug!(published, "republished rendezvous records");
        }
    }
}

/// Stop a development build that was launched on its own, with an explanation.
///
/// A dev build does not contain the frontend: `tauri.conf.json` points the
/// window at the vite dev server instead, so running this binary directly —
/// `cargo run`, or `target/debug/vega-app` — starts the Rust half correctly,
/// prints its listening addresses, and then opens a window on "Could not
/// connect to localhost". That message names the symptom and not the cause,
/// and the fix is not guessable from it, so the port is checked up front and
/// the answer printed instead.
///
/// Release builds embed the frontend and never call this, which is why it is
/// behind `debug_assertions` rather than a runtime flag.
#[cfg(all(debug_assertions, not(mobile)))]
fn require_dev_server() {
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::{Duration, Instant};

    // Fixed rather than read from the config: vite.config.ts sets
    // `strictPort`, so this is the only port a dev server is ever on.
    const DEV_ADDR: &str = "localhost:1420";

    // `tauri dev` starts vite and this binary concurrently, and against a warm
    // cargo cache the binary wins often enough that checking the port once
    // would fail the dev loop at random — vite needs a few hundred
    // milliseconds that a 0.7s incremental build does not leave it. So this
    // waits rather than samples. The deadline is what stops it hanging forever
    // when there is genuinely no dev server coming.
    const DEADLINE: Duration = Duration::from_secs(20);

    let started = Instant::now();
    let mut announced = false;

    while started.elapsed() < DEADLINE {
        // `localhost` resolves to both ::1 and 127.0.0.1 on a dual-stack
        // machine while vite binds only one of them, so every candidate
        // address is tried before concluding that nothing is listening.
        let reachable = DEV_ADDR
            .to_socket_addrs()
            .map(|mut addrs| {
                addrs.any(|addr| {
                    TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
                })
            })
            .unwrap_or(false);

        if reachable {
            return;
        }

        // Only once the wait is long enough to look like a hang, so a normal
        // `./make dev` start stays silent.
        if !announced && started.elapsed() > Duration::from_secs(2) {
            eprintln!("Vega: waiting for the vite dev server on http://{DEV_ADDR} …");
            announced = true;
        }

        std::thread::sleep(Duration::from_millis(250));
    }

    eprintln!(
        "\n\
         Vega: nothing came up on http://{DEV_ADDR}, so this window would have\n\
         opened on \"Could not connect to localhost\".\n\
         \n\
         This is a development build. It does not contain the frontend — the\n\
         window loads it from the vite dev server — so starting this binary on\n\
         its own can never work, however many times it is tried.\n\
         \n\
         Run the whole stack, which starts vite first and then this binary:\n\
         \n    ./make dev\n\
         \n\
         Or build a real application, which embeds the frontend and needs no\n\
         dev server at all:\n\
         \n    ./make dist\n"
    );
    std::process::exit(1);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Before tracing is initialised, so the explanation is not buried under a
    // screen of listening addresses that are themselves working fine.
    #[cfg(all(debug_assertions, not(mobile)))]
    require_dev_server();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "vega_app=info,vega_net=info,vega_core=info".into()),
        )
        .init();

    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let handle = app.handle().clone();

            let (ctx, events) = tauri::async_runtime::block_on(async move {
                let ctx = build_ctx(data_dir).await?;
                let events = EVENTS
                    .with(|slot| slot.borrow_mut().take())
                    .ok_or("network event channel went missing during startup")?;
                Ok::<_, String>((ctx, events))
            })
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

            app.manage(ctx.clone());
            tauri::async_runtime::spawn(pump(handle, ctx.clone(), events));
            tauri::async_runtime::spawn(announce_loop(ctx.clone()));
            tauri::async_runtime::spawn(upkeep_loop(ctx.clone()));
            tauri::async_runtime::spawn(retry_loop(ctx));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            me,
            my_invite,
            add_contact,
            list_contacts,
            send_message,
            send_file,
            conversation,
            network,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Vega");
}
