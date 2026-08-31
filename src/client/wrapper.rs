use async_channel::{Receiver, Sender};
use asyncified::Asyncified;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures::executor;
use glib::clone;
use gtk::gio::prelude::*;
use gtk::{gio, glib};
use itertools::Itertools;
use lru::LruCache;
use mpd::search::{Operation as QueryOperation, Window};
use mpd::{
    Channel, EditAction, Output, SaveMode, Subsystem, Version,
    error::{Error as MpdError, ErrorCode as MpdErrorCode},
    song::Id,
};
use mpd::{Query, Status, Term};
use nohash_hasher::NoHashHasher;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use std::borrow::Cow;
use std::hash::BuildHasherDefault;
use std::io::Cursor;
use std::num::NonZero;
use std::thread;
use std::{cell::RefCell, rc::Rc};
use uuid::Uuid;

use crate::cache::sqlite;
use crate::client::connection::ImageHandle;
use crate::common::{AlbumInfo, ArtistInfo, DynamicPlaylist, split_genre_tag, tags};
use crate::utils::settings_manager;
use crate::{
    common::{Album, Artist, INode, Song, SongInfo, Stickers},
    player::PlaybackFlow,
    utils,
};

use super::connection::{Connection, Error as ClientError, Result as ClientResult, Task};
use super::state::{ClientState, ConnectionState, StickersSupportLevel};
use super::{BATCH_SIZE, FETCH_LIMIT, StickerSetMode};

static MAX_RETRIES: u32 = 3;
static MAX_EXAMPLE_ALBUMS_PER_ALBUMARTIST: usize = 3;
// About as large as one sticker can contain without a "connection reset by peer".
// Also, even the most minimal metadata doc is already ~2300 base64 chars.
static META_STICKER_PAGE_SIZE: usize = 4096;

// Thin wrapper around blocking mpd::Clients. It contains two separate client
// objects connected to the same address, each living on their own std::thread.
// One (foreground) is used for short interactive operations like playback
// controls. The (background) other is reserved for batch operations such as
// fetching many songs or albums. The background client is also put into
// idle mode to receive server-side changes, such as MPRIS controls or changes
// from  another frontend. Both receives tasks from the main thread via their
// unbounded async_channels and responds via lightweight oneshot channels in
// order to expose an async API to the rest of the code.

// Heavy operations such as streaming lots of album arts from a remote server
// should be performed by the background client. Note that it is the foreground
// client that updates the seekbar position, as it is never in idle mode.

// Once in the idle mode, the background client is blocked and thus cannot check the
// work queue. As such, after inserting a work item into the queue, we use the
// foreground client to send a message to an mpd inter-client channel also listened
// to by the background client. This triggers an idle notification for the Message
// subsystem, allowing the background client to break out of the blocking idle.

// Compared to the pre-0.98.1 design, the new async API makes it much easier to
// implement loading spinners, vastly reduces dependency on async channels
// and glib object signals, and simplifies daisy-chaining metadata provision
// code (as the cache can now simply await cover art requests sent to the MPD
// wrapper directly).

/// RAII guard that decrements the fg/bg task counters exactly once, whether
/// the future it lives in completes normally or is aborted (dropped) mid-flight.
/// Without it, aborted calls would never decrement and the counters would leak.
struct TaskGuard {
    state: ClientState,
    bg: bool,
}

impl TaskGuard {
    fn new(state: ClientState, bg: bool) -> Self {
        let res = Self {
            state,
            bg
        };
        if bg {
            res.state.inc_bg();
        } else {
            res.state.inc_fg();
        }
        res
    }
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        if self.bg {
            self.state.dec_bg();
        } else {
            self.state.dec_fg();
        }
    }
}

#[derive(Debug)]
pub struct MpdWrapper {
    // Handles return bool to indicate whether the threads stopped due to an error
    // (true) or disconnection request (false).
    _fg_handle: thread::JoinHandle<bool>,
    _bg_handle: thread::JoinHandle<bool>,
    // For heavy but parallelisable local tasks.
    state: ClientState,
    fg_sender: Sender<Task>, // For sending tasks to the interactive client
    bg_sender: Sender<Task>, // For sending tasks to the background client
    client_version: RefCell<Option<Version>>,
    song_cache: RefCell<LruCache<u32, Song, BuildHasherDefault<NoHashHasher<u32>>>>,
}

impl MpdWrapper {
    pub fn new() -> Rc<Self> {
        let ch_name = Uuid::new_v4().simple().to_string();
        let wake_channel = Channel::new(&ch_name).unwrap();
        let wake_channel_bg = wake_channel.clone();
        let (fg_sender, fg_receiver) = async_channel::unbounded();
        let (bg_sender, bg_receiver) = async_channel::unbounded();
        let (idle_sender, idle_receiver) = async_channel::unbounded();
        println!("Channel name: {}", &ch_name);
        let settings = settings_manager().child("client");
        let max_retries = if settings.boolean("mpd-auto-reconnect") {
            MAX_RETRIES
        } else {
            0
        };
        let wrapper = Rc::new(Self {
            _fg_handle: thread::spawn(move || {
                Connection::new(fg_receiver, wake_channel, None, max_retries)
                    .run()
                    .is_err()
            }),
            _bg_handle: thread::spawn(move || {
                Connection::new(bg_receiver, wake_channel_bg, Some(idle_sender), max_retries)
                    .run()
                    .is_err()
            }),
            state: ClientState::default(),
            fg_sender,
            bg_sender,
            client_version: RefCell::new(None),
            // Cache song infos so we can reuse them on queue updates.
            // Song IDs are u32s anyway, and I don't think there's any risk of a HashDoS attack
            // from a self-hosted music server so we'll just use identity hash for speed.
            song_cache: RefCell::new(LruCache::with_hasher(
                NonZero::new(16384).unwrap(),
                BuildHasherDefault::default(),
            )),
        });

        wrapper.clone().setup_channel(idle_receiver);

        wrapper
    }

    pub fn get_client_state(&self) -> ClientState {
        self.state.clone()
    }

    fn setup_channel(self: Rc<Self>, idle_receiver: Receiver<Subsystem>) {
        // Loop to handle idle changes
        glib::MainContext::default().spawn_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                use futures::prelude::*;
                let mut receiver = std::pin::pin!(idle_receiver);

                while let Some(change) = receiver.next().await {
                    this.handle_idle_changes(change).await;
                }
            }
        ));

        // Set up a ping loop. Main client does not use idle mode, so it needs to ping periodically.
        // If there is no client connected, it will simply skip pinging.
        let conn = utils::settings_manager().child("client");
        let ping_interval = conn.uint("mpd-ping-interval-s");
        glib::MainContext::default().spawn_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                loop {
                    let (s, r) = oneshot::channel();
                    match this.foreground(Task::Ping(s), r).await {
                        Ok(()) => {}
                        Err(ClientError::NotConnected) => {
                            println!(
                                "[KeepAlive] There is no client currently running. Won't ping."
                            );
                        }
                        Err(e) => {
                            dbg!(e);
                        }
                    };
                    glib::timeout_future_seconds(ping_interval).await;
                }
            }
        ));
    }

    async fn handle_idle_changes(&self, subsystem: Subsystem) {
        self.state.emit_boxed_result("idle", subsystem); // Handle some directly here
        match subsystem {
            Subsystem::Database => {
                // Database changed after updating. Perform a reconnection,
                // which will also trigger views to refresh their contents.
                let (s, r) = oneshot::channel();
                let _ = self.background(Task::Connect(s), r).await;
            }
            // More to come
            _ => {}
        }
    }

    pub async fn disconnect(&self, stop: bool, end_state: ConnectionState) -> ClientResult<()> {
        // Clients might be currently disconnected so don't exit on error.
        // In case both are running, disconnect the background first as we need to use
        // the foreground client to wake it up.
        let (s, r) = oneshot::channel();
        self.background(Task::Disconnect(stop, s), r).await?;
        let (s, r) = oneshot::channel();
        self.foreground(Task::Disconnect(stop, s), r).await?;
        self.state.set_connection_state(end_state);
        self.client_version.take();
        Ok(())
    }

    async fn handle_error<T>(&self, res: ClientResult<T>) -> ClientResult<T> {
        if let Err(e) = &res {
            match e {
                ClientError::Mpd(e) => {
                    match e {
                        MpdError::Io(_e) => {
                            self.state
                                .set_connection_state(ConnectionState::NotConnected);
                            // TODO
                        }
                        MpdError::Parse(_e) => {}
                        MpdError::Proto(_e) => {}
                        MpdError::Server(e) => {
                            match e.code {
                                MpdErrorCode::Password => {
                                    self.state
                                        .set_connection_state(ConnectionState::WrongPassword);
                                }
                                MpdErrorCode::Permission => {
                                    self.state
                                        .set_connection_state(ConnectionState::Unauthenticated);
                                }
                                _ => {
                                    // TODO
                                }
                            }
                        }
                    }
                }
                ClientError::NotConnected | ClientError::Socket | ClientError::Tcp => {
                    self.state
                        .set_connection_state(ConnectionState::NotConnected);
                }
                _ => {
                    // TODO
                }
            }
        }

        res
    }

    async fn handle_connect_error(&self, res: ClientResult<Version>) -> ClientResult<Version> {
        match &res {
            Err(e) => match e {
                ClientError::Mpd(MpdError::Server(e)) => match e.code {
                    MpdErrorCode::Password => {
                        self.state
                            .set_connection_state(ConnectionState::WrongPassword);
                    }
                    MpdErrorCode::Permission => {
                        self.state
                            .set_connection_state(ConnectionState::Unauthenticated);
                    }
                    _ => {
                        self.state
                            .set_connection_state(ConnectionState::NotConnected);
                    }
                },
                ClientError::Socket => {
                    self.state
                        .set_connection_state(ConnectionState::SocketNotFound);
                }
                ClientError::Tcp => {
                    self.state
                        .set_connection_state(ConnectionState::ConnectionRefused);
                }
                ClientError::CredentialStore => {
                    self.state
                        .set_connection_state(ConnectionState::CredentialStoreError);
                }
                _ => {
                    self.state
                        .set_connection_state(ConnectionState::NotConnected);
                }
            },
            _ => {
                self.state
                    .set_connection_state(ConnectionState::NotConnected);
            }
        }
        res
    }

    pub async fn connect(&self) -> ClientResult<()> {
        // Disconnect both clients.
        if let Err(e) = self.disconnect(false, ConnectionState::Connecting).await {
            eprintln!("Warning: did not cleanly disconnect");
            dbg!(e);
        }

        let (s, r) = oneshot::channel();
        self.fg_sender
            .send(Task::Connect(s))
            .await
            .expect("Broken FG sender");
        let version = self
            .handle_connect_error(r.await.expect("Broken oneshot receiver"))
            .await?;

        // Figure out stickers support early as we need to decide whether we should show the Dynamic Playlists page.
        // Set to maximum supported level first by MPD version.
        if version.1 < 24 {
            self.state
                .set_stickers_support_level(StickersSupportLevel::SongsOnly);
        } else {
            self.state
                .set_stickers_support_level(StickersSupportLevel::All);
        }
        // Now test if stickers DB is enabled by querying for a made-up path. This will most likely
        // return an error but as long as that error isn't an "unknown command" one, the sticker DB
        // is enabled.
        if let Err(ClientError::Mpd(MpdError::Server(e))) = self
            .get_common_stickers("song", String::from("euphonica_sticker_test"))
            .await
            && e.code == MpdErrorCode::UnknownCmd
        {
            println!("Sticker DB not enabled. Disabling stickers-related functionality...");
            self.state
                .set_stickers_support_level(StickersSupportLevel::Disabled);
        }
        self.client_version.replace(Some(version));

        let (s, r) = oneshot::channel();
        self.bg_sender
            .send(Task::Connect(s))
            .await
            .expect("Broken BG sender");
        self.handle_connect_error(r.await.expect("Broken oneshot receiver"))
            .await?;

        self.state.set_connection_state(ConnectionState::Connected);
        Ok(())
    }

    async fn foreground<T>(
        &self,
        task: Task,
        receiver: oneshot::Receiver<ClientResult<T>>,
    ) -> ClientResult<T> {
        // Decrement when this function returns OR when the future is dropped
        // (aborted by a view navigating away), exactly once in both cases.
        let _guard = TaskGuard::new(self.state.clone(), false);
        self.fg_sender.send(task).await.expect("Broken FG sender");
        self.handle_error(receiver.await.expect("Broken oneshot receiver"))
            .await
    }

    async fn background<T>(
        &self,
        task: Task,
        receiver: oneshot::Receiver<ClientResult<T>>,
    ) -> ClientResult<T> {
        // Decrement when this function returns OR when the future is dropped
        // (aborted by a view navigating away), exactly once in both cases.
        let _guard = TaskGuard::new(self.state.clone(), true);
        self.bg_sender.send(task).await.expect("Broken BG sender");
        // Wake background thread
        let (s, r) = oneshot::channel();
        // Ignore errors here, client might be reconnecting itself
        let _ = self
            .foreground(Task::SendMessage(String::from("wake"), s), r)
            .await;
        self.handle_error(receiver.await.expect("Broken oneshot receiver"))
            .await
    }

    pub async fn get_volume(&self) -> ClientResult<i8> {
        let (s, r) = oneshot::channel();
        self.foreground(Task::GetVolume(s), r).await
    }

    pub async fn set_volume(&self, vol: i8) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.foreground(Task::SetVolume(vol, s), r).await
    }

    pub async fn get_outputs(&self) -> ClientResult<Vec<Output>> {
        let (s, r) = oneshot::channel();
        self.foreground(Task::GetOutputs(s), r).await
    }

    pub async fn set_output(&self, id: u32, state: bool) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.foreground(Task::SetOutput(id, state, s), r).await
    }

    // Special handling for stickers, run AFTER the general error handling logic.
    fn handle_sticker_error<T>(&self, res: ClientResult<T>) -> ClientResult<T> {
        if let Err(ClientError::Mpd(MpdError::Server(e))) = &res {
            match e.code {
                MpdErrorCode::UnknownCmd => {
                    self.state
                        .set_stickers_support_level(StickersSupportLevel::Disabled);
                }
                MpdErrorCode::Argument => {
                    self.state
                        .set_stickers_support_level(StickersSupportLevel::SongsOnly);
                }
                _ => {}
            }
        }
        res
    }

    pub async fn get_sticker(
        &self,
        typ: &'static str,
        uri: String,
        name: Cow<'static, str>,
    ) -> ClientResult<String> {
        let min_lvl = if typ == "song" {
            StickersSupportLevel::SongsOnly
        } else {
            StickersSupportLevel::All
        };
        if self.state.stickers_support_level() >= min_lvl {
            let (s, r) = oneshot::channel();
            self.handle_sticker_error(
                self.foreground(Task::GetSticker(typ, uri, name, s), r)
                    .await,
            )
        } else {
            Err(ClientError::InsufficientStickersSupportLevel)
        }
    }

    /// Fetch stickers commonly used by other MPD clients, such as myMPD, and parse them into
    /// a Stickers object.
    /// This does NOT fetch Euphonica-specific stickers, such as album and artist metadata.
    pub async fn get_common_stickers(
        &self,
        typ: &'static str,
        uri: String,
    ) -> ClientResult<Stickers> {
        let min_lvl = if typ == "song" {
            StickersSupportLevel::SongsOnly
        } else {
            StickersSupportLevel::All
        };
        if self.state.stickers_support_level() >= min_lvl {
            let (s, r) = oneshot::channel();
            self.handle_sticker_error(
                self.foreground(Task::GetCommonStickers(typ, uri, s), r)
                    .await,
            )
        } else {
            Err(ClientError::InsufficientStickersSupportLevel)
        }
    }

    pub async fn set_sticker(
        &self,
        typ: &'static str,
        uri: String,
        name: Cow<'static, str>,
        value: Cow<'static, str>,
        mode: StickerSetMode,
    ) -> ClientResult<()> {
        let min_lvl = if typ == "song" {
            StickersSupportLevel::SongsOnly
        } else {
            StickersSupportLevel::All
        };
        if self.state.stickers_support_level() >= min_lvl {
            let (s, r) = oneshot::channel();
            self.handle_sticker_error(
                self.foreground(Task::SetSticker(typ, uri, name, value, mode, s), r)
                    .await,
            )
        } else {
            Err(ClientError::InsufficientStickersSupportLevel)
        }
    }

    /// Fetch all stickers for a given type/uri and return them as a HashMap.
    /// Used internally to read paged metadata documents.
    async fn get_stickers(
        &self,
        typ: &'static str,
        uri: &str,
        names: Vec<Cow<'static, str>>,
    ) -> ClientResult<Vec<(String, String)>> {
        let min_lvl = if typ == "song" {
            StickersSupportLevel::SongsOnly
        } else {
            StickersSupportLevel::All
        };
        if self.state.stickers_support_level() >= min_lvl {
            let (s, r) = oneshot::channel();
            self.handle_sticker_error(
                self.foreground(Task::GetStickers(typ, uri.to_string(), names, s), r)
                    .await,
            )
        } else {
            Err(ClientError::InsufficientStickersSupportLevel)
        }
    }

    pub async fn delete_sticker(
        &self,
        typ: &'static str,
        uri: String,
        name: Cow<'static, str>,
    ) -> ClientResult<()> {
        let min_lvl = if typ == "song" {
            StickersSupportLevel::SongsOnly
        } else {
            StickersSupportLevel::All
        };
        if self.state.stickers_support_level() >= min_lvl {
            let (s, r) = oneshot::channel();
            self.handle_sticker_error(
                self.foreground(Task::DeleteSticker(typ, uri, name, s), r)
                    .await,
            )
        } else {
            Err(ClientError::InsufficientStickersSupportLevel)
        }
    }

    /// Atomically delete multiple stickers for a given type/uri. All names are deleted in a
    /// single MPD command list, ensuring atomicity — either all succeed or none do.
    pub async fn delete_stickers(
        &self,
        typ: &'static str,
        uri: String,
        names: Vec<Cow<'static, str>>,
    ) -> ClientResult<()> {
        let min_lvl = if typ == "song" {
            StickersSupportLevel::SongsOnly
        } else {
            StickersSupportLevel::All
        };
        if self.state.stickers_support_level() >= min_lvl {
            let (s, r) = oneshot::channel();
            self.handle_sticker_error(
                self.foreground(Task::DeleteStickers(typ, uri, names, s), r)
                    .await,
            )
        } else {
            Err(ClientError::InsufficientStickersSupportLevel)
        }
    }

    /// Atomically set multiple stickers for a given object. All pairs are written in a single
    /// MPD command list. SHOULD make it atomic (dunno, need to check with MPD source).
    pub async fn set_stickers(
        &self,
        typ: &'static str,
        uri: String,
        names_values: Vec<(Cow<'static, str>, Cow<'static, str>)>,
    ) -> ClientResult<()> {
        let min_lvl = if typ == "song" {
            StickersSupportLevel::SongsOnly
        } else {
            StickersSupportLevel::All
        };
        if self.state.stickers_support_level() >= min_lvl {
            let (s, r) = oneshot::channel();
            self.handle_sticker_error(
                self.foreground(Task::SetStickers(typ, uri, names_values, s), r)
                    .await,
            )
        } else {
            Err(ClientError::InsufficientStickersSupportLevel)
        }
    }

    fn handle_playlist_error<T>(&self, res: ClientResult<T>) -> ClientResult<T> {
        if let Err(ClientError::Mpd(MpdError::Server(e))) = &res
            && e.detail.contains("disabled")
        {
            self.state.set_supports_playlists(false);
            println!("Playlists are not supported.");
        }
        res
    }

    pub async fn get_playlists(&self) -> ClientResult<Vec<INode>> {
        let (s, r) = oneshot::channel();
        self.handle_playlist_error(self.foreground(Task::GetPlaylists(s), r).await)
            .map(|infos| infos.into_iter().map(INode::from).collect::<Vec<INode>>())
    }

    pub async fn load_playlist(&self, name: String) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.handle_playlist_error(self.foreground(Task::LoadPlaylist(name, s), r).await)
    }

    pub async fn save_queue_as_playlist(
        &self,
        name: String,
        save_mode: SaveMode,
    ) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.handle_playlist_error(
            self.foreground(Task::SaveQueueAsPlaylist(name, save_mode, s), r)
                .await,
        )
    }

    pub async fn rename_playlist(&self, old_name: String, new_name: String) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.handle_playlist_error(
            self.foreground(Task::RenamePlaylist(old_name, new_name, s), r)
                .await,
        )
    }

    pub async fn edit_playlist(&self, actions: Vec<EditAction<'static>>) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.handle_playlist_error(self.foreground(Task::EditPlaylist(actions, s), r).await)
    }

    pub async fn delete_playlist(&self, name: String) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.handle_playlist_error(self.foreground(Task::DeletePlaylist(name, s), r).await)
    }

    pub async fn get_status(&self) -> ClientResult<Status> {
        // Stop borrowing main client as soon as possible
        let (s, r) = oneshot::channel();
        self.handle_playlist_error(self.foreground(Task::GetStatus(s), r).await)
    }

    /// Fetch the current queue in an asynchronous batchwise manner.
    pub async fn get_current_queue<F>(&self, respond: F) -> ClientResult<()>
    where
        F: Fn(Vec<Song>),
    {
        // This command is only called upon connection so we should drop the entire cache
        {
            self.song_cache.borrow_mut().clear();
        }
        let mut curr_len: usize = 0;
        let mut more: bool = true;
        while more && (curr_len) < FETCH_LIMIT {
            let (s, r) = oneshot::channel();
            match self
                .foreground(
                    Task::GetQueue(
                        Window::from((curr_len as u32, (curr_len + BATCH_SIZE) as u32)),
                        s,
                    ),
                    r,
                )
                .await
            {
                Ok(song_infos) => {
                    if !song_infos.is_empty() {
                        let mut res: Vec<Song> = Vec::with_capacity(song_infos.len());
                        // Cache
                        for mut song_info in song_infos.into_iter() {
                            if let Some(id) = song_info.queue_id {
                                let song = Song::from(std::mem::take(&mut song_info));
                                res.push(song.clone()); // lightweight Rc
                                self.song_cache.borrow_mut().put(id, song);
                            }
                        }
                        curr_len += BATCH_SIZE;
                        respond(res);
                    } else {
                        more = false;
                    }
                }
                Err(e) => {
                    if let ClientError::Mpd(MpdError::Server(se)) = &e {
                        if se.detail == "Bad song index" {
                            // Gracefully handle end-of-queue instead of returning an error
                            more = false;
                        } else {
                            return Err(e);
                        }
                    } else {
                        return Err(e);
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn get_queue_changes<F>(
        &self,
        curr_version: u32,
        total_len: u32,
        respond: F,
    ) -> ClientResult<()>
    where
        F: Fn(Vec<Song>),
    {
        let mut curr_len: usize = 0;
        while curr_len < total_len as usize {
            let (s, r) = oneshot::channel();
            let changes = self
                .background(
                    Task::GetQueueChanges(
                        curr_version,
                        Window::from((curr_len as u32, (curr_len + BATCH_SIZE) as u32)),
                        s,
                    ),
                    r,
                )
                .await?;
            if !changes.is_empty() {
                // Map to songs.
                let mut songs: Vec<Song> = Vec::with_capacity(changes.len());
                for change in changes.into_iter() {
                    let cached_song;
                    {
                        cached_song = self.song_cache.borrow_mut().get(&change.id.0).cloned();
                    }
                    if let Some(cached_song) = cached_song {
                        cached_song.set_queue_pos(change.pos);
                        songs.push(cached_song);
                    } else {
                        let (s, r) = oneshot::channel();
                        if let Some(song_info) = self
                            .background(Task::GetSongAtQueueId(change.id, s), r)
                            .await?
                        {
                            let song = Song::from(song_info);
                            self.song_cache.borrow_mut().put(change.id.0, song.clone());
                            songs.push(song);
                        } else {
                            // Queue has probably changed again. Push empty song &
                            // wait for next refresh.
                            let mut si = SongInfo::default();
                            si.queue_id = Some(change.id.0);
                            si.queue_pos = Some(change.pos);
                            songs.push(si.into());
                        }
                    }
                }
                respond(songs);
            }
            curr_len += BATCH_SIZE;
        }
        Ok(())
    }

    pub async fn get_song_at_queue_id(
        &self,
        id: Id,
        fetch_stickers: bool,
    ) -> ClientResult<Option<Song>> {
        let (s, r) = oneshot::channel();
        if let Some(song_info) = self.foreground(Task::GetSongAtQueueId(id, s), r).await? {
            let res = Song::from(song_info);
            if fetch_stickers {
                // Error handling is already performed for us
                if let Ok(stickers) = self
                    .get_common_stickers("song", res.get_uri().to_owned())
                    .await
                {
                    res.set_stickers(stickers);
                }
            }
            Ok(Some(res))
        } else {
            Ok(None)
        }
    }

    pub async fn set_playback_flow(&self, flow: PlaybackFlow) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.foreground(Task::SetPlaybackFlow(flow, s), r).await
    }

    pub async fn set_crossfade(&self, fade: f64) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.foreground(Task::SetCrossfade(fade as i64, s), r).await
    }

    pub async fn set_replaygain(&self, mode: mpd::status::ReplayGain) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.foreground(Task::SetReplayGain(mode, s), r).await
    }

    pub async fn set_mixramp_db(&self, db: f32) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.foreground(Task::SetMixRampDb(db, s), r).await
    }

    pub async fn set_mixramp_delay(&self, delay: f64) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.foreground(Task::SetMixRampDelay(delay, s), r).await
    }

    pub async fn set_random(&self, state: bool) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.foreground(Task::SetRandom(state, s), r).await
    }

    pub async fn set_consume(&self, state: bool) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.foreground(Task::SetConsume(state, s), r).await
    }

    pub async fn pause(&self, is_pause: bool) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.foreground(Task::Pause(is_pause, s), r).await
    }

    pub async fn stop(&self) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.foreground(Task::Stop(s), r).await
    }

    pub async fn prev(&self) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.foreground(Task::Prev(s), r).await
    }

    pub async fn next(&self) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.foreground(Task::Next(s), r).await
    }

    pub async fn play_at(&self, id_or_pos: u32, is_id: bool) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        if is_id {
            self.foreground(Task::PlayAtId(Id(id_or_pos), s), r).await
        } else {
            self.foreground(Task::PlayAtPos(id_or_pos, s), r).await
        }
    }

    pub async fn swap_pos(&self, pos1: u32, pos2: u32) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.foreground(Task::SwapPos(pos1, pos2, s), r).await
    }

    pub async fn move_id(&self, from_id: u32, to_pos: usize) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.foreground(Task::MoveId(from_id, to_pos, s), r).await
    }

    pub async fn delete_at_pos(&self, pos: u32) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.foreground(Task::DeleteAtPos(pos, s), r).await
    }

    pub async fn clear_queue(&self) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.foreground(Task::ClearQueue(s), r).await
    }

    pub async fn seek_current_song(&self, position: f64) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.foreground(Task::Seek(position, s), r).await
    }

    pub async fn update_db(&self) -> ClientResult<u32> {
        let (s, r) = oneshot::channel();
        self.foreground(Task::UpdateDb(s), r).await
    }

    pub async fn get_embedded_cover(
        &self,
        uri: String,
    ) -> ClientResult<Option<ImageHandle>> {
        // Leave downloading to the background connection thread, but local operations
        // (resizing, transcoding, writing to disk) will be done with a threadpool, whose
        // handle is held by the this thread (the main one).
        let (s, r) = oneshot::channel();
        self.background(Task::GetEmbeddedCover(uri, None, s), r).await
    }

    pub async fn get_folder_cover(
        &self,
        example_uri: String,
        folder_uri: String,
    ) -> ClientResult<Option<ImageHandle>> {
        let (s, r) = oneshot::channel();
        self
            .background(Task::GetFolderCover(example_uri, Some(folder_uri), s), r)
            .await
    }

    /// Fetch albums and albumartists together for efficiency.
    /// This is because fetching albums require albumartists to disambiguate same-named albums from different artists,
    /// while albumartists require example URIs for display purposes.
    /// Algorithm:
    /// 1. Fetch (albumartist tag, album) pairs
    /// 2. For each (albumartist tag, album) pair, fetch one song entry to glean information from it. Create the album object
    ///    as usual, but also extract the albumartists into a hashmap. In case some of the albumartists are already present
    ///    in the hashmap (due to them being present in another album, just append the album's example URI to their list of
    ///    example albums.
    pub async fn get_albums_and_albumartists_by_query(
        &self,
        query: Query<'static>,
        fetch_stickers: bool,
    ) -> ClientResult<(Vec<Album>, Vec<Artist>)> {
        // TODO: batched windowed retrieval
        // Most of the below logic are asyncified to avoid blocking the main UI thread.
        let asyncified = Asyncified::builder().build_ok(|| ()).await;

        let (s, r) = oneshot::channel();
        let grouped_vals = self
            .foreground(
                Task::List(
                    Term::Tag(tags::ALBUM.into()),
                    query,
                    Some(tags::ALBUMARTIST),
                    s,
                ),
                r,
            )
            .await?;

        let (album_count, chunked_queries_windows): (usize, Vec<Vec<(Query, Window)>>) = asyncified
            .call(move |_| {
                let mut all_queries_windows = Vec::new();
                for (key, tags) in grouped_vals.groups.into_iter() {
                    all_queries_windows.reserve(tags.len());
                    for tag in tags.into_iter() {
                        // Only care if tag is not empty
                        if !tag.is_empty() {
                            let mut query = Query::new();
                            query.and(Term::Tag(Cow::Borrowed(tags::ALBUM)), tag);
                            query.and(Term::Tag(Cow::Borrowed(tags::ALBUMARTIST)), key.clone());
                            all_queries_windows.push((query, Window::from((0, 1))));
                        }
                    }
                }
                // Chunk the queries to avoid timing out on slow servers. The below messy code actually
                // gives owned chunks without cloning.
                (
                    all_queries_windows.len(),
                    all_queries_windows
                        .into_iter()
                        .chunks(256)
                        .into_iter()
                        .map(|chunk| chunk.collect())
                        .collect(),
                )
            })
            .await;

        // Now we can fetch song entries chunk by chunk.
        let mut songs: Vec<SongInfo> = Vec::with_capacity(album_count);
        for chunk in chunked_queries_windows {
            let (s, r) = oneshot::channel();
            songs.append(
                &mut self
                    .foreground(
                        Task::FindMultiple(
                            chunk,
                            Some(vec![
                                tags::ALBUM,
                                tags::ARTIST,  // as fallback
                                tags::ALBUMARTIST,
                                tags::ALBUMARTISTSORT,
                                tags::ALBUMARTIST_MBID,
                                tags::ALBUM_MBID,
                                tags::ORIGINAL_DATE,  // as fallback
                                tags::DATE,
                                tags::GENRE,
                            ]),
                            s,
                        ),
                        r,
                    )
                    .await?,
            );
        }

        // Then fetch album ratings.
        // New scheme: "albumRating" sticker attached to filter expression URIs (unique per album).
        // Legacy fallback: "rating" sticker attached to album name (collides for same-named albums).
        let mut ratings_map: FxHashMap<String, String> = FxHashMap::default();
        let mut legacy_ratings_map: FxHashMap<String, String> = FxHashMap::default();
        if fetch_stickers {
            // 1) Fetch new-style ratings (filter-based).
            self.find_sticker(
                "filter",
                String::new(), // empty URI = search all filter expressions
                Stickers::RATING.into(),
                &mut |stickers: Vec<(String, String)>| {
                    for (filter_expr, value) in stickers {
                        ratings_map.insert(filter_expr.to_lowercase(), value);
                    }
                },
            )
            .await?;
            // 2) Legacy fallback: fill in ratings for albums that have no new-style rating.
            self.find_sticker(
                "album",
                String::new(), // empty URI = search all albums
                Stickers::RATING.into(),
                &mut |stickers: Vec<(String, String)>| {
                    for (name, value) in stickers {
                        legacy_ratings_map.insert(name, value);
                    }
                },
            )
            .await?;
        }

        // Yet more off thread work
        let (album_infos_and_ratings, artist_infos) = asyncified
            .call(move |_| {
                let mut albumartists: FxHashMap<String, ArtistInfo> = FxHashMap::default();
                let mut album_infos_and_ratings: Vec<(AlbumInfo, Option<String>)> =
                    Vec::with_capacity(album_count);

                for i in 0..songs.len() {
                    if let Some(album_info) = std::mem::take(&mut songs[i]).into_album_info() {
                        // Handle artist first
                        let example_uri = &album_info.example_uri;
                        for artist in album_info.artists.iter() {
                            let comp_id = artist.get_comp_id();
                            let existing =
                                albumartists.entry(comp_id.to_string()).or_insert_with(|| {
                                    // Haven't seen this artist before => push new
                                    artist.to_owned()
                                });

                            existing.insert_genres(&album_info.genres);

                            // Slack off here (we'll not use all of them anyway; alloc only what's needed)
                            if existing.example_uris.len() < MAX_EXAMPLE_ALBUMS_PER_ALBUMARTIST {
                                existing.example_uris.push(example_uri.to_owned());
                            }
                        }
                        let rating;
                        if fetch_stickers {
                            // Prefer new-style filter-based rating; fall back to legacy title-based rating.
                            // Force case-insensitive comparison for now as MPD mangles the expression string (all terms become uppercase there).
                            let filter_expr = album_info.get_filter_expression().to_lowercase();
                            rating = ratings_map
                                .get(&filter_expr)
                                .cloned()
                                .or_else(|| legacy_ratings_map.get(&album_info.title).cloned());
                        } else {
                            rating = None;
                        }
                        album_infos_and_ratings.push((album_info, rating));
                    }
                }

                (
                    album_infos_and_ratings,
                    albumartists
                        .into_iter()
                        .map(|p| p.1)
                        .collect::<Vec<ArtistInfo>>(),
                )
            })
            .await;
        Ok((
            album_infos_and_ratings
                .into_iter()
                .map(|(i, r)| {
                    let res = Album::from(i);
                    if let Some(rating) = r {
                        res.get_stickers().borrow_mut().set_rating(&rating);
                    }
                    res
                })
                .collect(),
            artist_infos.into_iter().map(Artist::from).collect(),
        ))
    }

    pub async fn get_distinct_genres<F>(&self, respond: &mut F) -> ClientResult<()>
    where
        F: FnMut(Vec<String>),
    {
        let (s, r) = oneshot::channel();
        let split_genres: Vec<String> = self.foreground(Task::ListGenres(s), r).await?;
        respond(split_genres);
        Ok(())
    }

    pub async fn get_recent_albums(&self) -> ClientResult<Vec<Album>> {
        let settings = utils::settings_manager().child("library");
        let n = settings.uint("n-recent-albums");

        // Build queries off-thread from SQLite results
        let asyncified = Asyncified::builder().build_ok(|| ()).await;
        let queries_windows: Vec<(Query, Window)> = asyncified
            .call(move |_| {
                let recent_albums = sqlite::get_last_n_albums(n).expect("Sqlite DB error");
                recent_albums
                    .into_iter()
                    .map(|(album, artist, mbid)| {
                        let mut query = Query::new();
                        query.and(Term::Tag(tags::ALBUM.into()), album);
                        if let Some(a) = artist {
                            query.and(Term::Tag(tags::ALBUMARTIST.into()), a);
                        }
                        if let Some(m) = mbid {
                            query.and(Term::Tag(tags::ALBUM_MBID.into()), m);
                        }
                        (query, Window::from((0, 1)))
                    })
                    .collect()
            })
            .await;

        let (s, r) = oneshot::channel();
        Ok(self
            .foreground(
                Task::FindMultiple(
                    queries_windows,
                    Some(vec![tags::ALBUM, tags::ALBUMARTIST, tags::ALBUM_MBID]),
                    s,
                ),
                r,
            )
            .await?
            .into_iter()
            .map(|si| si.into_album_info())
            .filter(|maybe_info| maybe_info.is_some())
            .map(|maybe_info| maybe_info.unwrap().into())
            .collect())
    }

    /// Alternative to get_songs_by_query that does not wrap SongInfos in GObjects for efficiency
    /// in downstream processing.
    ///
    /// By default this is run on the background client. Pass use_fg = true to make use of the
    /// foreground client, e.g. when responding to user interactions.
    pub async fn get_song_infos_by_query<F>(
        &self,
        query: Query<'static>,
        use_fg: bool,
        respond: &mut F,
    ) -> ClientResult<()>
    where
        F: FnMut(Vec<SongInfo>),
    {
        let mut curr_len: usize = 0;
        let mut more: bool = true;
        while more && (curr_len) < FETCH_LIMIT {
            let (s, r) = oneshot::channel();
            let win = Window::from((curr_len as u32, (curr_len + BATCH_SIZE) as u32));
            let songs = if use_fg {
                self.foreground(Task::Find(query.clone(), win, s), r)
                    .await?
            } else {
                self.background(Task::Find(query.clone(), win, s), r)
                    .await?
            };
            if !songs.is_empty() {
                respond(songs);
                curr_len += BATCH_SIZE;
            } else {
                more = false;
            }
        }
        Ok(())
    }

    /// By default this is run on the background client. Pass use_fg = true to make use of the
    /// foreground client, e.g. when responding to user interactions.
    pub async fn get_songs_by_query<F>(
        &self,
        query: Query<'static>,
        use_fg: bool,
        respond: &mut F,
    ) -> ClientResult<()>
    where
        F: FnMut(Vec<Song>),
    {
        self.get_song_infos_by_query(query, use_fg, &mut |song_infos| {
            respond(
                song_infos
                    .into_iter()
                    .map(|mut si| Song::from(std::mem::take(&mut si)))
                    .collect(),
            )
        })
        .await
    }

    /// Fetch artists by artist tag. Will NOT fetch by albumartist (functionality already moved to get_albums_and_albumartists_by_query).
    /// This is more complicated than it sounds: we have to extract individual artists from multi-artist tags AND also assign genres to them,
    /// which themselves are also in composite (multi-genre) tags.
    /// 1. For each unique Artist tag (which can contain multiple artists), fetch all unique Genre tags (each can contain multiple genres).
    ///    The same unique multi-artist tag may be present in different songs which in turn may contain different sets of genres.
    ///    Split & deduplicate all genres for each artist tag.
    /// 2. For each unique Artist tag, fetch exactly ONE song with that tag so we can extract artistsort and MBID. This should be batched.
    /// 3. For each of the above songs (each associated with one of the unique artist tags), split into individual artists, enriching
    ///    existing artist object instances instead if already discovered from previous tags, then union with genres associated with this
    ///    song/artist tag (resolved in step 1).
    /// All of the above should be kept off the main thread as much as possible.
    pub async fn get_artists(&self) -> ClientResult<Vec<Artist>> {
        let tagtypes_to_load = [tags::ARTIST, tags::ARTISTSORT, tags::ARTIST_MBID];

        let (s, r) = oneshot::channel();
        let grouped_vals = self
            .foreground(
                Task::List(
                    Term::Tag(tags::GENRE.into()),
                    Query::new(),
                    Some(tags::ARTIST),
                    s,
                ),
                r,
            )
            .await?;

        let asyncified = Asyncified::builder().build_ok(|| ()).await;
        let (artist_tag_count, chunked_queries_windows, genres): (
            usize,
            Vec<Vec<(Query, Window)>>,
            Vec<FxHashSet<String>>,
        ) = asyncified
            .call(move |_| {
                let mut all_queries_windows: Vec<(Query, mpd::search::Window)> =
                    Vec::with_capacity(grouped_vals.groups.len());
                // Same order as the upcoming songs, so there's no need to use a hash table
                let mut genres = Vec::with_capacity(grouped_vals.groups.len());
                for (artist_tag, genre_tags) in grouped_vals.groups.into_iter() {
                    let mut query = Query::new();
                    query.and(Term::Tag(tags::ARTIST.into()), artist_tag.to_owned());
                    all_queries_windows.push((query, mpd::search::Window::from((0, 1))));
                    let mut genre_set = FxHashSet::default();
                    for genre_tag in genre_tags {
                        for genre in split_genre_tag(&genre_tag) {
                            let _ = genre_set.insert(genre.to_owned());
                        }
                    }
                    genres.push(genre_set);
                }
                // Chunk the queries to avoid timing out on slow servers. The below messy code actually
                // gives owned chunks without cloning.
                (
                    all_queries_windows.len(),
                    all_queries_windows
                        .into_iter()
                        .chunks(256)
                        .into_iter()
                        .map(|chunk| chunk.collect())
                        .collect(),
                    genres,
                )
            })
            .await;

        // Now we can fetch song entries chunk by chunk, then zip with genres.
        // THIS WILL BREAK if the number of returned songs is different from the length of the genres vec.
        // The above should never happen, since the tags used to look for songs were derived from the songs
        // in the first place, meaning each search should never return empty-handed.
        let mut songs: Vec<SongInfo> = Vec::with_capacity(artist_tag_count);
        for chunk in chunked_queries_windows {
            let (s, r) = oneshot::channel();
            songs.append(
                &mut self
                    .foreground(
                        Task::FindMultiple(chunk, Some(tagtypes_to_load.into()), s),
                        r,
                    )
                    .await?,
            );
        }

        // Yet more off thread work
        Ok(asyncified
            .call(move |_| {
                let mut res: FxHashMap<String, ArtistInfo> = FxHashMap::default();
                for (song, genres) in songs.into_iter().zip(genres) {
                    // Here we're fetching song.artists instead of song.album.artists
                    for artist in song.artists {
                        let existing =
                            res.entry(artist.get_comp_id().to_string())
                                .or_insert_with(|| {
                                    // Haven't seen this artist before => push new
                                    artist
                                });
                        existing.insert_genres(&genres);
                    }
                }
                res.into_iter()
                    .map(|(_, info)| info)
                    .collect::<Vec<ArtistInfo>>()
            })
            .await
            .into_iter()
            .map(Artist::from)
            .collect::<Vec<Artist>>())
    }

    pub async fn get_recent_artists(&self) -> ClientResult<Vec<Artist>> {
        let settings = utils::settings_manager().child("library");
        let n = settings.uint("n-recent-artists");

        // Build queries off-thread from SQLite results
        let asyncified = Asyncified::builder().build_ok(|| ()).await;
        let (queries_windows, recent_names_set): (Vec<(Query, Window)>, FxHashSet<String>) =
            asyncified
                .call(move |_| {
                    let recent_names = sqlite::get_last_n_artists(n).expect("Sqlite DB error");
                    let recent_names_set: FxHashSet<String> =
                        recent_names.iter().cloned().collect();
                    let queries_windows: Vec<(Query, Window)> = recent_names
                        .into_iter()
                        .map(|name| {
                            let mut query = Query::new();
                            query.and_with_op(
                                Term::Tag(Cow::Borrowed("artist")),
                                QueryOperation::Contains,
                                name,
                            );
                            (query, Window::from((0, 1)))
                        })
                        .collect();
                    (queries_windows, recent_names_set)
                })
                .await;

        let (s, r) = oneshot::channel();
        let songs = self
            .foreground(
                Task::FindMultiple(
                    queries_windows,
                    Some(vec![tags::ARTIST, tags::ARTIST_MBID]),
                    s,
                ),
                r,
            )
            .await?;

        // Deduplicate artists by comp_id, filtering to only those whose names
        // appear in the recent names set.
        Ok(asyncified
            .call(move |_| {
                let mut res = Vec::with_capacity(recent_names_set.len());
                let mut already_parsed: FxHashSet<String> = FxHashSet::default();
                for song in songs {
                    let artists = song.into_artist_infos();
                    for artist in artists.into_iter() {
                        if recent_names_set.contains(&artist.name)
                            && already_parsed.insert(artist.get_comp_id().to_owned())
                        {
                            res.push(artist)
                        }
                    }
                }
                res
            })
            .await
            .into_iter()
            .map(Artist::from)
            .collect())
    }

    pub async fn lsinfo(&self, path: String) -> ClientResult<Vec<INode>> {
        let (s, r) = oneshot::channel();
        self.foreground(Task::LsInfo(path, s), r)
            .await
            .map(|infos| infos.into_iter().map(INode::from).collect::<Vec<INode>>())
    }

    async fn get_playlist_song_infos<F>(&self, name: String, respond: &mut F) -> ClientResult<()>
    where
        F: FnMut(Vec<SongInfo>),
    {
        let client_version = self
            .client_version
            .borrow()
            .ok_or(ClientError::NotConnected)?;
        if client_version.1 < 24 {
            let (s, r) = oneshot::channel();
            let songs = self.background(Task::GetPlaylist(name, None, s), r).await?;
            if !songs.is_empty() {
                respond(songs);
            }
        } else {
            // For MPD 0.24+, use the new paged loading
            let mut curr_len: u32 = 0;
            let mut more: bool = true;
            while more && (curr_len as usize) < FETCH_LIMIT {
                let (s, r) = oneshot::channel();
                let songs = self
                    .background(
                        Task::GetPlaylist(
                            name.clone(),
                            Some(curr_len..(curr_len + BATCH_SIZE as u32)),
                            s,
                        ),
                        r,
                    )
                    .await?;
                more = songs.len() >= BATCH_SIZE;
                if !songs.is_empty() {
                    curr_len += songs.len() as u32;
                    respond(songs);
                }
            }
        }
        Ok(())
    }

    pub async fn get_playlist_songs<F>(&self, name: String, mut respond: F) -> ClientResult<()>
    where
        F: FnMut(Vec<Song>),
    {
        self.get_playlist_song_infos(name, &mut |song_infos: Vec<SongInfo>| {
            respond(
                song_infos
                    .into_iter()
                    .map(|mut si| Song::from(std::mem::take(&mut si)))
                    .collect(),
            )
        })
        .await
    }

    /// Convenience function to get a single song by URI using the background client.
    async fn get_song_by_uri(
        &self,
        uri: String,
        fetch_stickers: bool,
    ) -> ClientResult<Option<(SongInfo, Option<Stickers>)>> {
        let mut query = Query::new();
        query.and(Term::File, uri.clone());
        let (s, r) = oneshot::channel();
        let mut found_songs = self
            .foreground(Task::Find(query, Window::from((0, 1)), s), r)
            .await?;
        if !found_songs.is_empty() {
            let song = std::mem::take(&mut found_songs[0]);
            if fetch_stickers {
                // Error handling is already performed for us
                let maybe_stickers = self
                    .get_common_stickers("song", song.uri.to_owned())
                    .await
                    .ok();
                Ok(Some((song, maybe_stickers)))
            } else {
                Ok(Some((song, None)))
            }
        } else {
            Ok(None)
        }
    }

    pub async fn get_recent_songs(&self, n: u32) -> ClientResult<Vec<Song>> {
        let asyncified = Asyncified::builder().build_ok(|| ()).await;
        let (queries_windows, ts): (Vec<(Query, Window)>, Vec<OffsetDateTime>) = asyncified
            .call(move |_| {
                let resp = sqlite::get_last_n_songs(n).expect("Sqlite DB error");
                let ts = resp.iter().map(|(_, ts)| ts.to_owned()).collect();
                (
                    resp.into_iter()
                        .map(|(uri, _ts)| {
                            let mut q = Query::new();
                            q.and(Term::File, uri);
                            (q, Window::from((0, 1)))
                        })
                        .collect(),
                    ts,
                )
            })
            .await;

        let (s, r) = oneshot::channel();
        self.foreground(
            Task::FindMultiple(
                queries_windows,
                Some(vec![
                    tags::ALBUM,
                    tags::ARTIST,
                    tags::ALBUM_MBID,
                    tags::ALBUMARTIST, // Needed for cover art fetch :)
                    tags::ORIGINAL_DATE,
                ]),
                s,
            ),
            r,
        )
        .await?
        .into_iter()
        .zip(ts)
        .map(|(mut si, ts)| {
            si.last_played = Some(ts);
            Ok(si.into())
        })
        .collect()
    }

    /// Find all stickers of a given name for a path (recursive URI, or leave empty for other types).
    /// Returns a list of (name, value) pairs. Uses batched windowed retrieval
    /// to avoid overwhelming the server.
    pub async fn find_sticker<F>(
        &self,
        typ: &'static str,
        uri: String,
        name: Cow<'static, str>,
        respond: &mut F,
    ) -> ClientResult<()>
    where
        F: FnMut(Vec<(String, String)>),
    {
        let mut curr_len: usize = 0;
        let mut more: bool = true;
        while more && (curr_len) < FETCH_LIMIT {
            let (s, r) = oneshot::channel();
            let stickers = self
                .background(
                    Task::FindSticker(
                        typ,
                        uri.clone(),
                        name.clone(),
                        Window::from((curr_len as u32, (curr_len + BATCH_SIZE) as u32)),
                        s,
                    ),
                    r,
                )
                .await?;
            if !stickers.is_empty() {
                respond(stickers);
                curr_len += BATCH_SIZE;
            } else {
                more = false;
            }
        }
        Ok(())
    }

    pub async fn find_add(&self, query: Query<'static>) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.foreground(Task::FindAdd(query, s), r).await
    }

    /// When queuing multiple URIs, will use the background client & command list for efficiency.
    pub async fn add_multi(
        &self,
        mut uris: Vec<String>,
        recursive: bool,
        insert_pos: Option<usize>,
    ) -> ClientResult<()> {
        if uris.is_empty() {
            return Ok(());
        }
        if uris.len() > 1 {
            // Batch by batch to avoid holding the server up too long (and timing out)
            let mut inserted: usize = 0;
            while inserted < uris.len() {
                let to_insert = (uris.len() - inserted).min(BATCH_SIZE);
                let batch = uris[inserted..(inserted + to_insert)]
                    .iter_mut()
                    .map(std::mem::take)
                    .collect();
                if let Some(pos) = insert_pos {
                    let (s, r) = oneshot::channel();
                    self.background(Task::InsertMultiple(batch, pos, s), r)
                        .await?;
                } else {
                    let (s, r) = oneshot::channel();
                    self.background(Task::AddMultiple(batch, s), r).await?;
                }
                inserted += to_insert;
            }
        } else if recursive {
            // TODO: support inserting at specific location in queue
            let mut query = Query::new();
            query.and(Term::Base, std::mem::take(&mut uris[0]));
            self.find_add(query).await?;
        } else if let Some(pos) = insert_pos {
            let (s, r) = oneshot::channel();
            self.foreground(Task::Insert(std::mem::take(&mut uris[0]), pos, s), r)
                .await?;
        } else {
            let (s, r) = oneshot::channel();
            self.foreground(Task::Add(std::mem::take(&mut uris[0]), s), r)
                .await?;
        }

        Ok(())
    }

    pub async fn get_dynamic_playlist_songs(
        &self,
        dp: DynamicPlaylist,
        cache: bool, // If true, will cache resolved song URIs locally
    ) -> ClientResult<Vec<Song>> {
        let (s, r) = oneshot::channel();
        Ok(self
            .foreground(Task::ResolveDynamicPlaylist(dp, cache, s), r)
            .await?
            .into_iter()
            .map(Song::from)
            .collect())
    }

    pub async fn get_dynamic_playlist_songs_cached(&self, name: String) -> ClientResult<Vec<Song>> {
        let uris = gio::spawn_blocking(move || {
            sqlite::get_cached_dynamic_playlist_results(&name).map_err(|_| ClientError::Internal)
        })
        .await
        .unwrap()
        .map_err(|_| ClientError::Internal)?;
        let mut songs: Vec<Song> = Vec::with_capacity(uris.len());
        for uri in uris.into_iter() {
            if let Some(tup) = self.get_song_by_uri(uri, false).await? {
                songs.push(tup.0.into());
            }
        }
        Ok(songs)
    }

    pub async fn queue_cached_dynamic_playlist(&self, name: String) -> ClientResult<Vec<Id>> {
        let uris = gio::spawn_blocking(move || {
            sqlite::get_cached_dynamic_playlist_results(&name).map_err(|_| ClientError::Internal)
        })
        .await
        .unwrap()
        .map_err(|_| ClientError::Internal)?;
        let (s, r) = oneshot::channel();
        self.background(Task::AddMultiple(uris, s), r).await
    }

    #[inline]
    fn get_full_key_paged(base: &'static str, page: usize) -> Cow<'static, str> {
        format!("{}:{}", base, page).into()
    }

    /// Generate sticker names for the metadata document pages (0..page_count).
    /// Also includes the page count and last-modified stickers.
    fn generate_all_meta_sticker_names(page_count: usize) -> Vec<Cow<'static, str>> {
        (0..page_count)
            .map(|p| Self::get_full_key_paged(Stickers::META_DOC, p))
            .chain(std::iter::once({
                let base = Stickers::META_PAGE_COUNT;
                base.into()
            }))
            .chain(std::iter::once({
                let base = Stickers::META_LAST_MODIFIED;
                base.into()
            }))
            .collect()
    }

    /// Get metadata document as backed up to MPD's sticker database. Metadata is stored as
    /// BSON serialized to a base64 string, split across multiple sticker keys if it exceeds
    /// 2048 characters per sticker.
    pub async fn get_meta<T: for<'a> Deserialize<'a>>(
        &self,
        typ: &'static str,
        uri: String,
    ) -> ClientResult<Option<T>> {
        // First, fetch only the page count to know how many pages exist
        let page_count_key: String = Stickers::META_PAGE_COUNT.to_string();
        let page_count = self
            .get_sticker(typ, uri.clone(), page_count_key.into())
            .await?
            .parse::<usize>()
            .map_err(|_| ClientError::Parse)?;

        // Generate names for the pages only
        let page_names: Vec<Cow<'static, str>> = (0..page_count)
            .map(|p| Self::get_full_key_paged(Stickers::META_DOC, p))
            .collect();
        let pages = self.get_stickers(typ, &uri, page_names).await?;

        let mut combined: Vec<u8> = Vec::new();
        for (_, page) in pages {
            combined.extend_from_slice(&BASE64.decode(&page).map_err(|_| ClientError::Parse)?);
        }

        let doc = bson::Document::from_reader(&mut Cursor::new(&combined))
            .map_err(|_| ClientError::Parse)?;
        let meta = bson::deserialize_from_document::<T>(doc).map_err(|_| ClientError::Parse)?;
        Ok(Some(meta))
    }

    /// Sync metadata document to MPD's sticker database. Other Euphonica clients connected to the same
    /// server will be able to reuse this metadata document.
    /// Uses atomic command list to write both the document and last-modified stickers together,
    /// preventing partial syncs that would leave a document with a stale or missing timestamp.
    /// Metadata is serialized to BSON then base64-encoded. If the base64 string exceeds 2048
    /// characters, it is split across multiple sticker keys (euphonica:meta:doc:N).
    /// A pageCount sticker tracks the number of pages for reconstruction.
    /// After writing, any excess pages from a previous larger metadata document are cleaned up.
    pub async fn set_meta<T: Serialize>(
        &self,
        typ: &'static str,
        uri: String,
        meta: &T,
        last_modified: OffsetDateTime,
    ) -> ClientResult<()> {
        // TODO: Handle this in another thread to avoid blocking UI
        let b64 = bson::serialize_to_document(meta)
            .and_then(|res| bson::serialize_to_vec(&res))
            .and_then(|res| Ok(BASE64.encode(&res)))
            .map_err(|_| ClientError::Parse)?;

        // Split into 2048-char pages
        // This works because b64 is all ASCII
        let pages = b64.as_bytes().chunks(META_STICKER_PAGE_SIZE).into_iter();

        let mut names_values: Vec<(Cow<'static, str>, Cow<'static, str>)> = Vec::new(); // meta pages, plus page count and last-modified
        let mut new_page_count: usize = 0;
        for (idx, page) in pages.enumerate() {
            names_values.push((
                Self::get_full_key_paged(Stickers::META_DOC, idx),
                std::str::from_utf8(page)
                    .map_err(|_| ClientError::Parse)?
                    .to_owned()
                    .into(),
            ));
            new_page_count += 1;
        }

        // Write page count
        names_values.push((
            {
                let base = Stickers::META_PAGE_COUNT;
                base.into()
            },
            new_page_count.to_string().into(),
        ));

        // Write last-modified
        names_values.push((
            {
                let base = Stickers::META_LAST_MODIFIED;
                base.into()
            },
            last_modified.unix_timestamp_nanos().to_string().into(),
        ));

        // Clean up excess pages from a previous larger metadata document.
        // Read the old page count BEFORE the write so we don't compare against the new value.
        // Cleanup itself runs AFTER the write so that if it fails we don't lose data.
        let uri_for_cleanup = uri.clone();
        let old_page_count = self
            .get_sticker(typ, uri_for_cleanup, {
                let base = Stickers::META_PAGE_COUNT;
                base.into()
            })
            .await
            .ok()
            .and_then(|s| s.parse::<usize>().ok());

        // Perform the atomic write
        self.set_stickers(typ, uri.clone(), names_values).await?;

        if let Some(old_page_count) = old_page_count {
            if old_page_count > new_page_count {
                let excess_pages = old_page_count - new_page_count;
                let excess_start = new_page_count;
                let names: Vec<Cow<'static, str>> = (excess_start..excess_start + excess_pages)
                    .map(|p| Self::get_full_key_paged(Stickers::META_DOC, p))
                    .collect();
                // Ignore errors during cleanup (shouldn't affect metadata coherence).
                let _ = self.delete_stickers(typ, uri.clone(), names).await;
            }
        }

        Ok(())
    }

    /// Get the last-modified time of the backed-up metadata doc. If no metadata had been backed up,
    /// this returns None.
    pub async fn get_meta_last_modified(
        &self,
        typ: &'static str,
        uri: String,
    ) -> ClientResult<Option<OffsetDateTime>> {
        match self
            .get_sticker(typ, uri, {
                let base = Stickers::META_LAST_MODIFIED;
                base.into()
            })
            .await
        {
            Ok(unix_ts) => Ok(Some(
                OffsetDateTime::from_unix_timestamp_nanos(
                    unix_ts.parse::<i128>().map_err(|_| ClientError::Parse)?,
                )
                .map_err(|_| ClientError::Parse)?,
            )),
            Err(e) => {
                if matches!(e, ClientError::InsufficientStickersSupportLevel) {
                    Err(e)
                } else {
                    Ok(None)
                }
            }
        }
    }

    /// Clear all metadata stickers for a given type/uri.
    /// Reads the current page count, then atomically deletes all document pages,
    /// the page count sticker, and the last-modified sticker.
    pub async fn clear_meta(&self, typ: &'static str, uri: String) -> ClientResult<()> {
        // Read the current page count to know how many pages to delete
        let page_count_key: String = Stickers::META_PAGE_COUNT.to_owned();
        let page_count = self
            .get_sticker(typ, uri.clone(), page_count_key.into())
            .await?;
        let page_count = page_count
            .parse::<usize>()
            .map_err(|_| ClientError::Parse)?;

        if page_count == 0 {
            return Ok(());
        }

        // Generate all sticker names: doc pages + page count + last-modified
        let names = Self::generate_all_meta_sticker_names(page_count);

        // Atomically delete all of them
        self.delete_stickers(typ, uri, names).await
    }
}

impl Drop for MpdWrapper {
    fn drop(&mut self) {
        executor::block_on(async move {
            let _ = self.disconnect(true, ConnectionState::NotConnected).await;
        });
    }
}
