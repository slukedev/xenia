//! Sharing from other apps: the iOS share extension opens the signed-in session without syncing,
//! lists conversations from the stores, and sends what was shared to one of them.
//!
//! A share extension lives under a tight memory ceiling, and the SDK holds a file in memory (and
//! an encrypted copy of it) while it uploads. Anything too large to send from there is handed to
//! the app instead: copied into an outbox beside the stores, which the app sends through its send
//! queue the next time it starts or comes back. A share that fails partway can be tried again, or
//! handed over, without sending what already went a second time.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use eyeball::SharedObservable;
use matrix_sdk::{
    Client, Room, RoomState, TransmissionProgress,
    attachment::AttachmentConfig,
    ruma::{OwnedRoomId, RoomId, events::room::message::RoomMessageEventContent},
};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::{
    error::{Result, XeniaError},
    platform::{ClientConfig, SecretStore},
    rooms::{rules::matches_query, summary::room_name},
    runtime::run,
    session::{Process, SESSION_KEY, Server, StoredSession, build_client, lock_crypto_store},
    sync::MutexExt,
    text::compose_message,
    timeline::{Attachment, AttachmentParts, attachment_parts, message_content},
};

/// The largest file the share extension uploads itself. The SDK keeps the file and its encrypted
/// copy in memory while it uploads, and the extension has about 120 MB in all, most of it taken by
/// the client; anything larger goes to the app.
pub(crate) const EXTENSION_UPLOAD_LIMIT: u64 = 15 * 1024 * 1024;

/// What a text step weighs in the progress, against a file's bytes.
const TEXT_WEIGHT: u64 = 1024;

/// Bumped when the outbox's manifest changes shape; an entry from another version is left for
/// the build that wrote it.
const OUTBOX_VERSION: u32 = 1;

/// A conversation to share to.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ShareTarget {
    pub id: String,
    pub name: String,
    /// An `mxc://` URL: the room's photo, or the other person's for a chat.
    pub avatar_url: Option<String>,
    /// Picks the initials' color: the other person's ID for a chat, the room's otherwise.
    pub avatar_seed: String,
    /// A chat (drawn as a person) rather than a room.
    pub is_direct: bool,
    /// The other person in a one-to-one chat, for the system's share suggestions.
    pub dm_user_id: Option<String>,
    /// The latest activity, for ordering (0 when unknown).
    pub timestamp_ms: i64,
}

/// One thing shared from another app.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum SharedItem {
    /// Text or a link.
    Text { text: String },
    /// A photo, video or file, already copied somewhere the core can read.
    File { attachment: Attachment },
}

/// How a share ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ShareOutcome {
    /// Everything reached the server.
    Sent,
    /// What was left went to the app, which sends it the next time it opens.
    HandedOff,
}

/// Conversations whose name, address or other person matches `query`, in the order given.
#[uniffi::export]
pub fn filter_share_targets(targets: Vec<ShareTarget>, query: String) -> Vec<ShareTarget> {
    targets
        .into_iter()
        .filter(|t| matches_query(&query, &[&t.name, t.dm_user_id.as_deref().unwrap_or_default()]))
        .collect()
}

/// Whether the share extension can send these itself; if not, it hands them to the app, and the
/// sheet says so before anything is sent.
#[uniffi::export]
pub fn shares_from_extension(items: Vec<SharedItem>) -> bool {
    sends_in_extension(&with_sizes_on_disk(plan(items, "")))
}

/// The sizes the files have on disk, whatever the app guessed.
fn with_sizes_on_disk(steps: Vec<Step>) -> Vec<Step> {
    steps
        .into_iter()
        .map(|step| match step {
            Step::File(mut file) => {
                file.size = std::fs::metadata(&file.path).ok().map(|m| m.len()).or(file.size);
                Step::File(file)
            }
            text => text,
        })
        .collect()
}

/// Newest activity first, then by name, as the share sheet lists them.
pub(crate) fn order_targets(targets: &mut [ShareTarget]) {
    targets.sort_by(|a, b| b.timestamp_ms.cmp(&a.timestamp_ms).then_with(|| a.name.cmp(&b.name)));
}

/// One send in a share, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    Text(String),
    File(Attachment),
}

/// What a share sends: the comment and any shared text or links as one message, then each file.
/// A single file with nothing but a comment carries the comment as its caption, the way a photo
/// sent from the composer does.
pub(crate) fn plan(items: Vec<SharedItem>, comment: &str) -> Vec<Step> {
    let comment = comment.trim();
    let mut texts = Vec::new();
    let mut files = Vec::new();
    for item in items {
        match item {
            SharedItem::Text { text } => {
                let text = text.trim();
                // A link often arrives twice: as a URL and as the page's text.
                if !text.is_empty() && !texts.iter().any(|t: &String| t == text) {
                    texts.push(text.to_owned());
                }
            }
            SharedItem::File { mut attachment } => {
                attachment.caption = None;
                files.push(attachment);
            }
        }
    }
    if texts.is_empty() && files.len() == 1 && !comment.is_empty() {
        let mut file = files.remove(0);
        file.caption = Some(comment.to_owned());
        return vec![Step::File(file)];
    }
    let mut message: Vec<&str> = Vec::new();
    if !comment.is_empty() {
        message.push(comment);
    }
    message.extend(texts.iter().map(String::as_str));
    let mut steps = Vec::new();
    if !message.is_empty() {
        steps.push(Step::Text(message.join("\n")));
    }
    steps.extend(files.into_iter().map(Step::File));
    steps
}

/// Whether the extension can send every file itself; otherwise the whole share goes to the app,
/// so its parts arrive in order.
pub(crate) fn sends_in_extension(steps: &[Step]) -> bool {
    steps.iter().all(|s| match s {
        Step::Text(_) => true,
        Step::File(file) => file.size.is_some_and(|size| size <= EXTENSION_UPLOAD_LIMIT),
    })
}

fn weight(step: &Step) -> u64 {
    match step {
        Step::Text(_) => TEXT_WEIGHT,
        Step::File(file) => file.size.unwrap_or(0).max(1),
    }
}

// MARK: The extension's session

/// The signed-in session, opened by the share extension without syncing. One per extension
/// process.
#[derive(uniffi::Object)]
pub struct ShareSession {
    client: Client,
    me: String,
    outbox: PathBuf,
}

impl Drop for ShareSession {
    fn drop(&mut self) {
        // The stores close on the runtime (see `PushDecoder`).
        let client = self.client.clone();
        crate::runtime::spawn(async move { drop(client) });
    }
}

/// Opens the signed-in session for the share extension, or `None` when no one is signed in.
#[uniffi::export]
pub async fn open_share_session(
    config: ClientConfig,
    secrets: Arc<dyn SecretStore>,
) -> Result<Option<Arc<ShareSession>>> {
    crate::client::init_tracing();
    run(async move {
        let Some(raw) = secrets.load(SESSION_KEY.to_owned()) else { return Ok(None) };
        let stored: StoredSession = serde_json::from_str(&raw).map_err(XeniaError::generic)?;
        let process = Process::ShareExtension;
        let client =
            build_client(&config, Server::Url(&stored.homeserver_url), &stored.folder, &stored.passphrase, process)
                .await?;
        client.restore_session(stored.session).await?;
        lock_crypto_store(&client, process).await?;
        let me = client.user_id().ok_or(XeniaError::NotSignedIn)?.to_string();
        Ok(Some(Arc::new(ShareSession { client, me, outbox: outbox_dir(&config.data_dir) })))
    })
    .await
}

#[uniffi::export]
impl ShareSession {
    /// Every conversation something can be shared to, newest activity first, from what the stores
    /// already know (the extension doesn't sync).
    pub async fn conversations(self: Arc<Self>) -> Vec<ShareTarget> {
        let client = self.client.clone();
        run(async move {
            let mut targets = Vec::new();
            for room in client.joined_rooms() {
                if room.is_space() || room.is_tombstoned() {
                    continue;
                }
                targets.push(target(&room).await);
            }
            order_targets(&mut targets);
            targets
        })
        .await
    }

    /// A square avatar, `size_px` on a side.
    pub async fn load_avatar(self: Arc<Self>, mxc_url: String, size_px: u32) -> Result<Vec<u8>> {
        let client = self.client.clone();
        run(async move { crate::media::avatar_bytes(&client, mxc_url, size_px).await }).await
    }

    /// Gets a share ready to send to a conversation: the comment typed above it, then the items.
    pub fn prepare(&self, room_id: String, items: Vec<SharedItem>, comment: String) -> Result<Arc<PendingShare>> {
        let room_id = RoomId::parse(&room_id)?;
        let steps = with_sizes_on_disk(plan(items, &comment));
        if steps.is_empty() {
            return Err(XeniaError::generic("nothing to share"));
        }
        let total = steps.iter().map(weight).sum();
        Ok(Arc::new(PendingShare {
            client: self.client.clone(),
            me: self.me.clone(),
            outbox: self.outbox.clone(),
            room_id,
            sends_here: sends_in_extension(&steps),
            steps,
            total,
            state: Mutex::new(ShareState::default()),
            sending: tokio::sync::Mutex::new(()),
        }))
    }
}

async fn target(room: &Room) -> ShareTarget {
    let is_direct = room.is_direct().await.unwrap_or(false);
    let heroes = if is_direct { room.heroes().await } else { Vec::new() };
    let hero = heroes.first();
    let is_dm = room.compute_is_dm().await.unwrap_or(false);
    ShareTarget {
        id: room.room_id().to_string(),
        name: room_name(room, hero.and_then(|h| h.display_name.as_deref())),
        avatar_url: room.avatar_url().or_else(|| hero.and_then(|h| h.avatar_url.clone())).map(|u| u.to_string()),
        avatar_seed: hero.map(|h| h.user_id.to_string()).unwrap_or_else(|| room.room_id().to_string()),
        is_direct,
        dm_user_id: if is_dm { hero.map(|h| h.user_id.to_string()) } else { None },
        timestamp_ms: room.latest_event_timestamp().map(|t| i64::from(t.get())).unwrap_or(0),
    }
}

#[derive(Default)]
struct ShareState {
    /// The first step that hasn't reached the server.
    next: usize,
    /// The weight of the steps already sent.
    done: u64,
    /// The step being uploaded now.
    current: Option<SharedObservable<TransmissionProgress>>,
    handed_off: bool,
}

/// A share on its way to one conversation. Sending again after a failure picks up at the step
/// that failed.
#[derive(uniffi::Object)]
pub struct PendingShare {
    client: Client,
    me: String,
    outbox: PathBuf,
    room_id: OwnedRoomId,
    steps: Vec<Step>,
    total: u64,
    /// Every file is small enough to upload from the extension.
    sends_here: bool,
    state: Mutex<ShareState>,
    /// Held while sending or handing off, so the two never overlap.
    sending: tokio::sync::Mutex<()>,
}

impl Drop for PendingShare {
    fn drop(&mut self) {
        let client = self.client.clone();
        crate::runtime::spawn(async move { drop(client) });
    }
}

#[uniffi::export]
impl PendingShare {
    /// Nothing has reached the server yet, so the share can still be changed or dropped.
    pub fn nothing_sent(&self) -> bool {
        self.state.locked().next == 0
    }

    /// How far the share has got, from 0 to 1.
    pub fn progress(&self) -> f64 {
        let state = self.state.locked();
        let mut done = state.done as f64;
        if let (Some(current), Some(step)) = (&state.current, self.steps.get(state.next)) {
            let p = current.get();
            if p.total > 0 {
                done += weight(step) as f64 * (p.current as f64 / p.total as f64).min(1.0);
            }
        }
        if self.total == 0 { 0.0 } else { (done / self.total as f64).min(1.0) }
    }

    /// Sends what hasn't been sent yet, and returns once the server has all of it. A share too
    /// large for the extension is handed to the app instead.
    pub async fn send(self: Arc<Self>) -> Result<ShareOutcome> {
        let this = Arc::clone(&self);
        run(async move {
            let _sending = this.sending.lock().await;
            if this.state.locked().handed_off {
                return Ok(ShareOutcome::HandedOff);
            }
            if !this.sends_here {
                this.write_to_outbox().await?;
                return Ok(ShareOutcome::HandedOff);
            }
            let room = this.client.get_room(&this.room_id).ok_or(XeniaError::NotFound)?;
            if room.state() != RoomState::Joined {
                return Err(XeniaError::NotFound);
            }
            loop {
                let next = this.state.locked().next;
                let Some(step) = this.steps.get(next) else { return Ok(ShareOutcome::Sent) };
                let progress = SharedObservable::new(TransmissionProgress::default());
                this.state.locked().current = Some(progress.clone());
                let result = send_step(&room, step, progress).await;
                let mut state = this.state.locked();
                state.current = None;
                result?;
                state.next += 1;
                state.done += weight(step);
            }
        })
        .await
    }

    /// Hands what hasn't been sent yet to the app, which sends it the next time it opens.
    pub async fn hand_off(self: Arc<Self>) -> Result<()> {
        let this = Arc::clone(&self);
        run(async move {
            let _sending = this.sending.lock().await;
            this.write_to_outbox().await
        })
        .await
    }
}

impl PendingShare {
    async fn write_to_outbox(&self) -> Result<()> {
        let remaining = {
            let state = self.state.locked();
            if state.handed_off {
                return Ok(());
            }
            self.steps[state.next.min(self.steps.len())..].to_vec()
        };
        if !remaining.is_empty() {
            write_entry(&self.outbox, &self.me, self.room_id.as_str(), &remaining).await?;
        }
        self.state.locked().handed_off = true;
        Ok(())
    }
}

async fn send_step(room: &Room, step: &Step, progress: SharedObservable<TransmissionProgress>) -> Result<()> {
    match step {
        Step::Text(text) => {
            let message = compose_message(text.clone(), Vec::new());
            room.send(RoomMessageEventContent::from(message_content(&message))).await?;
        }
        Step::File(file) => {
            let data = tokio::fs::read(&file.path).await?;
            let (mime, config) = sdk_attachment(file).await;
            room.send_attachment(file.filename.clone(), &mime, data, config)
                .with_send_progress_observable(progress)
                .await?;
        }
    }
    Ok(())
}

async fn sdk_attachment(file: &Attachment) -> (mime::Mime, AttachmentConfig) {
    let AttachmentParts { mime, info, caption, thumbnail } = attachment_parts(file).await;
    (mime, AttachmentConfig { info: Some(info), caption, thumbnail, ..Default::default() })
}

// MARK: The outbox

/// Beside the stores, in the app group, where both processes can reach it.
pub(crate) fn outbox_dir(data_dir: &str) -> PathBuf {
    Path::new(data_dir).join("outbox")
}

/// An entry still being written starts with this, so the app never reads half of one.
const PARTIAL_PREFIX: &str = ".partial-";
const MANIFEST: &str = "manifest.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    /// The account that shared it: another account signed in later sends none of it.
    user_id: String,
    room_id: String,
    steps: Vec<StoredStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredStep {
    Text {
        text: String,
    },
    File {
        /// The file's name inside the entry's folder.
        file: String,
        filename: String,
        mimetype: String,
        size: Option<u64>,
        width: Option<u32>,
        height: Option<u32>,
        duration_ms: Option<u64>,
        blurhash: Option<String>,
        caption: Option<String>,
        thumbnail: Option<String>,
    },
}

impl StoredStep {
    fn into_step(self, folder: &Path) -> Step {
        match self {
            StoredStep::Text { text } => Step::Text(text),
            StoredStep::File {
                file,
                filename,
                mimetype,
                size,
                width,
                height,
                duration_ms,
                blurhash,
                caption,
                thumbnail,
            } => Step::File(Attachment {
                path: folder.join(file).to_string_lossy().into_owned(),
                filename,
                mimetype,
                size,
                width,
                height,
                duration_ms,
                blurhash,
                caption,
                thumbnail_path: thumbnail.map(|t| folder.join(t).to_string_lossy().into_owned()),
            }),
        }
    }
}

/// Copies the steps' files into a new outbox entry and writes its manifest, then makes it visible
/// to the app in one rename.
async fn write_entry(outbox: &Path, user_id: &str, room_id: &str, steps: &[Step]) -> Result<()> {
    let name = format!("{:013}-{}", chrono::Utc::now().timestamp_millis(), uuid::Uuid::new_v4().simple());
    let partial = outbox.join(format!("{PARTIAL_PREFIX}{name}"));
    tokio::fs::create_dir_all(&partial).await?;
    let result = async {
        let mut stored = Vec::new();
        for (index, step) in steps.iter().enumerate() {
            stored.push(match step {
                Step::Text(text) => StoredStep::Text { text: text.clone() },
                Step::File(file) => {
                    let copied = format!("{index}-file");
                    tokio::fs::copy(&file.path, partial.join(&copied)).await?;
                    let thumbnail = match &file.thumbnail_path {
                        Some(path) => {
                            let name = format!("{index}-thumbnail");
                            // A poster frame that can't be copied is left out, as when sending.
                            tokio::fs::copy(path, partial.join(&name)).await.ok().map(|_| name)
                        }
                        None => None,
                    };
                    StoredStep::File {
                        file: copied,
                        filename: file.filename.clone(),
                        mimetype: file.mimetype.clone(),
                        size: file.size,
                        width: file.width,
                        height: file.height,
                        duration_ms: file.duration_ms,
                        blurhash: file.blurhash.clone(),
                        caption: file.caption.clone(),
                        thumbnail,
                    }
                }
            });
        }
        let manifest = Manifest {
            version: OUTBOX_VERSION,
            user_id: user_id.to_owned(),
            room_id: room_id.to_owned(),
            steps: stored,
        };
        let json = serde_json::to_vec(&manifest).map_err(XeniaError::generic)?;
        tokio::fs::write(partial.join(MANIFEST), json).await?;
        tokio::fs::rename(&partial, outbox.join(&name)).await?;
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_dir_all(&partial).await;
    }
    result
}

/// The entries ready to send, oldest first. Ones still being written are skipped; ones abandoned
/// halfway (the extension was stopped) are removed after a day.
async fn ready_entries(outbox: &Path) -> Vec<PathBuf> {
    let Ok(mut read) = tokio::fs::read_dir(outbox).await else { return Vec::new() };
    let mut entries = Vec::new();
    while let Ok(Some(entry)) = read.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(PARTIAL_PREFIX) {
            let stale = entry
                .metadata()
                .await
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > std::time::Duration::from_secs(24 * 60 * 60));
            if stale {
                let _ = tokio::fs::remove_dir_all(entry.path()).await;
            }
            continue;
        }
        if name.starts_with('.') {
            continue;
        }
        entries.push(entry.path());
    }
    entries.sort();
    entries
}

/// What the app should do with an outbox entry.
#[derive(Debug, PartialEq, Eq)]
enum EntryFate {
    Send,
    /// Another account's, or unreadable: nothing in it will ever be sent.
    Discard,
    /// From another version of Xenia: left for it.
    Keep,
}

fn fate(manifest: Option<&Manifest>, me: &str) -> EntryFate {
    match manifest {
        None => EntryFate::Discard,
        Some(m) if m.version != OUTBOX_VERSION => EntryFate::Keep,
        Some(m) if m.user_id != me => EntryFate::Discard,
        Some(_) => EntryFate::Send,
    }
}

/// Queues everything the share extension handed over, in order, through the send queue (so each
/// appears in its conversation at once and retries like any message), and removes each entry once
/// queued. Run by the app when a session starts and when it comes back.
pub(crate) async fn send_handed_off(client: Client, data_dir: String, me: String) {
    static DRAINING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    use std::sync::atomic::Ordering;
    if DRAINING.swap(true, Ordering::AcqRel) {
        return;
    }
    let outbox = outbox_dir(&data_dir);
    for folder in ready_entries(&outbox).await {
        let manifest_path = folder.join(MANIFEST);
        let manifest: Option<Manifest> =
            tokio::fs::read(&manifest_path).await.ok().and_then(|bytes| serde_json::from_slice(&bytes).ok());
        match fate(manifest.as_ref(), &me) {
            EntryFate::Keep => continue,
            EntryFate::Discard => {
                let _ = tokio::fs::remove_dir_all(&folder).await;
                continue;
            }
            EntryFate::Send => {}
        }
        let Some(mut manifest) = manifest else { continue };
        let room = RoomId::parse(&manifest.room_id).ok().and_then(|id| client.get_room(&id));
        let Some(room) = room.filter(|r| r.state() == RoomState::Joined) else {
            info!("Dropped a shared item for a conversation that's gone");
            let _ = tokio::fs::remove_dir_all(&folder).await;
            continue;
        };
        let mut stopped = false;
        while !manifest.steps.is_empty() {
            let step = manifest.steps[0].clone().into_step(&folder);
            if let Err(error) = queue_step(&room, &step).await {
                warn!("Couldn't queue a shared item, trying again later: {error}");
                stopped = true;
                break;
            }
            manifest.steps.remove(0);
            // What's queued is the send queue's now: an interruption here mustn't send it twice.
            if let Ok(json) = serde_json::to_vec(&manifest) {
                let _ = tokio::fs::write(&manifest_path, json).await;
            }
        }
        if stopped {
            break;
        }
        let _ = tokio::fs::remove_dir_all(&folder).await;
    }
    DRAINING.store(false, Ordering::Release);
}

async fn queue_step(room: &Room, step: &Step) -> Result<()> {
    let queue = room.send_queue();
    match step {
        Step::Text(text) => {
            let message = compose_message(text.clone(), Vec::new());
            let content = RoomMessageEventContent::from(message_content(&message));
            queue.send(content.into()).await.map_err(XeniaError::generic)?;
        }
        Step::File(file) => {
            let data = tokio::fs::read(&file.path).await?;
            let (mime, config) = sdk_attachment(file).await;
            queue.send_attachment(file.filename.clone(), mime, data, config).await.map_err(XeniaError::generic)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, size: Option<u64>) -> Attachment {
        Attachment {
            path: format!("/tmp/{name}"),
            filename: name.into(),
            mimetype: "image/jpeg".into(),
            size,
            width: None,
            height: None,
            duration_ms: None,
            blurhash: None,
            caption: Some("left over".into()),
            thumbnail_path: None,
        }
    }

    fn text(text: &str) -> SharedItem {
        SharedItem::Text { text: text.into() }
    }

    fn target(id: &str, name: &str, timestamp_ms: i64) -> ShareTarget {
        ShareTarget {
            id: id.into(),
            name: name.into(),
            avatar_url: None,
            avatar_seed: id.into(),
            is_direct: false,
            dm_user_id: None,
            timestamp_ms,
        }
    }

    #[test]
    fn a_lone_file_takes_the_comment_as_its_caption() {
        let steps = plan(vec![SharedItem::File { attachment: file("a.jpg", Some(5)) }], "  Look  ");
        let mut expected = file("a.jpg", Some(5));
        expected.caption = Some("Look".into());
        assert_eq!(steps, vec![Step::File(expected)]);
    }

    #[test]
    fn text_goes_first_as_one_message_without_repeats() {
        let url = "https://example.org/a";
        let steps = plan(
            vec![
                text(url),
                SharedItem::File { attachment: file("a.jpg", Some(5)) },
                text(&format!(" {url} ")),
                text("  "),
            ],
            "Look",
        );
        let mut bare = file("a.jpg", Some(5));
        bare.caption = None;
        assert_eq!(steps, vec![Step::Text(format!("Look\n{url}")), Step::File(bare)]);
    }

    #[test]
    fn several_files_keep_the_comment_as_its_own_message() {
        let steps = plan(
            vec![
                SharedItem::File { attachment: file("a.jpg", Some(5)) },
                SharedItem::File { attachment: file("b.jpg", Some(5)) },
            ],
            "Two",
        );
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0], Step::Text("Two".into()));
        assert!(steps[1..].iter().all(|s| matches!(s, Step::File(f) if f.caption.is_none())));
        assert!(plan(vec![text(" ")], " ").is_empty());
    }

    #[test]
    fn large_or_unknown_files_go_to_the_app() {
        let small = Step::File(file("a.jpg", Some(EXTENSION_UPLOAD_LIMIT)));
        let large = Step::File(file("b.mov", Some(EXTENSION_UPLOAD_LIMIT + 1)));
        let unknown = Step::File(file("c.bin", None));
        assert!(sends_in_extension(&[Step::Text("hi".into()), small.clone()]));
        assert!(!sends_in_extension(&[small.clone(), large]));
        assert!(!sends_in_extension(&[small, unknown]));
    }

    #[test]
    fn targets_are_newest_first_and_found_by_name() {
        let mut targets = vec![target("!b", "Book club", 10), target("!a", "Alex", 30), target("!c", "Camp", 10)];
        order_targets(&mut targets);
        let names: Vec<&str> = targets.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Alex", "Book club", "Camp"]);
        let found = filter_share_targets(targets.clone(), "club bo".into());
        assert_eq!(found.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["!b"]);
        assert_eq!(filter_share_targets(targets, " ".into()).len(), 3);
    }

    #[test]
    fn entries_belong_to_their_account_and_version() {
        let manifest =
            Manifest { version: OUTBOX_VERSION, user_id: "@me:x".into(), room_id: "!a:x".into(), steps: vec![] };
        assert_eq!(fate(Some(&manifest), "@me:x"), EntryFate::Send);
        assert_eq!(fate(Some(&manifest), "@other:x"), EntryFate::Discard);
        assert_eq!(fate(None, "@me:x"), EntryFate::Discard);
        let newer = Manifest { version: OUTBOX_VERSION + 1, ..manifest };
        assert_eq!(fate(Some(&newer), "@other:x"), EntryFate::Keep);
    }

    #[tokio::test]
    async fn an_entry_appears_whole_with_its_files() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("photo.jpg");
        std::fs::write(&source, b"jpeg").unwrap();
        let mut photo = file("photo.jpg", Some(4));
        photo.path = source.to_string_lossy().into_owned();
        photo.thumbnail_path = Some(dir.path().join("missing.jpg").to_string_lossy().into_owned());
        let outbox = dir.path().join("outbox");
        let steps = vec![Step::Text("Look".into()), Step::File(photo.clone())];
        write_entry(&outbox, "@me:x", "!a:x", &steps).await.unwrap();

        // A half-written entry from a stopped extension is never read.
        std::fs::create_dir_all(outbox.join(format!("{PARTIAL_PREFIX}0-x"))).unwrap();
        let entries = ready_entries(&outbox).await;
        assert_eq!(entries.len(), 1);
        let folder = &entries[0];
        let manifest: Manifest = serde_json::from_slice(&std::fs::read(folder.join(MANIFEST)).unwrap()).unwrap();
        assert_eq!(manifest.room_id, "!a:x");
        let read: Vec<Step> = manifest.steps.into_iter().map(|s| s.into_step(folder)).collect();
        assert_eq!(read[0], Step::Text("Look".into()));
        let Step::File(copied) = &read[1] else { panic!("expected a file") };
        assert_eq!(std::fs::read(&copied.path).unwrap(), b"jpeg");
        assert_eq!(copied.filename, "photo.jpg");
        assert_eq!(copied.caption, photo.caption);
        // The poster frame couldn't be copied, so it's left out rather than failing the share.
        assert_eq!(copied.thumbnail_path, None);
    }
}
