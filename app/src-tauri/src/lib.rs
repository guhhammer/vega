//! The Tauri shell: commands the UI calls, and the tasks that keep the node running.

mod invite;
mod keystore;
mod runtime;
mod words;

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
    /// The device key, for the files beside the database. The database holds its
    /// own copy; this is the same key.
    key: [u8; 32],
}

/// Where a transfer's bytes live once they are whole.
///
/// One directory per transfer id, and the file inside is always called `blob`.
/// The name the sender chose is kept in the message instead, which is encrypted
/// — so a directory listing of received files says how many there are and
/// nothing about what they are. It also means no part of this path comes from a
/// peer, and there is no longer anything here to sanitise.
fn transfer_dir(root: &std::path::Path, transfer: &[u8; 32]) -> std::path::PathBuf {
    root.join(data_encoding::HEXLOWER.encode(transfer))
}

/// The file itself, encrypted with the device key.
fn blob_path(root: &std::path::Path, transfer: &[u8; 32]) -> std::path::PathBuf {
    transfer_dir(root, transfer).join("blob")
}

/// What kind of image the blob holds, written beside it so that listing a
/// conversation does not have to decrypt every file to find out.
fn kind_path(root: &std::path::Path, transfer: &[u8; 32]) -> std::path::PathBuf {
    transfer_dir(root, transfer).join("kind")
}

/// Delete everything a finished transfer left on disk.
///
/// Failure is logged rather than raised: the history it belonged to is already
/// gone, and telling someone their chat could not be cleared because of a
/// leftover directory would be both alarming and useless.
fn remove_transfer_dir(root: &std::path::Path, transfer: &[u8; 32]) {
    let dir = transfer_dir(root, transfer);
    if let Err(e) = std::fs::remove_dir_all(&dir) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(error = %e, path = %dir.display(), "could not delete a file");
        }
    }
}

/// The image formats Vega will render, recognised by their first bytes.
///
/// The file name is never consulted. An extension is the sender's word, and
/// deciding how to treat a file by what it claims to be is how a document ends
/// up rendered as something it is not.
///
/// SVG is deliberately absent. It is a scriptable document rather than a bitmap,
/// and there is no reason to accept one from a contact and hand it to a browser
/// engine. The four here are all decoded by WebKit, WebView2 and WKWebView.
fn image_mime(head: &[u8]) -> Option<&'static str> {
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if head.starts_with(PNG) {
        return Some("image/png");
    }
    // Start of Image, then the first marker. Enough: every JPEG begins with it.
    if head.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    // RIFF container, four bytes of length, then the form type.
    if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// What a stored transfer is, according to the note left beside it.
///
/// The blob is one sealed unit, so there is no reading its first bytes without
/// decrypting all of it — which listing a conversation must not do for every
/// file in it. The answer is recorded at the moment the file is written, when
/// the plaintext is in hand anyway.
fn stored_kind(root: &std::path::Path, key: &[u8; 32], transfer: &[u8; 32]) -> Option<String> {
    let sealed = std::fs::read(kind_path(root, transfer)).ok()?;
    let plain = vega_core::at_rest::open(key, vega_core::at_rest::purpose::FILE, &sealed).ok()?;
    String::from_utf8(plain).ok()
}

/// What this device calls itself: the local override if one was set, otherwise
/// the label the identity was created with.
fn device_label(rt: &Runtime) -> String {
    rt.store
        .device_label()
        .ok()
        .flatten()
        .unwrap_or_else(|| rt.identity.label.clone())
}

/// Store a completed file, encrypted with the device key.
///
/// Nothing readable reaches the disk: not the contents, and not the name. What
/// is written beside it is the image type, if it is one, so that showing a
/// conversation does not mean decrypting every file in it.
fn write_file(
    root: &std::path::Path,
    key: &[u8; 32],
    transfer: &[u8; 32],
    bytes: &[u8],
) -> std::io::Result<()> {
    use vega_core::at_rest::{purpose, seal};
    let io = |e: vega_core::Error| std::io::Error::other(e.to_string());

    std::fs::create_dir_all(transfer_dir(root, transfer))?;
    std::fs::write(
        blob_path(root, transfer),
        seal(key, purpose::FILE, bytes).map_err(io)?,
    )?;

    if let Some(mime) = image_mime(bytes) {
        std::fs::write(
            kind_path(root, transfer),
            seal(key, purpose::FILE, mime.as_bytes()).map_err(io)?,
        )?;
    }
    Ok(())
}

/// Read a stored file back, decrypted.
fn read_file(
    root: &std::path::Path,
    key: &[u8; 32],
    transfer: &[u8; 32],
) -> Result<Vec<u8>, String> {
    let sealed = std::fs::read(blob_path(root, transfer))
        .map_err(|e| format!("that file is not readable: {e}"))?;
    vega_core::at_rest::open(key, vega_core::at_rest::purpose::FILE, &sealed)
        .map_err(|_| "that file does not open with this device's key".to_string())
}

// ---------------------------------------------------------------- views

#[derive(Debug, Serialize)]
pub struct MeView {
    account_id: String,
    short_id: String,
    device_label: String,
    device_id: String,
    /// This account's own fingerprint as words, to read out while handing an
    /// invite over. Fifty-two characters of base32 is not something anyone says
    /// down a phone line, and an id nobody checks is an id anybody can swap.
    identity_words: String,
}

#[derive(Debug, Serialize)]
pub struct ContactView {
    account_id: String,
    short_id: String,
    display_name: String,
    verified: bool,
    safety_number: String,
    /// The same fingerprint as words. Easier to read to somebody than digits,
    /// and the check people actually perform is the one they will do.
    safety_words: String,
    /// Their own phrase, independent of ours — the one they read out when they
    /// sent the invite. Kept so it can still be compared after the fact.
    identity_words: String,
    /// Messages that arrived since this conversation was last on screen.
    unread: usize,
}

/// What an invite claims, before anything is saved.
#[derive(Debug, Serialize)]
pub struct InvitePreviewView {
    account_id: String,
    short_id: String,
    display_name: String,
    identity_words: String,
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
    /// Whether the whole file is here. There is no path to offer: what is on
    /// disk is ciphertext, and `export_file` is how a usable copy is made.
    ready: bool,
    /// The image type, if the stored file is one Vega will render. `None` for
    /// everything else, which the interface shows as a plain file.
    image: Option<String>,
    /// Chunks held out of chunks expected — the progress bar, and the reason
    /// this view is computed per read rather than stored with the message.
    have: u32,
    chunks: u32,
    /// Names the file for [`read_image`]. Hex, because it crosses into JSON.
    transfer: String,
}

/// An image handed to the web view, ready to drop into a `src`.
#[derive(Debug, Serialize)]
pub struct ImageData {
    mime: &'static str,
    /// The file, base64. Not a path: the web view has no filesystem access, and
    /// this is what keeps it that way.
    data: String,
}

/// A group, as the interface lists it.
#[derive(Debug, Serialize)]
pub struct GroupView {
    /// The thread key — `group:<id>` — the same string every thread command takes.
    id: String,
    name: String,
    /// Who may change the membership. Everyone else may only leave.
    creator: String,
    /// True when that creator is me, which is what the interface uses to decide
    /// whether to offer the buttons at all.
    mine: bool,
    members: Vec<GroupMemberView>,
    /// We are no longer in it. The thread stays readable and stops accepting.
    departed: bool,
    unread: usize,
}

/// One member of a group.
#[derive(Debug, Serialize)]
pub struct GroupMemberView {
    account_id: String,
    display_name: String,
    /// This is me.
    self_: bool,
    /// Not in my contacts, so I cannot send to them and they see nothing I
    /// write. The creator knows them; I do not. Shown rather than hidden,
    /// because a member list that quietly omits people is a lie about who is
    /// reading.
    unreachable: bool,
}

/// One conversation, as the history screen lists it.
#[derive(Debug, Serialize)]
pub struct HistoryView {
    /// The thread key: a contact's account id, or `group:<id>`.
    account_id: String,
    display_name: String,
    group: bool,
    messages: usize,
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
        device_label: device_label(&rt),
        device_id: rt.identity.device_id.short(),
        identity_words: invite::Invite::identity_words(&rt.identity.account_id),
    })
}

#[tauri::command]
async fn my_invite(display_name: String, ctx: State<'_, Ctx>) -> Result<String, String> {
    let rt = ctx.shared.lock().await;
    rt.my_invite(&display_name).map_err(fail)
}

/// Say who an invite claims to be, without saving anything.
///
/// This exists for the phrase. Whoever sent the invite read ten words out; this
/// is where the receiver sees the ten words computed from what actually
/// arrived, and reads them back before pressing Add. Two phrases that differ
/// mean the invite was swapped on the way, which is the one attack a manual
/// exchange has to be able to catch.
///
/// Nothing here is taken on trust: `decode` verifies the sigchain and refuses
/// an invite whose account id, contact key or device roster disagree with it,
/// exactly as it does for `add_contact`. A preview that cannot be decoded is an
/// error, so a malformed invite is refused here rather than after it is saved.
#[tauri::command]
async fn preview_invite(invite: String) -> Result<InvitePreviewView, String> {
    let decoded = invite::Invite::decode(&invite).map_err(fail)?;
    Ok(InvitePreviewView {
        account_id: decoded.account_id.to_display(),
        short_id: decoded.account_id.short(),
        display_name: decoded.display_name,
        identity_words: invite::Invite::identity_words(&decoded.account_id),
    })
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
    Ok(view_of(&contact, &rt))
}

#[tauri::command]
async fn list_contacts(ctx: State<'_, Ctx>) -> Result<Vec<ContactView>, String> {
    let rt = ctx.shared.lock().await;
    Ok(rt
        .store
        .list_contacts()
        .map_err(fail)?
        .iter()
        .map(|c| view_of(c, &rt))
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

    // The sender's own copy, stored exactly as the recipient will store theirs,
    // so a file looks the same in both threads.
    if let Err(e) = write_file(&ctx.files, &ctx.key, &transfer, &bytes) {
        // Queued and on its way regardless; only our local copy is missing.
        tracing::warn!(error = %e, "could not keep a local copy of a sent file");
    }

    flush(&ctx).await;
    Ok(())
}

/// Rename a contact, for this installation only.
///
/// The name a contact arrives with is the one they wrote in their own invite.
/// This replaces it locally and tells nobody: the account id is what identifies
/// them on the wire, and it does not change.
#[tauri::command]
async fn rename_contact(
    account: String,
    name: String,
    ctx: State<'_, Ctx>,
) -> Result<ContactView, String> {
    let id = AccountId::from_display(&account).map_err(fail)?;
    let rt = ctx.shared.lock().await;
    let mut contact = rt
        .store
        .get_contact(&id)
        .map_err(fail)?
        .ok_or("no such contact")?;

    // An empty name is allowed: it means "drop the name I gave them". The
    // invite that carried their own name is not kept, so what the interface
    // falls back to is their short id — which is at least true.
    contact.display_name = name.trim().to_string();
    rt.store.put_contact(&contact).map_err(fail)?;
    Ok(view_of(&contact, &rt))
}

/// Move a conversation's read marker up to whatever is in it now.
///
/// Called when the conversation is on screen, which is the only thing this
/// program can honestly observe about reading. It is deliberately its own
/// command rather than a side effect of [`conversation`]: a query that quietly
/// writes is a query nobody can call twice with confidence, and the interface
/// legitimately re-reads a thread — on a reload, on a window that was left
/// open — at moments that are not somebody sitting down to read it.
///
/// Returns how many are still unread, which is always zero here, so the caller
/// can settle the badge without a second round trip.
#[tauri::command]
async fn mark_read(with: String, ctx: State<'_, Ctx>) -> Result<usize, String> {
    let which = thread_of(&with)?;
    let rt = ctx.shared.lock().await;

    // Never backwards: a marker that moved down would resurrect messages that
    // have been read, and `last_seq` is zero for a thread with no history.
    let last = rt.store.last_seq(&which).map_err(fail)?;

    let read_seq = match which {
        vega_core::Conversation::Direct(account) => {
            let Some(mut contact) = rt.store.get_contact(&account).map_err(fail)? else {
                // A conversation with somebody already deleted. Nothing to
                // mark, and nothing worth interrupting the interface over.
                return Ok(0);
            };
            if last > contact.read_seq {
                contact.read_seq = last;
                rt.store.put_contact(&contact).map_err(fail)?;
            }
            contact.read_seq
        }
        vega_core::Conversation::Group(id) => {
            let Some(mut group) = rt.store.get_group(&id).map_err(fail)? else {
                return Ok(0);
            };
            if last > group.read_seq {
                group.read_seq = last;
                rt.store.put_group(&group).map_err(fail)?;
            }
            group.read_seq
        }
    };

    Ok(rt.store.unread(&which, read_seq).unwrap_or_default())
}

/// Record that the safety words were compared, and how it went.
///
/// The comparison itself happens on a call or across a table — no program can
/// do it, and no program should claim to. All this does is remember the answer,
/// so the interface can stop asking a question that has been settled and say
/// plainly which conversations have been checked and which have not.
///
/// Reversible on purpose. A contact who adds a new device, or a phrase that
/// turns out to have been compared carelessly, should be able to go back to
/// unverified rather than leave a reassuring mark nobody earned.
#[tauri::command]
async fn set_verified(
    account: String,
    verified: bool,
    ctx: State<'_, Ctx>,
) -> Result<ContactView, String> {
    let id = AccountId::from_display(&account).map_err(fail)?;
    let rt = ctx.shared.lock().await;
    let mut contact = rt
        .store
        .get_contact(&id)
        .map_err(fail)?
        .ok_or("no such contact")?;

    contact.verified = verified;
    rt.store.put_contact(&contact).map_err(fail)?;
    Ok(view_of(&contact, &rt))
}

/// Rename this device, for this installation only.
///
/// The label inside the sigchain is signed and cannot be edited after the fact.
/// This is a local override; no contact ever learns it.
#[tauri::command]
async fn rename_device(name: String, ctx: State<'_, Ctx>) -> Result<String, String> {
    let rt = ctx.shared.lock().await;
    rt.store.set_device_label(name.trim()).map_err(fail)?;
    Ok(device_label(&rt))
}

/// Delete one conversation's history, and any files that came with it.
#[tauri::command]
async fn clear_chat(with: String, ctx: State<'_, Ctx>) -> Result<usize, String> {
    let which = thread_of(&with)?;
    let (removed, transfers) = {
        let rt = ctx.shared.lock().await;
        // Collected before the messages go, since the manifests are what name
        // the files on disk.
        let transfers: Vec<[u8; 32]> = rt
            .store
            .conversation(&which, usize::MAX)
            .map_err(fail)?
            .iter()
            .filter_map(|m| m.content.file().map(|f| f.transfer))
            .collect();
        (
            rt.store.clear_conversation(&which).map_err(fail)?,
            transfers,
        )
    };

    for transfer in transfers {
        remove_transfer_dir(&ctx.files, &transfer);
    }
    Ok(removed)
}

/// Delete every conversation, and every file received or sent.
#[tauri::command]
async fn clear_all_history(ctx: State<'_, Ctx>) -> Result<usize, String> {
    let removed = {
        let rt = ctx.shared.lock().await;
        rt.store.clear_all_messages().map_err(fail)?
    };

    // The whole directory rather than transfer by transfer: after this there is
    // no message left that could name one, so anything still in there is
    // unreachable by definition.
    if let Err(e) = std::fs::remove_dir_all(ctx.files.as_path()) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(error = %e, "could not delete received files");
        }
    }
    if let Err(e) = std::fs::create_dir_all(ctx.files.as_path()) {
        tracing::warn!(error = %e, "could not recreate the files directory");
    }
    Ok(removed)
}

/// Read one received image back off disk, for the view to display.
///
/// Bytes rather than a path, because the web view is not allowed to read the
/// filesystem and this is the point of that arrangement: it can ask for a file
/// Vega already accepted, by the transfer id Vega gave it, and nothing else.
/// `name` is re-sanitised on the way through `file_path`, so no combination of
/// arguments can address anything outside a transfer's own directory.
///
/// The sniff is repeated here rather than trusted from the listing: this is the
/// call that decides what a browser engine is asked to decode.
#[tauri::command]
async fn read_image(transfer: String, ctx: State<'_, Ctx>) -> Result<ImageData, String> {
    let id = transfer_id(&transfer)?;
    let bytes = read_file(&ctx.files, &ctx.key, &id)?;

    let mime = image_mime(&bytes).ok_or("that file is not an image Vega can show")?;
    Ok(ImageData {
        mime,
        data: data_encoding::BASE64.encode(&bytes),
    })
}

/// Write a usable copy of a received file into the downloads folder.
///
/// What Vega keeps is encrypted, which is the point — and also means it is not a
/// file anything else can open. This is the way out: one decrypted copy, put
/// where downloads go, under the name it was sent with. From that moment it is
/// an ordinary file with ordinary protection, and the interface says so.
#[tauri::command]
async fn export_file(
    transfer: String,
    name: String,
    app: AppHandle,
    ctx: State<'_, Ctx>,
) -> Result<String, String> {
    let id = transfer_id(&transfer)?;
    let bytes = read_file(&ctx.files, &ctx.key, &id)?;

    // The downloads folder if the platform has one; the data directory is a
    // poor second choice but better than refusing to hand the file over.
    let dir = app
        .path()
        .download_dir()
        .unwrap_or_else(|_| ctx.files.as_path().to_path_buf());
    std::fs::create_dir_all(&dir).map_err(fail)?;

    // The name is a peer's, so it is reduced to one harmless component before it
    // is joined to a directory this program did not choose.
    let safe = vega_core::safe_file_name(&name);
    let mut path = dir.join(&safe);

    // Never overwrite what is already there. This writes outside Vega's own
    // directory, where things it did not put there live.
    if path.exists() {
        let as_path = std::path::Path::new(&safe);
        let stem = as_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".into());
        let ext = as_path
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        for n in 2..1000 {
            let candidate = dir.join(format!("{stem} ({n}){ext}"));
            if !candidate.exists() {
                path = candidate;
                break;
            }
        }
    }

    std::fs::write(&path, &bytes).map_err(fail)?;
    Ok(path.display().to_string())
}

/// Prefix marking a group in the one string the interface uses to name a thread.
///
/// The interface has exactly one notion of "which thread is open", and it is
/// this string. An account's display form is base32 with dashes and can never
/// begin with it, so the two spaces do not overlap.
const THREAD_GROUP_PREFIX: &str = "group:";

/// The string the interface uses to name a thread.
fn thread_key(which: &vega_core::Conversation) -> String {
    match which {
        vega_core::Conversation::Direct(account) => account.to_display(),
        vega_core::Conversation::Group(group) => {
            format!("{THREAD_GROUP_PREFIX}{}", group.to_display())
        }
    }
}

/// Parse one back. Anything unparseable is the interface's bug, not a peer's.
fn thread_of(key: &str) -> Result<vega_core::Conversation, String> {
    match key.strip_prefix(THREAD_GROUP_PREFIX) {
        Some(rest) => vega_core::GroupId::from_display(rest)
            .map(vega_core::Conversation::Group)
            .map_err(fail),
        None => AccountId::from_display(key)
            .map(vega_core::Conversation::Direct)
            .map_err(fail),
    }
}

/// A group id from the string the interface holds, with or without the prefix.
fn group_id_of(key: &str) -> Result<vega_core::GroupId, String> {
    vega_core::GroupId::from_display(key.strip_prefix(THREAD_GROUP_PREFIX).unwrap_or(key))
        .map_err(fail)
}

/// A transfer id from the hex the interface was given.
fn transfer_id(hex: &str) -> Result<[u8; 32], String> {
    data_encoding::HEXLOWER
        .decode(hex.as_bytes())
        .ok()
        .and_then(|raw| <[u8; 32]>::try_from(raw).ok())
        .ok_or_else(|| "not a transfer id".to_string())
}

/// What each conversation holds, for the screen that offers to clear them.
#[tauri::command]
async fn history(ctx: State<'_, Ctx>) -> Result<Vec<HistoryView>, String> {
    let rt = ctx.shared.lock().await;
    let counts = rt.store.message_counts().map_err(fail)?;
    Ok(counts
        .into_iter()
        .map(|(which, messages)| {
            let name = match which {
                vega_core::Conversation::Direct(account) => rt
                    .store
                    .get_contact(&account)
                    .ok()
                    .flatten()
                    .map(|c| c.display_name)
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| account.short()),
                vega_core::Conversation::Group(group) => rt
                    .store
                    .get_group(&group)
                    .ok()
                    .flatten()
                    .map(|g| g.name)
                    .unwrap_or_else(|| group.short()),
            };
            HistoryView {
                account_id: thread_key(&which),
                display_name: name,
                group: which.as_group().is_some(),
                messages,
            }
        })
        .collect())
}

#[tauri::command]
async fn conversation(with: String, ctx: State<'_, Ctx>) -> Result<Vec<MessageView>, String> {
    let which = thread_of(&with)?;
    let rt = ctx.shared.lock().await;
    Ok(rt
        .store
        .conversation(&which, 500)
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
                // Progress is read here rather than stored on the message,
                // because it changes without the message changing. A transfer
                // marked done, or pruned away entirely, is finished — and in
                // both cases the blob on disk is what actually settles it.
                let (have, ready) = match rt.store.get_transfer(&info.transfer) {
                    Ok(Some(open)) if !open.done => (open.have, false),
                    _ => {
                        let there = blob_path(&ctx.files, &info.transfer).is_file();
                        (if there { info.chunks } else { 0 }, there)
                    }
                };
                return Some(common(
                    String::new(),
                    Some(FileView {
                        name: info.name.to_string(),
                        size: info.size,
                        // Recorded when the file was written, from its bytes
                        // rather than its name.
                        image: ready
                            .then(|| stored_kind(&ctx.files, &ctx.key, &info.transfer))
                            .flatten(),
                        ready,
                        have,
                        chunks: info.chunks,
                        transfer: data_encoding::HEXLOWER.encode(&info.transfer),
                    }),
                ));
            }

            m.content.text().map(|t| common(t.to_string(), None))
        })
        .collect())
}

// ---------------------------------------------------------------- groups

/// Start a group.
///
/// The people named have to be contacts already: there is no directory to look
/// anyone up in, and a group is not a way around exchanging invites. Everyone
/// named is told about it immediately, which is also how they learn they are in
/// it — there is no invitation to accept, for the same reason there is no
/// server to hold one.
///
/// Returns the new thread's key, and anybody it could not be delivered to.
#[tauri::command]
async fn create_group(
    name: String,
    members: Vec<String>,
    ctx: State<'_, Ctx>,
) -> Result<GroupView, String> {
    let members: Result<Vec<AccountId>, String> = members
        .iter()
        .map(|m| AccountId::from_display(m).map_err(fail))
        .collect();
    let members = members?;

    let mut rt = ctx.shared.lock().await;
    for member in &members {
        if rt.store.get_contact(member).map_err(fail)?.is_none() {
            return Err("everybody in a group has to be one of your contacts first".into());
        }
    }

    let (id, _unreachable) = rt.create_group(&name, &members).map_err(fail)?;
    let group = rt.store.get_group(&id).map_err(fail)?.ok_or("no group")?;
    Ok(group_view(&group, &rt))
}

#[tauri::command]
async fn list_groups(ctx: State<'_, Ctx>) -> Result<Vec<GroupView>, String> {
    let rt = ctx.shared.lock().await;
    let mut groups = rt.store.list_groups().map_err(fail)?;
    // Newest first, which is the order somebody who just made one expects.
    groups.sort_by_key(|g| std::cmp::Reverse(g.created_at));
    Ok(groups.iter().map(|g| group_view(g, &rt)).collect())
}

#[tauri::command]
async fn send_group_message(
    group: String,
    text: String,
    ctx: State<'_, Ctx>,
) -> Result<Vec<String>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("nothing to send".into());
    }
    let id = group_id_of(&group)?;
    let unreachable = {
        let mut rt = ctx.shared.lock().await;
        rt.send_group_text(&id, text).map_err(fail)?
    };
    flush(&ctx).await;
    Ok(unreachable.iter().map(|a| a.to_display()).collect())
}

/// Add somebody. Creator only — the runtime refuses otherwise.
#[tauri::command]
async fn add_to_group(
    group: String,
    account: String,
    ctx: State<'_, Ctx>,
) -> Result<GroupView, String> {
    let id = group_id_of(&group)?;
    let who = AccountId::from_display(&account).map_err(fail)?;

    let view = {
        let mut rt = ctx.shared.lock().await;
        if rt.store.get_contact(&who).map_err(fail)?.is_none() {
            return Err("add them as a contact first".into());
        }
        rt.add_to_group(&id, who).map_err(fail)?;
        let group = rt.store.get_group(&id).map_err(fail)?.ok_or("no group")?;
        group_view(&group, &rt)
    };
    flush(&ctx).await;
    Ok(view)
}

#[tauri::command]
async fn remove_from_group(
    group: String,
    account: String,
    ctx: State<'_, Ctx>,
) -> Result<GroupView, String> {
    let id = group_id_of(&group)?;
    let who = AccountId::from_display(&account).map_err(fail)?;

    let view = {
        let mut rt = ctx.shared.lock().await;
        rt.remove_from_group(&id, who).map_err(fail)?;
        let group = rt.store.get_group(&id).map_err(fail)?.ok_or("no group")?;
        group_view(&group, &rt)
    };
    flush(&ctx).await;
    Ok(view)
}

#[tauri::command]
async fn rename_group(
    group: String,
    name: String,
    ctx: State<'_, Ctx>,
) -> Result<GroupView, String> {
    let id = group_id_of(&group)?;
    let view = {
        let mut rt = ctx.shared.lock().await;
        rt.rename_group(&id, &name).map_err(fail)?;
        let group = rt.store.get_group(&id).map_err(fail)?.ok_or("no group")?;
        group_view(&group, &rt)
    };
    flush(&ctx).await;
    Ok(view)
}

/// Leave a group, telling the others on the way out.
///
/// The thread stays; what ends is being able to send to it. Deleting it is
/// [`delete_group`], and deliberately a separate decision.
#[tauri::command]
async fn leave_group(group: String, ctx: State<'_, Ctx>) -> Result<GroupView, String> {
    let id = group_id_of(&group)?;
    let view = {
        let mut rt = ctx.shared.lock().await;
        rt.leave_group(&id).map_err(fail)?;
        let group = rt.store.get_group(&id).map_err(fail)?.ok_or("no group")?;
        group_view(&group, &rt)
    };
    flush(&ctx).await;
    Ok(view)
}

/// Forget a group and everything said in it, on this device.
///
/// Local. The others keep their copy of the conversation, and nothing tells them
/// this happened — the same as clearing a one-to-one history.
#[tauri::command]
async fn delete_group(group: String, ctx: State<'_, Ctx>) -> Result<usize, String> {
    let id = group_id_of(&group)?;
    let rt = ctx.shared.lock().await;
    rt.store.drop_group(&id).map_err(fail)
}

fn group_view(group: &vega_core::Group, rt: &Runtime) -> GroupView {
    let me = rt.identity.account_id;
    GroupView {
        id: thread_key(&vega_core::Conversation::Group(group.id)),
        name: group.name.clone(),
        creator: group.creator.to_display(),
        mine: group.creator == me,
        members: group
            .members
            .iter()
            .map(|account| {
                let contact = rt.store.get_contact(account).ok().flatten();
                GroupMemberView {
                    account_id: account.to_display(),
                    display_name: match (&contact, *account == me) {
                        (_, true) => "You".to_string(),
                        (Some(c), _) if !c.display_name.is_empty() => c.display_name.clone(),
                        _ => account.short(),
                    },
                    self_: *account == me,
                    unreachable: contact.is_none() && *account != me,
                }
            })
            .collect(),
        departed: group.departed,
        unread: rt
            .store
            .unread(&vega_core::Conversation::Group(group.id), group.read_seq)
            .unwrap_or_default(),
    }
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

fn view_of(c: &vega_core::Contact, rt: &Runtime) -> ContactView {
    let me = &rt.identity.account_id;
    ContactView {
        account_id: c.account_id.to_display(),
        short_id: c.account_id.short(),
        display_name: c.display_name.clone(),
        verified: c.verified,
        safety_number: invite::Invite::safety_number(me, &c.account_id),
        safety_words: invite::Invite::safety_words(me, &c.account_id),
        identity_words: invite::Invite::identity_words(&c.account_id),
        // A count that cannot be read is shown as nothing rather than taken as
        // a reason to fail the whole contact list. The badge is the least
        // important thing on the row.
        unread: rt
            .store
            .unread(&c.account_id.into(), c.read_seq)
            .unwrap_or_default(),
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
    let store = Store::open(data_dir.join("vega.redb"), pickle_key).map_err(fail)?;

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
    // A phone takes from the network without carrying anyone else's traffic.
    // Taken whole rather than field by field: copying the fields that looked
    // relevant is how `kad_mode` stayed at its desktop default here long after
    // `mobile()` had an opinion about it, and every future field would inherit
    // the same bug.
    let config = if cfg!(target_os = "android") {
        NodeConfig::mobile()
    } else {
        NodeConfig::default()
    }
    .with_bootstrap(seeds.clone());

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
        key: pickle_key,
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

/// The name this device starts life with, before anyone renames it.
///
/// "some device" rather than "this device": the label is only ever read next to
/// others, and a list where every entry says *this* device names nothing. It is
/// also a prompt — a name that obviously wants replacing gets replaced.
fn hostname_label() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "some device".to_string())
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
                    } => (
                        Some(vega_core::Conversation::Direct(conversation)),
                        Some((transfer, name, bytes)),
                    ),
                    _ => (None, None),
                },
                _ => (None, None),
            };
            (rt.connected_count(), decrypted, arrived)
        };

        // Written with the lock released: a ten-megabyte write must not hold up
        // decryption of everything behind it.
        if let Some((transfer, name, bytes)) = arrived {
            match write_file(&ctx.files, &ctx.key, &transfer, &bytes) {
                Ok(()) => tracing::info!(%name, "saved a file"),
                // The transfer is gone from the store either way, so this file
                // is lost rather than resumable. Saying so is all that is left.
                Err(e) => tracing::error!(error = %e, %name, "could not save a received file"),
            }
        }

        if let Some(conversation) = decrypted {
            // The same string the interface passes back to `conversation`, so a
            // group thread and a contact thread refresh through one path.
            let _ = app.emit(EVT_MESSAGE, thread_key(&conversation));
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

/// Run-loop events that only Android has an opinion about.
///
/// Two things follow from a foreground service holding the process open after
/// the last activity is gone, and both are handled here rather than in the
/// plugin, because both are about Tauri's window rather than about Android.
#[cfg(target_os = "android")]
fn on_run_event(app: &AppHandle, event: tauri::RunEvent) {
    match event {
        // Without this the process dies with the last activity and the service
        // has nothing left to keep alive. On the desktop the same call would
        // mean the app could not be quit, which is why this is Android only.
        tauri::RunEvent::ExitRequested { api, .. } => api.prevent_exit(),

        tauri::RunEvent::Resumed => {
            // tauri-apps/tauri#15671: an activity recreated after the task was
            // swiped away gets a fresh id, Tauri finds no window filed under
            // it, and the app comes up blank. Open as of Tauri 2.11.5;
            // rebuilding the window is the workaround until it lands.
            if app.webview_windows().is_empty() {
                tracing::info!("resumed with no window — rebuilding it");
                if let Err(e) =
                    tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::default())
                        .build()
                {
                    tracing::error!(error = %e, "could not rebuild the window");
                }
            }

            // Idempotent, and the cheapest place to recover from a service the
            // system killed or the user stopped from the notification.
            if let Err(e) = vega_android::start(app, vega_android::Notice::default()) {
                tracing::debug!(error = %e, "foreground service did not restart on resume");
            }
        }

        _ => {}
    }
}

/// Nothing on any other platform: a desktop process is not killed for being in
/// the background, and preventing exit there would mean the app could not quit.
#[cfg(not(target_os = "android"))]
fn on_run_event(_app: &AppHandle, _event: tauri::RunEvent) {}

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
        // Registered on every platform; off Android it does nothing.
        .plugin(vega_android::init())
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

            // The node is up; ask Android to stop killing the process it runs
            // in. Not fatal if it fails — what is lost is background delivery,
            // which is where this was before the service was written.
            if let Err(e) = vega_android::start(app.handle(), vega_android::Notice::default()) {
                tracing::warn!(error = %e, "no foreground service: delivery stops when the app does");
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            me,
            my_invite,
            preview_invite,
            add_contact,
            list_contacts,
            send_message,
            send_file,
            read_image,
            export_file,
            rename_contact,
            rename_device,
            set_verified,
            mark_read,
            clear_chat,
            clear_all_history,
            history,
            conversation,
            network,
            create_group,
            list_groups,
            send_group_message,
            add_to_group,
            remove_from_group,
            rename_group,
            leave_group,
            delete_group,
        ])
        .build(tauri::generate_context!())
        .expect("error while starting Vega")
        .run(on_run_event);
}

#[cfg(test)]
mod tests {
    use super::{blob_path, image_mime, read_file, stored_kind, write_file};

    const KEY: [u8; 32] = [4u8; 32];

    /// A received file must not be legible to anyone reading the disk, and must
    /// still be legible to Vega.
    #[test]
    fn a_stored_file_is_encrypted_and_still_readable() {
        let dir = tempfile::tempdir().unwrap();
        let transfer = [0xaa; 32];
        let png = b"\x89PNG\r\n\x1a\nSECRET-PIXELS-DO-NOT-LEAK";

        write_file(dir.path(), &KEY, &transfer, png).unwrap();

        let on_disk = std::fs::read(blob_path(dir.path(), &transfer)).unwrap();
        assert!(
            !on_disk.windows(png.len()).any(|w| w == png),
            "the file is sitting on disk in the clear"
        );
        assert_eq!(read_file(dir.path(), &KEY, &transfer).unwrap(), png);

        // The kind is recorded so a listing need not decrypt, and it is sealed
        // like everything else.
        assert_eq!(
            stored_kind(dir.path(), &KEY, &transfer).as_deref(),
            Some("image/png")
        );
        let kind_raw = std::fs::read(super::kind_path(dir.path(), &transfer)).unwrap();
        assert!(!kind_raw.starts_with(b"image/"));

        // Another key opens neither.
        assert!(read_file(dir.path(), &[9u8; 32], &transfer).is_err());
        assert_eq!(stored_kind(dir.path(), &[9u8; 32], &transfer), None);
    }

    #[test]
    fn only_images_get_a_kind_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let transfer = [0xbb; 32];
        // Named like a picture elsewhere in the system, but these are the bytes
        // that decide, and they are a script.
        write_file(dir.path(), &KEY, &transfer, b"#!/bin/sh\nrm -rf /").unwrap();
        assert_eq!(stored_kind(dir.path(), &KEY, &transfer), None);
        assert!(read_file(dir.path(), &KEY, &transfer).is_ok());
    }

    #[test]
    fn images_are_recognised_by_their_bytes() {
        assert_eq!(
            image_mime(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0]),
            Some("image/png")
        );
        assert_eq!(image_mime(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
        assert_eq!(image_mime(b"GIF89a....."), Some("image/gif"));
        assert_eq!(image_mime(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
    }

    #[test]
    fn everything_else_is_a_plain_file() {
        // A name is not evidence, so these are what matters: things that would
        // like to be treated as an image and are not one.
        assert_eq!(
            image_mime(b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script/>"),
            None,
            "svg is a scriptable document, not a bitmap"
        );
        assert_eq!(image_mime(b"%PDF-1.7"), None);
        assert_eq!(image_mime(b"#!/bin/sh\nrm -rf /"), None);
        assert_eq!(
            image_mime(b"RIFF\0\0\0\0WAVEfmt "),
            None,
            "the same container, but audio"
        );
        assert_eq!(image_mime(b""), None);
        assert_eq!(
            image_mime(&[0x89, b'P']),
            None,
            "a truncated header is not a match"
        );
    }
}
