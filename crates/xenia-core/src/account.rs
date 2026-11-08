//! The account's own settings that live on the server or in the stores: profile photo, blocked
//! people, notification keywords, and the space Xenia takes on this device.

use std::path::Path;

use matrix_sdk::{
    Client,
    media::MediaRetentionPolicy,
    ruma::{MxcUri, OwnedMxcUri, UserId, events::ignored_user_list::IgnoredUserListEventContent},
    store::{StateStoreDataKey, StateStoreDataValue},
};
use tracing::{info, warn};

use crate::{
    client::XeniaClient,
    error::{XeniaError, Result},
    models::Person,
    runtime::run,
};

/// How much space Xenia's caches take on this device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct StorageUsage {
    /// Photos, videos and files downloaded from conversations. Clearing it is safe: they
    /// download again when needed.
    pub media_bytes: u64,
}

#[uniffi::export]
impl XeniaClient {
    // MARK: Profile photo

    /// Uploads a new profile photo (JPEG or PNG bytes) and uses it.
    pub async fn set_avatar(&self, data: Vec<u8>, mimetype: String) -> Result<()> {
        let core = self.core().await?;
        let client = core.client.clone();
        let url = run(async move { change_avatar(&client, &mimetype, data).await }).await?;
        // Shown from what the server took, not read back: a read right after the change can
        // still see the old photo (a worker behind on replication), or fail.
        self.show_avatar(Some(url));
        Ok(())
    }

    pub async fn remove_avatar(&self) -> Result<()> {
        let core = self.core().await?;
        let client = core.client.clone();
        run(async move { clear_avatar(&client).await }).await?;
        self.show_avatar(None);
        Ok(())
    }

    // MARK: Blocked people

    /// People I've blocked (ignored): their messages and invitations don't reach me.
    pub async fn blocked_people(&self) -> Result<Vec<Person>> {
        let core = self.core().await?;
        run(async move {
            let content = core.client.account().account_data::<IgnoredUserListEventContent>().await?;
            let ids: Vec<_> = content
                .and_then(|raw| raw.deserialize().ok())
                .map(|c| c.ignored_users.into_keys().collect())
                .unwrap_or_default();
            // Profiles load together, not one after another.
            let account = core.client.account();
            let profiles = futures_util::future::join_all(ids.iter().map(|id| account.fetch_user_profile_of(id))).await;
            let mut people = Vec::with_capacity(ids.len());
            for (user_id, profile) in ids.into_iter().zip(profiles) {
                let profile = profile.ok();
                people.push(Person {
                    display_name: profile
                        .as_ref()
                        .and_then(|p| p.get("displayname"))
                        .and_then(|v| v.as_str())
                        .map(str::to_owned),
                    avatar_url: profile
                        .as_ref()
                        .and_then(|p| p.get("avatar_url"))
                        .and_then(|v| v.as_str())
                        .map(str::to_owned),
                    user_id: user_id.to_string(),
                });
            }
            people.sort_by_key(|p| crate::text::normalize_for_search(&p.name()));
            Ok(people)
        })
        .await
    }

    /// The IDs of people I've blocked, from what this device already knows (no request).
    pub async fn blocked_user_ids(&self) -> Vec<String> {
        let Ok(core) = self.core().await else { return Vec::new() };
        core.client.subscribe_to_ignore_user_list_changes().get()
    }

    pub async fn block(&self, user_id: String) -> Result<()> {
        let core = self.core().await?;
        run(async move {
            core.client.account().ignore_user(&UserId::parse(&user_id)?).await?;
            Ok(())
        })
        .await
    }

    pub async fn unblock(&self, user_id: String) -> Result<()> {
        let core = self.core().await?;
        run(async move {
            core.client.account().unignore_user(&UserId::parse(&user_id)?).await?;
            Ok(())
        })
        .await
    }

    // MARK: Keywords

    /// Words that notify like a mention, from the account's push rules (shared by every device).
    pub async fn notification_keywords(&self) -> Result<Vec<String>> {
        let core = self.core().await?;
        run(async move {
            let settings = core.client.notification_settings().await;
            let mut words: Vec<String> = settings.enabled_keywords().await.into_iter().collect();
            words.sort_by_key(|w| w.to_lowercase());
            Ok(words)
        })
        .await
    }

    pub async fn add_notification_keyword(&self, keyword: String) -> Result<()> {
        let core = self.core().await?;
        run(async move {
            let word = keyword.trim().to_owned();
            if word.is_empty() {
                return Ok(());
            }
            core.client.notification_settings().await.add_keyword(word).await.map_err(XeniaError::generic)?;
            core.reload_keywords().await;
            Ok(())
        })
        .await
    }

    pub async fn remove_notification_keyword(&self, keyword: String) -> Result<()> {
        let core = self.core().await?;
        run(async move {
            core.client.notification_settings().await.remove_keyword(&keyword).await.map_err(XeniaError::generic)?;
            core.reload_keywords().await;
            Ok(())
        })
        .await
    }

    // MARK: Storage

    /// Downloaded media in the SDK's media store, plus files saved for Quick Look and sharing.
    pub async fn storage_usage(&self) -> StorageUsage {
        let cache = self.cache_dir();
        let Ok(core) = self.core().await else { return StorageUsage { media_bytes: 0 } };
        run(async move {
            let store = core.client.get_store_sizes().await.ok().and_then(|s| s.media_store).unwrap_or(0);
            // A walk of the disk: off the async threads.
            let files = crate::media::saved_files_dir(&cache);
            let saved = tokio::task::spawn_blocking(move || folder_size(&files)).await.unwrap_or(0);
            StorageUsage { media_bytes: store as u64 + saved }
        })
        .await
    }

    /// Removes downloaded photos and files. Messages, keys and settings stay.
    pub async fn clear_media_cache(&self) -> Result<()> {
        let core = self.core().await?;
        let cache = self.cache_dir();
        run(async move {
            let files = crate::media::saved_files_dir(&cache);
            if let Err(error) = tokio::fs::remove_dir_all(&files).await
                && error.kind() != std::io::ErrorKind::NotFound
            {
                warn!("Couldn't remove saved files: {error}");
            }
            // The SDK's media store empties itself when the policy allows nothing, then goes back
            // to its usual limits.
            let media = core.client.media();
            media
                .set_media_retention_policy(MediaRetentionPolicy::new().with_max_cache_size(Some(0)))
                .await
                .map_err(XeniaError::generic)?;
            let cleaned = media.clean().await.map_err(XeniaError::generic);
            media.set_media_retention_policy(MediaRetentionPolicy::new()).await.map_err(XeniaError::generic)?;
            cleaned?;
            // SQLite keeps the freed pages until it's vacuumed; without this the space never
            // comes back.
            if let Ok(store) = core.client.media_store().lock().await
                && let Err(error) = store.optimize().await
            {
                warn!("Couldn't compact the media store: {error}");
            }
            Ok(())
        })
        .await
    }
}

// MARK: Profile

/// My name and photo as the server has them. A field is `None` when it couldn't be read, so a
/// failed read never wipes what's already shown; `Some(None)` means it isn't set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProfileRead {
    pub display_name: Option<Option<String>>,
    pub avatar_url: Option<Option<String>>,
}

impl ProfileRead {
    pub(crate) fn is_complete(&self) -> bool {
        self.display_name.is_some() && self.avatar_url.is_some()
    }
}

/// Reads my profile on Xenia's runtime, so any executor can await it: the SDK's requests need
/// Tokio's reactor and timers, and awaited from the app's own executor they panic, which the
/// app sees as an unexplained error.
pub(crate) async fn read_own_profile(client: Client) -> ProfileRead {
    run(async move { fetch_own_profile(&client).await }).await
}

/// The whole profile in one request (`GET /profile/{me}`); the two fields one by one only when
/// that fails. The per-field reads are fussier: the SDK's display name read fails outright on
/// `{"displayname": null}`, which some servers send for a name that isn't set.
async fn fetch_own_profile(client: &Client) -> ProfileRead {
    let account = client.account();
    match account.fetch_user_profile().await {
        Ok(profile) => {
            let field = |key: &str| profile.get(key).and_then(|v| v.as_str()).map(str::to_owned);
            let avatar_url = field("avatar_url").and_then(usable_avatar_url);
            remember_avatar(client, avatar_url.as_deref()).await;
            ProfileRead {
                display_name: Some(field("displayname").filter(|n| !n.trim().is_empty())),
                avatar_url: Some(avatar_url),
            }
        }
        Err(error) => {
            warn!("Couldn't read my profile, reading its fields one by one: {error}");
            let (name, avatar) = futures_util::join!(account.get_display_name(), account.get_avatar_url());
            ProfileRead {
                display_name: name
                    .inspect_err(|error| warn!("Couldn't read my display name: {error}"))
                    .ok()
                    .map(|n| n.filter(|n| !n.trim().is_empty())),
                avatar_url: avatar
                    .inspect_err(|error| warn!("Couldn't read my profile photo: {error}"))
                    .ok()
                    .map(|url| url.and_then(|u| usable_avatar_url(u.to_string()))),
            }
        }
    }
}

/// An avatar URL an avatar can be loaded from. Clients unset a photo with `""` (Element does),
/// and anything that isn't an `mxc://` URL can't be fetched from the media repository.
pub(crate) fn usable_avatar_url(url: String) -> Option<String> {
    let url = url.trim();
    <&MxcUri>::from(url).is_valid().then(|| url.to_owned())
}

/// Keeps my photo's URL where the SDK keeps it, so it shows at once on the next launch, offline
/// included.
async fn remember_avatar(client: &Client, url: Option<&str>) {
    let Some(user_id) = client.user_id() else { return };
    let store = client.state_store();
    let result = match url {
        Some(url) => {
            store
                .set_kv_data(
                    StateStoreDataKey::UserAvatarUrl(user_id),
                    StateStoreDataValue::UserAvatarUrl(OwnedMxcUri::from(url)),
                )
                .await
        }
        None => store.remove_kv_data(StateStoreDataKey::UserAvatarUrl(user_id)).await,
    };
    if let Err(error) = result {
        warn!("Couldn't remember my profile photo: {error}");
    }
}

/// My photo's URL from the last time it was read, without asking the server.
pub(crate) async fn cached_avatar_url(client: &Client) -> Option<String> {
    client.account().get_cached_avatar_url().await.ok().flatten().and_then(|u| usable_avatar_url(u.to_string()))
}

/// My photo's URL as the server has it now, or why it couldn't be read.
async fn server_avatar_url(client: &Client) -> Result<Option<String>> {
    let url = client.account().get_avatar_url().await?;
    Ok(url.and_then(|u| usable_avatar_url(u.to_string())))
}

/// Uploads a photo and makes it mine. Returns its `mxc://` URL.
///
/// Changing the photo makes the server update my membership in every room I'm in, one event
/// per room. With bridges that's hundreds of rooms, and the answer can time out (here, or at a
/// proxy in front of the server) after the server has already taken the photo. So a failed
/// change is checked against the server before it's reported.
pub(crate) async fn change_avatar(client: &Client, mimetype: &str, data: Vec<u8>) -> Result<String> {
    let mime: mime::Mime = mimetype.parse().unwrap_or(mime::IMAGE_JPEG);
    let url = upload(client, &mime, data).await.map_err(XeniaError::for_profile_change)?;
    if let Err(error) = put_avatar_url(client, &url).await {
        match server_avatar_url(client).await {
            Ok(Some(now)) if now == url.as_str() => {
                info!("Setting the profile photo failed but the server has it: {error}");
            }
            _ => {
                warn!("Couldn't set the profile photo to {url}: {error}");
                return Err(XeniaError::from(error).for_profile_change());
            }
        }
    }
    remember_avatar(client, Some(url.as_str())).await;
    Ok(url.to_string())
}

/// Uploads to the media repository and returns the new `mxc://` URL.
///
/// The SDK's own upload first asks the server for its upload limit
/// (`GET /_matrix/client/v1/media/config`) and gives up when that fails, though the upload
/// itself never needed it. That read fails on real servers: a proxy that routes the old media
/// paths but not the authenticated ones, a media repository that leaves out the optional
/// `m.upload.size` (which the SDK requires), or one that answers -1 for no limit. So the limit
/// is honoured when it can be read and skipped when it can't.
pub(crate) async fn upload(client: &Client, mime: &mime::Mime, data: Vec<u8>) -> Result<OwnedMxcUri> {
    use matrix_sdk::{config::RequestConfig, ruma::api::client::media::create_content};
    let bytes = data.len();
    match client.load_or_fetch_max_upload_size().await {
        Ok(max) if u64::try_from(bytes).is_ok_and(|b| b > u64::from(max)) => {
            warn!("Not uploading {bytes} bytes: the server takes at most {max}");
            return Err(XeniaError::TooLarge);
        }
        Ok(_) => {}
        Err(error) => warn!("Couldn't read the server's upload limit, uploading anyway: {error}"),
    }
    let mut request = create_content::v3::Request::new(data);
    request.content_type = Some(mime.essence_str().to_owned());
    // As the SDK times uploads: by size, never under a minute.
    let timeout = std::time::Duration::from_secs(60.max(bytes as u64 / 50_000));
    let response =
        client.send(request).with_request_config(RequestConfig::short_retry().timeout(timeout)).await.map_err(
            |error| {
                warn!("Couldn't upload {bytes} bytes of {mime}: {error}");
                XeniaError::from(error)
            },
        )?;
    Ok(response.content_uri)
}

/// Points my profile at `url`. Unlike the SDK's own call, with a bounded retry and a longer
/// wait: the server answers only once every room has my new membership, and the SDK keeps
/// retrying a 502 or 504 from a proxy for a quarter of an hour, all of it behind a spinner.
async fn put_avatar_url(client: &Client, url: &MxcUri) -> Result<(), matrix_sdk::Error> {
    use matrix_sdk::{config::RequestConfig, ruma::api::client::profile::set_avatar_url};
    let user_id = client.user_id().ok_or(matrix_sdk::Error::AuthenticationRequired)?;
    #[allow(deprecated)] // `set_profile_field` is newer than many servers.
    let request = set_avatar_url::v3::Request::new(user_id.to_owned(), Some(url.to_owned()));
    let config = RequestConfig::short_retry().timeout(std::time::Duration::from_secs(120));
    client.send(request).with_request_config(config).await.map_err(matrix_sdk::Error::from)?;
    Ok(())
}

/// Removes my photo, checked against the server the same way when the answer fails.
pub(crate) async fn clear_avatar(client: &Client) -> Result<()> {
    if let Err(error) = client.account().set_avatar_url(None).await {
        match server_avatar_url(client).await {
            Ok(None) => info!("Removing the profile photo failed but the server has none: {error}"),
            _ => {
                warn!("Couldn't remove the profile photo: {error}");
                return Err(XeniaError::from(error).for_profile_change());
            }
        }
    }
    remember_avatar(client, None).await;
    Ok(())
}

fn folder_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else { return 0 };
    entries
        .flatten()
        .map(|entry| match entry.metadata() {
            Ok(meta) if meta.is_dir() => folder_size(&entry.path()),
            Ok(meta) => meta.len(),
            Err(_) => 0,
        })
        .sum()
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{
        future::Future,
        sync::{Arc, Mutex},
        task::{Context, Poll, Wake, Waker},
    };

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    /// Awaits a future the way the app's executor does: on a plain thread, with no Tokio
    /// runtime around it.
    pub(crate) fn block_on_foreign<F: Future>(future: F) -> F::Output {
        struct Unpark(std::thread::Thread);
        impl Wake for Unpark {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
        let mut cx = Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);
        loop {
            match future.as_mut().poll(&mut cx) {
                Poll::Ready(value) => return value,
                Poll::Pending => std::thread::park_timeout(std::time::Duration::from_millis(50)),
            }
        }
    }

    type Routes = Arc<dyn Fn(&str, &str) -> (u16, String) + Send + Sync>;

    /// A homeserver that answers each request from `routes` (method, path without the query)
    /// and records what was asked.
    pub(crate) struct MockServer {
        pub url: String,
        pub requests: Arc<Mutex<Vec<String>>>,
    }

    impl MockServer {
        pub(crate) fn start(routes: impl Fn(&str, &str) -> (u16, String) + Send + Sync + 'static) -> Self {
            let routes: Routes = Arc::new(routes);
            let requests = Arc::new(Mutex::new(Vec::new()));
            let log = Arc::clone(&requests);
            let listener = block_on_foreign(run(async { tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap() }));
            let url = format!("http://{}", listener.local_addr().unwrap());
            crate::runtime::spawn(async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    let routes = Arc::clone(&routes);
                    let log = Arc::clone(&log);
                    crate::runtime::spawn(async move {
                        let mut buffer = Vec::new();
                        let mut chunk = [0u8; 4096];
                        let head_end = loop {
                            let Ok(n) = socket.read(&mut chunk).await else { return };
                            if n == 0 {
                                return;
                            }
                            buffer.extend_from_slice(&chunk[..n]);
                            if let Some(i) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                                break i + 4;
                            }
                        };
                        let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
                        let length = head
                            .lines()
                            .find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().to_owned()))
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(0);
                        while buffer.len() < head_end + length {
                            let Ok(n) = socket.read(&mut chunk).await else { return };
                            if n == 0 {
                                break;
                            }
                            buffer.extend_from_slice(&chunk[..n]);
                        }
                        let mut words = head.split_whitespace();
                        let method = words.next().unwrap_or_default().to_owned();
                        let target = words.next().unwrap_or_default();
                        let path = target.split('?').next().unwrap_or_default().to_owned();
                        log.lock().unwrap().push(format!("{method} {path}"));
                        let (status, body) = if path == "/_matrix/client/versions" {
                            (200, r#"{"versions":["v1.1","v1.11"]}"#.to_owned())
                        } else {
                            routes(&method, &path)
                        };
                        let kind = if body.starts_with('{') { "application/json" } else { "text/html" };
                        let response = format!(
                            "HTTP/1.1 {status} X\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = socket.write_all(response.as_bytes()).await;
                        let _ = socket.shutdown().await;
                    });
                }
            });
            Self { url, requests }
        }

        pub(crate) fn asked(&self, request: &str) -> bool {
            self.requests.lock().unwrap().iter().any(|r| r == request)
        }
    }

    pub(crate) const ME: &str = "@me:localhost";

    /// A client signed in as `ME` on `server`, built on Xenia's runtime.
    pub(crate) fn signed_in(server: &MockServer) -> Client {
        let url = server.url.clone();
        block_on_foreign(run(async move {
            let client = Client::builder().homeserver_url(&url).build().await.unwrap();
            let session: matrix_sdk::authentication::matrix::MatrixSession = serde_json::from_value(
                serde_json::json!({ "user_id": ME, "device_id": "DEVICE", "access_token": "token" }),
            )
            .unwrap();
            client.restore_session(session).await.unwrap();
            client
        }))
    }

    fn not_found() -> (u16, String) {
        (404, r#"{"errcode":"M_UNRECOGNIZED","error":"Unrecognized request"}"#.to_owned())
    }

    const UPLOAD: &str = "/_matrix/media/v3/upload";
    const MEDIA_CONFIG: &str = "/_matrix/client/v1/media/config";

    #[test]
    fn a_photo_uploads_when_the_upload_limit_cant_be_read() {
        // A proxy that doesn't route the authenticated media paths, or a media repository
        // that leaves out the optional limit: the SDK's upload gave up before uploading.
        for config in [(404, "<html>Not Found</html>"), (200, "{}"), (200, r#"{"m.upload.size":-1}"#)] {
            let server = MockServer::start(move |method, path| match (method, path) {
                ("GET", MEDIA_CONFIG) => (config.0, config.1.to_owned()),
                ("POST", UPLOAD) => (200, r#"{"content_uri":"mxc://localhost/new"}"#.to_owned()),
                ("PUT", MY_AVATAR) => (200, "{}".to_owned()),
                _ => not_found(),
            });
            let client = signed_in(&server);
            let url = block_on_foreign(run(async move { change_avatar(&client, "image/jpeg", vec![1, 2]).await }));
            assert_eq!(url, Ok("mxc://localhost/new".to_owned()), "{config:?}");
            assert!(server.asked(&format!("PUT {MY_AVATAR}")));
        }
    }

    #[test]
    fn a_photo_over_the_upload_limit_is_not_sent() {
        let server = MockServer::start(|method, path| match (method, path) {
            ("GET", MEDIA_CONFIG) => (200, r#"{"m.upload.size":1}"#.to_owned()),
            _ => not_found(),
        });
        let client = signed_in(&server);
        let result = block_on_foreign(run(async move { change_avatar(&client, "image/jpeg", vec![1, 2]).await }));
        assert!(matches!(result, Err(XeniaError::PhotoNotAccepted { .. })), "{result:?}");
        assert!(!server.asked(&format!("POST {UPLOAD}")));
    }
    const MY_AVATAR: &str = "/_matrix/client/v3/profile/@me:localhost/avatar_url";

    #[test]
    fn a_photo_the_server_took_is_not_reported_as_failed() {
        // A proxy gave up on the answer while the server updated every room.
        let server = MockServer::start(|method, path| match (method, path) {
            ("GET", MEDIA_CONFIG) => (200, r#"{"m.upload.size":52428800}"#.to_owned()),
            ("POST", UPLOAD) => (200, r#"{"content_uri":"mxc://localhost/new"}"#.to_owned()),
            ("PUT", MY_AVATAR) => (504, "<html>504 Gateway Time-out</html>".to_owned()),
            ("GET", MY_AVATAR) => (200, r#"{"avatar_url":"mxc://localhost/new"}"#.to_owned()),
            _ => not_found(),
        });
        let client = signed_in(&server);
        let url = block_on_foreign(run(async move { change_avatar(&client, "image/jpeg", vec![0xFF, 0xD8]).await }));
        assert_eq!(url, Ok("mxc://localhost/new".to_owned()), "{:?}", server.requests.lock().unwrap());
        assert!(server.asked(&format!("PUT {MY_AVATAR}")));
    }

    #[test]
    fn a_refused_photo_says_why() {
        let server = MockServer::start(|method, path| match (method, path) {
            ("GET", MEDIA_CONFIG) => (200, r#"{"m.upload.size":52428800}"#.to_owned()),
            ("POST", UPLOAD) => (200, r#"{"content_uri":"mxc://localhost/new"}"#.to_owned()),
            ("PUT", MY_AVATAR) => {
                (400, r#"{"errcode":"M_FORBIDDEN","error":"Changing avatar is disabled on this server"}"#.to_owned())
            }
            ("GET", MY_AVATAR) => (200, "{}".to_owned()),
            _ => not_found(),
        });
        let client = signed_in(&server);
        let result = block_on_foreign(run(async move { change_avatar(&client, "image/jpeg", vec![1]).await }));
        assert!(matches!(result, Err(XeniaError::ProfileChangeDisabled { .. })), "{result:?}");

        let server = MockServer::start(|method, path| match (method, path) {
            ("GET", MEDIA_CONFIG) => (200, r#"{"m.upload.size":52428800}"#.to_owned()),
            ("POST", UPLOAD) => (413, "<html>413 Request Entity Too Large</html>".to_owned()),
            _ => not_found(),
        });
        let client = signed_in(&server);
        let result = block_on_foreign(run(async move { change_avatar(&client, "image/jpeg", vec![1]).await }));
        assert!(matches!(result, Err(XeniaError::PhotoNotAccepted { .. })), "{result:?}");
    }

    #[test]
    fn the_profile_reads_from_the_apps_executor() {
        let server = MockServer::start(|method, path| match (method, path) {
            ("GET", "/_matrix/client/v3/profile/@me:localhost") => {
                (200, r#"{"displayname":"Luke","avatar_url":"mxc://localhost/face"}"#.to_owned())
            }
            _ => not_found(),
        });
        let client = signed_in(&server);
        // What `set_avatar` used to do after uploading: the SDK awaited off Xenia's runtime.
        // It panics, which the app shows as "Something went wrong" over a photo that was set.
        let direct = client.clone();
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            block_on_foreign(async move { direct.account().fetch_user_profile().await.map(|_| ()) })
        }));
        assert!(panicked.is_err(), "the SDK needs Tokio's runtime");

        let read = block_on_foreign(read_own_profile(client.clone()));
        assert_eq!(
            read,
            ProfileRead {
                display_name: Some(Some("Luke".to_owned())),
                avatar_url: Some(Some("mxc://localhost/face".to_owned())),
            }
        );
        // Remembered for the next launch.
        let cached = block_on_foreign(run(async move { cached_avatar_url(&client).await }));
        assert_eq!(cached.as_deref(), Some("mxc://localhost/face"));
    }

    #[test]
    fn a_profile_read_that_fails_keeps_what_it_can() {
        // The whole profile can't be read; the fields can. Element unsets a photo with "".
        let server = MockServer::start(|method, path| match (method, path) {
            ("GET", "/_matrix/client/v3/profile/@me:localhost/displayname") => {
                (200, r#"{"displayname":"Luke"}"#.to_owned())
            }
            ("GET", MY_AVATAR) => (200, r#"{"avatar_url":""}"#.to_owned()),
            _ => (500, r#"{"errcode":"M_UNKNOWN","error":"Internal server error"}"#.to_owned()),
        });
        let client = signed_in(&server);
        let read = block_on_foreign(read_own_profile(client));
        assert_eq!(read, ProfileRead { display_name: Some(Some("Luke".to_owned())), avatar_url: Some(None) });

        // Nothing readable: nothing known, so nothing shown gets wiped.
        let server = MockServer::start(|_, _| (500, r#"{"errcode":"M_UNKNOWN","error":"Down"}"#.to_owned()));
        let read = block_on_foreign(read_own_profile(signed_in(&server)));
        assert_eq!(read, ProfileRead::default());
        assert!(!read.is_complete());
    }

    #[test]
    fn only_mxc_urls_are_avatars() {
        assert_eq!(usable_avatar_url("mxc://example.org/abc".to_owned()).as_deref(), Some("mxc://example.org/abc"));
        assert_eq!(usable_avatar_url(String::new()), None);
        assert_eq!(usable_avatar_url("https://example.org/a.png".to_owned()), None);
        assert_eq!(usable_avatar_url("mxc://".to_owned()), None);
    }

    #[test]
    fn folder_size_counts_nested_files_and_tolerates_missing_folders() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), [0u8; 10]).unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("b"), [0u8; 32]).unwrap();
        assert_eq!(folder_size(dir.path()), 42);
        assert_eq!(folder_size(&dir.path().join("missing")), 0);
    }
}
