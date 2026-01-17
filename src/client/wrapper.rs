use async_channel::{Receiver, Sender};
use chrono::{DateTime, Duration, Local};
use futures::executor;
use glib::clone;
use gtk::{gio, glib};
use gtk::gio::prelude::*;
use lru::LruCache;
use mpd::{Query, Status, Term};
use mpd::search::{Operation as QueryOperation, Window};
use mpd::{
    Channel, EditAction, Output, SaveMode, Subsystem, Version,
    error::{Error as MpdError, ErrorCode as MpdErrorCode},
    song::Id,
};
use nohash_hasher::NoHashHasher;
use rand::seq::SliceRandom;
use rustc_hash::FxHashSet;
use time::OffsetDateTime;
use zbus::{Connection as ZConnection, Proxy as ZProxy};

use std::borrow::Cow;
use std::cmp::Ordering as StdOrdering;
use std::hash::BuildHasherDefault;
use std::num::NonZero;
use std::thread;
use std::{
    cell::RefCell,
    rc::Rc,
};
use uuid::Uuid;

use crate::cache::sqlite;
use crate::common::DynamicPlaylist;
use crate::common::dynamic_playlist::{QueryLhs, StickerObjectType, StickerOperation, Ordering};
use crate::utils::settings_manager;
use crate::{
    common::{Album, AlbumInfo, Artist, INode, Song, SongInfo, Stickers, dynamic_playlist::{Rule}},
    player::PlaybackFlow,
    utils,
};

use super::connection::{Connection, Error as ClientError, Result as ClientResult, Task};
use super::state::{ClientState, ConnectionState, StickersSupportLevel};
use super::StickerSetMode;

static MAX_RETRIES: u32 = 3;

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

const BATCH_SIZE: usize = 128;
const FETCH_LIMIT: usize = 10000000; // Fetch at most ten million songs at once (same
// folder, same tag, etc)


fn get_past_unix_timestamp(backoff: i64) -> i64 {
    let current_local_dt: DateTime<Local> = Local::now();
    let backoff_dur: Duration = Duration::seconds(backoff);
    current_local_dt
        .checked_sub_signed(backoff_dur)
        .unwrap()
        .timestamp()
}


fn cmp_options_nulls_last<T: Ord>(a: Option<&T>, b: Option<&T>) -> StdOrdering {
    match (a, b) {
        (Some(val_a), Some(val_b)) => val_a.cmp(val_b),
        (Some(_), None) => StdOrdering::Less,
        (None, Some(_)) => StdOrdering::Greater,
        (None, None) => StdOrdering::Equal,
    }
}

// Reverse comparison, but still putting nulls last
fn reverse_cmp_options_nulls_last<T: Ord>(a: Option<&T>, b: Option<&T>) -> StdOrdering {
    match (a, b) {
        (Some(val_a), Some(val_b)) => val_a.cmp(val_b).reverse(),
        (Some(_), None) => StdOrdering::Less,
        (None, Some(_)) => StdOrdering::Greater,
        (None, None) => StdOrdering::Equal,
    }
}

/// Build and return a dynamic comparator closure.
///
/// This is highly efficient because the logic for choosing which fields to compare
/// is determined *once* when this function is called.
pub fn build_comparator(
    orderings: &[Ordering],
) -> Box<dyn Fn(&(SongInfo, Stickers), &(SongInfo, Stickers)) -> StdOrdering> {
    let orderings = orderings.to_vec();
    Box::new(
        move |a: &(SongInfo, Stickers), b: &(SongInfo, Stickers)| -> StdOrdering {
            let song_a = &a.0;
            let stickers_a = &a.1;
            let song_b = &b.0;
            let stickers_b = &b.1;
            for ordering in &orderings {
                // Determine the ordering for the current rule's field.
                // Nulls are always sorted last as it wouldn't really make sense otherwise in
                // the dynamic playlist/all songs view cases.
                let res = match *ordering {
                    Ordering::AscAlbumTitle => cmp_options_nulls_last(
                        song_a.album.as_ref().map(|album: &AlbumInfo| &album.title),
                        song_b.album.as_ref().map(|album: &AlbumInfo| &album.title),
                    ),
                    Ordering::DescAlbumTitle => reverse_cmp_options_nulls_last(
                        song_a.album.as_ref().map(|album: &AlbumInfo| &album.title),
                        song_b.album.as_ref().map(|album: &AlbumInfo| &album.title),
                    ),
                    Ordering::Track => {
                        let track_a = song_a.track.unwrap_or(i64::MAX);
                        let track_b = song_b.track.unwrap_or(i64::MAX);
                        track_a.cmp(&track_b)
                    }
                    Ordering::AscReleaseDate => cmp_options_nulls_last(
                        song_a.release_date.as_ref(),
                        song_b.release_date.as_ref(),
                    ),
                    Ordering::DescReleaseDate => reverse_cmp_options_nulls_last(
                        song_a.release_date.as_ref(),
                        song_b.release_date.as_ref(),
                    ),
                    Ordering::AscArtistTag => cmp_options_nulls_last(
                        song_a.artist_tag.as_ref(),
                        song_b.artist_tag.as_ref(),
                    ),
                    Ordering::DescArtistTag => reverse_cmp_options_nulls_last(
                        song_a.artist_tag.as_ref(),
                        song_b.artist_tag.as_ref(),
                    ),
                    Ordering::AscRating => cmp_options_nulls_last(
                        stickers_a.rating.as_ref(),
                        stickers_b.rating.as_ref(),
                    ),
                    Ordering::DescRating => reverse_cmp_options_nulls_last(
                        stickers_a.rating.as_ref(),
                        stickers_b.rating.as_ref(),
                    ),
                    Ordering::AscLastModified => cmp_options_nulls_last(
                        song_a.last_modified.as_ref(),
                        song_b.last_modified.as_ref(),
                    ),
                    Ordering::DescLastModified => reverse_cmp_options_nulls_last(
                        song_a.last_modified.as_ref(),
                        song_b.last_modified.as_ref(),
                    ),
                    Ordering::AscPlayCount => cmp_options_nulls_last(
                        stickers_a.play_count.as_ref(),
                        stickers_b.play_count.as_ref(),
                    ),
                    Ordering::DescPlayCount => reverse_cmp_options_nulls_last(
                        stickers_a.play_count.as_ref(),
                        stickers_b.play_count.as_ref(),
                    ),
                    Ordering::AscSkipCount => cmp_options_nulls_last(
                        stickers_a.skip_count.as_ref(),
                        stickers_b.skip_count.as_ref(),
                    ),
                    Ordering::DescSkipCount => reverse_cmp_options_nulls_last(
                        stickers_a.skip_count.as_ref(),
                        stickers_b.skip_count.as_ref(),
                    ),
                    Ordering::Random => unreachable!(),
                };

                if res != StdOrdering::Equal {
                    return res;
                }
                // If equal, fall through to next rule
            }

            // If all rules resulted in equality, the items are considered equal.
            std::cmp::Ordering::Equal
        },
    )
}


#[derive(Debug)]
pub struct MpdWrapper {
    // Handles return bool to indicate whether the threads stopped due to an error
    // (true) or disconnection request (false).
    fg_handle: thread::JoinHandle<bool>,
    bg_handle: thread::JoinHandle<bool>,
    state: ClientState,
    fg_sender: Sender<Task>, // For sending tasks to the interactive client
    bg_sender: Sender<Task>, // For sending tasks to the background client
    client_version: RefCell<Option<Version>>,
    song_cache: RefCell<LruCache<u32, Song, BuildHasherDefault<NoHashHasher<u32>>>>
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
            fg_handle: thread::spawn(move || {
                Connection::new(fg_receiver, wake_channel, None, max_retries)
                    .run()
                    .is_err()
            }),
            bg_handle: thread::spawn(move || {
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
            ))
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
                let mut receiver =
                    std::pin::pin!(idle_receiver);

                while let Some(change) = receiver.next().await {
                    this.handle_idle_changes(change).await;
                }
            })
        );

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
                            println!("[KeepAlive] There is no client currently running. Won't ping.");
                        }
                        Err(e) => {dbg!(e);}
                    };
                    glib::timeout_future_seconds(ping_interval).await;
                }
            }));

        // A new loop to watch for system suspend/wake actions. We proactively disconnect before suspend
        // and connect again upon wake to avoid freezing upon a timed-out connection.
        let fg_sender = self.fg_sender.clone();
        let bg_sender = self.bg_sender.clone();
        utils::tokio_runtime().spawn(async move {
            let connection = ZConnection::system().await.unwrap();

            // Create a proxy for systemd-logind
            let proxy = ZProxy::new(
                &connection,
                "org.freedesktop.login1",         // The service name
                "/org/freedesktop/login1",        // The object path
                "org.freedesktop.login1.Manager", // The interface
            )
            .await
            .unwrap();

            // Get a stream of the "PrepareForSleep" signal
            use futures::prelude::*;
            let mut signal_stream =
                std::pin::pin!(proxy.receive_signal("PrepareForSleep").await.unwrap());
            // Loop forever, processing signals
            while let Some(signal) = signal_stream.next().await {
                if let Ok(going_to_sleep) = signal.body().deserialize::<bool>() {
                    if going_to_sleep {
                        println!("System is preparing to suspend. Disconnecting...");
                        let (s, r) = oneshot::channel();
                        fg_sender.send(Task::Disconnect(false, s)).await.expect("Broken FG sender");
                        let _ = r.await.expect("Broken oneshot receiver");
                        let (s, r) = oneshot::channel();
                        bg_sender.send(Task::Disconnect(false, s)).await.expect("Broken BG sender");
                        let _ = r.await.expect("Broken oneshot receiver");
                    } else {
                        println!("System has woken up. Reconnecting...");
                        let (s, r) = oneshot::channel();
                        fg_sender.send(Task::Connect(s)).await.expect("Broken FG sender");
                        let _ = r.await.expect("Broken oneshot receiver");
                        let (s, r) = oneshot::channel();
                        bg_sender.send(Task::Connect(s)).await.expect("Broken BG sender");
                        let _ = r.await.expect("Broken oneshot receiver");
                    }
                }
            }
        });
    }

    async fn handle_idle_changes(&self, subsystem: Subsystem) {
        self.state.emit_boxed_result("idle", subsystem); // Handle some directly here
        match subsystem {
            Subsystem::Database => {
                // Database changed after updating. Perform a reconnection,
                // which will also trigger views to refresh their contents.
                let (s, r) = oneshot::channel();
                let _ = self.background(Task::UpdateDb(s), r).await;
            }
            // More to come
            _ => {}
        }
    }

    pub async fn disconnect(&self, stop: bool) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.fg_sender.send(Task::Disconnect(stop, s)).await.expect("Broken FG sender");
        r.await.expect("Broken oneshot receiver")?;
        println!("Stopped foreground client");
        let (s, r) = oneshot::channel();
        self.bg_sender.send(Task::Disconnect(stop, s)).await.expect("Broken BG sender");
        r.await.expect("Broken oneshot receiver")?;
        println!("Stopped background client");
        self.state
            .set_connection_state(ConnectionState::NotConnected);
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
                        self.state.set_connection_state(ConnectionState::Unauthenticated);
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
        self.state.set_connection_state(ConnectionState::Connecting);

        let (s, r) = oneshot::channel();
        self.fg_sender.send(Task::Connect(s)).await.expect("Broken FG sender");
        let version = self.handle_connect_error(r.await.expect("Broken oneshot receiver")).await?;
        // Set to maximum supported level first. Any subsequent sticker command will then
        // update it to a lower state upon encountering related errors.
        // Euphonica relies on 0.24+ stickers capabilities. Disable if connected to
        // an older daemon.
        if version.1 < 24 {
            self.state
                .set_stickers_support_level(StickersSupportLevel::SongsOnly);
        } else {
            self.state
                .set_stickers_support_level(StickersSupportLevel::All);
        }
        self.client_version.replace(Some(version));

        let (s, r) = oneshot::channel();
        self.bg_sender.send(Task::Connect(s)).await.expect("Broken BG sender");
        self.handle_connect_error(r.await.expect("Broken oneshot receiver")).await?;

        self.state.set_connection_state(ConnectionState::Connected);
        Ok(())
    }

    async fn foreground<T>(
        &self,
        task: Task,
        receiver: oneshot::Receiver<ClientResult<T>>,
    ) -> ClientResult<T> {
        self.state.inc_fg();
        self.fg_sender.send(task).await.expect("Broken FG sender");
        let res = self.handle_error(receiver.await.expect("Broken oneshot receiver"))
            .await;
        self.state.dec_fg();
        res
    }

    async fn background<T>(
        &self,
        task: Task,
        receiver: oneshot::Receiver<ClientResult<T>>,
    ) -> ClientResult<T> {
        self.state.inc.bg();
        self.bg_sender.send(task).await.expect("Broken BG sender");
        // Wake background thread
        let (s, r) = oneshot::channel();
        self.foreground(Task::SendMessage(String::from("wake"), s), r)
            .await?;
        let res = self.handle_error(receiver.await.expect("Broken oneshot receiver"))
            .await;
        self.state.dec_bg();
        res
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

    pub async fn get_sticker(&self, typ: &'static str, uri: String, name: Cow<'static, str>) -> ClientResult<String> {
        let min_lvl = if typ == "song" {
            StickersSupportLevel::SongsOnly
        } else {
            StickersSupportLevel::All
        };
        if self.state.stickers_support_level() >= min_lvl {
            let (s, r) = oneshot::channel();
            self.handle_sticker_error(
                self.foreground(
                    Task::GetSticker(typ, uri, name, s),
                    r,
                )
                .await,
            )
        } else {
            Err(ClientError::InsufficientStickersSupportLevel)
        }
    }

    pub async fn get_known_stickers(&self, typ: &'static str, uri: String) -> ClientResult<Stickers> {
        let min_lvl = if typ == "song" {
            StickersSupportLevel::SongsOnly
        } else {
            StickersSupportLevel::All
        };
        if self.state.stickers_support_level() >= min_lvl {
            let (s, r) = oneshot::channel();
            self.handle_sticker_error(
                self.foreground(Task::GetKnownStickers(typ, uri, s), r).await,
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
                self.foreground(Task::SetSticker(typ, uri, name, value, mode, s), r).await,
            )
        } else {
            Err(ClientError::InsufficientStickersSupportLevel)
        }
    }

    pub async fn delete_sticker(&self, typ: &'static str, uri: String, name: Cow<'static, str>) -> ClientResult<()> {
        let min_lvl = if typ == "song" {
            StickersSupportLevel::SongsOnly
        } else {
            StickersSupportLevel::All
        };
        if self.state.stickers_support_level() >= min_lvl {
            let (s, r) = oneshot::channel();
            self.handle_sticker_error(
                self.foreground(
                    Task::DeleteSticker(typ, uri, name, s),
                    r,
                )
                .await,
            )
        } else {
            Err(ClientError::InsufficientStickersSupportLevel)
        }
    }

    fn handle_playlist_error<T>(&self, res: ClientResult<T>) -> ClientResult<T> {
        if let Err(ClientError::Mpd(MpdError::Server(e))) = &res {
            if e.detail.contains("disabled") {
                self.state.set_supports_playlists(false);
                println!("Playlists are not supported.");
            } else {
                println!("Playlist operation error: {e}");
                // TODO
            }
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
        self.handle_playlist_error(
            self.foreground(Task::LoadPlaylist(name, s), r)
                .await,
        )
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
            self.foreground(
                Task::RenamePlaylist(old_name, new_name, s),
                r,
            )
            .await,
        )
    }

    pub async fn edit_playlist(&self, actions: Vec<EditAction<'static>>) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.handle_playlist_error(self.foreground(Task::EditPlaylist(actions, s), r).await)
    }

    pub async fn delete_playlist(&self, name: String) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.handle_playlist_error(
            self.foreground(Task::DeletePlaylist(name, s), r)
                .await,
        )
    }

    pub async fn get_status(&self) -> ClientResult<Status> {
        // Stop borrowing main client as soon as possible
        let (s, r) = oneshot::channel();
        self.handle_playlist_error(self.foreground(Task::GetStatus(s), r).await)
    }

    /// Fetch the current queue in an asynchronous batchwise manner.
    pub async fn get_current_queue<F>(&self, respond: F) -> ClientResult<()>
    where F: Fn(Vec<Song>) {
        // This command is only called upon connection so we should drop the entire cache
        {
            self.song_cache.borrow_mut().clear();
        }
        let mut curr_len: usize = 0;
        let mut more: bool = true;
        while more && (curr_len) < FETCH_LIMIT {
            let (s, r) = oneshot::channel();
            let song_infos = self.background(
                Task::GetQueue(
                    Window::from((
                        curr_len as u32,
                        (curr_len + BATCH_SIZE) as u32,
                    )), s
                ), r
            ).await?;
            if !song_infos.is_empty() {
                let mut res: Vec<Song> = Vec::with_capacity(song_infos.len());
                // Cache
                for mut song_info in song_infos.into_iter() {
                    if let Some(id) = song_info.queue_id {
                        let song = Song::from(std::mem::take(&mut song_info));
                        res.push(song.clone());  // lightweight Rc
                        self.song_cache.borrow_mut().put(id, song);
                    }
                }
                curr_len += BATCH_SIZE;
                respond(res);
            } else {
                more = false;
            }
        }
        Ok(())
    }

    pub async fn get_queue_changes<F>(&self, curr_version: u32, total_len: u32, respond: F) -> ClientResult<()>
    where F: Fn(Vec<Song>) {
        let mut curr_len: usize = 0;
        while curr_len < total_len as usize {
            let (s, r) = oneshot::channel();
            let changes = self.background(Task::GetQueueChanges(
                curr_version,
                Window::from((curr_len as u32, (curr_len + BATCH_SIZE) as u32)),
                s
            ), r).await?;
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
                        if let Some(song_info) = self.background(Task::GetSongAtQueueId(change.id, s), r).await? {
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

    pub async fn get_song_at_queue_id(&self, id: Id, fetch_stickers: bool) -> ClientResult<Option<Song>> {
        let (s, r) = oneshot::channel();
        if let Some(song_info) = self.foreground(Task::GetSongAtQueueId(id, s), r).await? {
            let res = Song::from(song_info);
            if fetch_stickers {
                let (s, r) = oneshot::channel();
                res.set_stickers(self.foreground(
                    Task::GetKnownStickers("song", res.get_uri().to_owned(), s), r
                ).await?);
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
        }
        else {
            self.foreground(Task::PlayAtPos(id_or_pos, s), r).await
        }
    }

    pub async fn swap_pos(&self, pos1: u32, pos2: u32) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.foreground(Task::SwapPos(pos1, pos2, s), r).await
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

    pub async fn get_embedded_cover(&self, uri: String) -> ClientResult<Option<(String, String)>> {
        let (s, r) = oneshot::channel();
        self.background(Task::GetEmbeddedCover(uri, s), r).await
    }

    pub async fn get_folder_cover(&self, folder_uri: String) -> ClientResult<Option<(String, String)>> {
        let (s, r) = oneshot::channel();
        self.background(Task::GetFolderCover(folder_uri, s), r).await
    }

    pub async fn get_albums_by_query<F>(&self, query: Query<'static>, respond: &mut F) -> ClientResult<()>
    where F: FnMut(Album) {
        // TODO: batched windowed retrieval
        // Get list of unique album tags, grouped by albumartist
        // Will block child thread until info for all albums have been retrieved.
        let (s, r) = oneshot::channel();
        let grouped_vals = self.background(Task::List(
            Term::Tag(Cow::Borrowed("album")),
            query,
            Some("albumartist"),
            s
        ), r).await?;
        for (key, tags) in grouped_vals.groups.into_iter() {
            for tag in tags.iter() {

                let mut query = Query::new();
                query.and(Term::Tag(Cow::Borrowed("album")), tag.to_string());
                query.and(Term::Tag(Cow::Borrowed("albumartist")), key.to_string());
                let (s, r) = oneshot::channel();
                let mut songs = self.background(Task::Find(query, Window::from((0, 1)), s), r).await?;
                if !songs.is_empty() {
                    respond(
                        std::mem::take(&mut songs[0])
                            .into_album_info()
                            .expect("Fetched song by album tag but did not find album info")
                            .into()
                    );
                }
            }
        }
        Ok(())
    }

    pub async fn get_recent_albums<F>(&self, respond: &mut F) -> ClientResult<()> where F: FnMut(Album) {
        let settings = utils::settings_manager().child("library");
        // TODO: async this
        let recent_albums =
            sqlite::get_last_n_albums(settings.uint("n-recent-albums")).expect("Sqlite DB error");
        for tup in recent_albums.into_iter() {
            let mut query = Query::new();
            query.and(Term::Tag(Cow::Borrowed("album")), tup.0);
            if let Some(artist) = tup.1 {
                query.and(Term::Tag(Cow::Borrowed("albumartist")), artist);
            }
            if let Some(mbid) = tup.2 {
                query.and(Term::Tag(Cow::Borrowed("musicbrainz_albumid")), mbid);
            }
            self.get_albums_by_query(query, respond).await?;
        }
        Ok(())
    }

    async fn get_song_infos_by_query<F>(&self, query: Query<'static>, respond: &mut F) -> ClientResult<()>
    where F: FnMut(Vec<SongInfo>) {
        let mut curr_len: usize = 0;
        let mut more: bool = true;
        while more && (curr_len) < FETCH_LIMIT {
            let (s, r) = oneshot::channel();
            let songs = self.background(Task::Find(
                query.clone(),
                Window::from((curr_len as u32, (curr_len + BATCH_SIZE) as u32)),
                s
            ), r).await?;
            if !songs.is_empty() {
                respond(songs);
                curr_len += BATCH_SIZE;
            } else {
                more = false;
            }
        }
        Ok(())
    }

    pub async fn get_songs_by_query<F>(&self, query: Query<'static>, respond: &mut F) -> ClientResult<()>
    where F: FnMut(Vec<Song>) {
        self.get_song_infos_by_query(query, &mut |song_infos| {
            respond(
                song_infos
                    .into_iter()
                    .map(|mut si| Song::from(std::mem::take(&mut si)))
                    .collect()
            )
        }).await
    }

    pub async fn get_artists<F>(&self, use_album_artist: bool, respond: &mut F) -> ClientResult<()>
    where F: FnMut(Artist) {
        // Fetching artists is a bit more involved: artist tags usually contain multiple artists.
        // For the same reason, one artist can appear in multiple tags.
        // Here we'll reuse the artist parsing code in our SongInfo struct and put parsed
        // ArtistInfos in a Set to deduplicate them.
        let tag_type: &'static str = if use_album_artist {
            "albumartist"
        } else {
            "artist"
        };
        let mut already_parsed: FxHashSet<String> = FxHashSet::default();
        let (s, r) = oneshot::channel();
        let mut grouped_vals = self.background(
            Task::List(Term::Tag(Cow::Borrowed(tag_type)), Query::new(), None, s), r
        ).await?;
        // TODO: Limit tags to only what we need locally
        for mut tag in std::mem::take(&mut grouped_vals.groups[0].1).into_iter() {
            let mut query = Query::new();
            query.and(Term::Tag(Cow::Borrowed(tag_type)), std::mem::take(&mut tag));
            let (s, r) = oneshot::channel();
            let mut songs = self.background(
                Task::Find(query, Window::from((0, 1)), s), r
            ).await?;
            if !songs.is_empty() {
                let artists = std::mem::take(&mut songs[0]).into_artist_infos();
                for artist in artists.into_iter() {
                    if already_parsed.insert(artist.name.clone()) {
                        respond(artist.into());
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn get_recent_artists<F>(&self, respond: &F) -> ClientResult<()>
    where F: Fn(Artist) {
        let mut already_parsed: FxHashSet<String> = FxHashSet::default();
        let settings = utils::settings_manager().child("library");
        let n = settings.uint("n-recent-artists");
        let recent_names = sqlite::get_last_n_artists(n).expect("Sqlite DB error");
        let mut recent_names_set: FxHashSet<String> = FxHashSet::default();
        for name in recent_names.iter() {
            recent_names_set.insert(name.clone());
        }
        for name in recent_names.into_iter() {
            let mut query = Query::new();
            query.and_with_op(Term::Tag(Cow::Borrowed("artist")), QueryOperation::Contains, name);
            let (s, r) = oneshot::channel();
            let mut songs = self.background(Task::Find(query, Window::from((0, 1)), s), r).await?;
            if !songs.is_empty() {
                let artists = std::mem::take(&mut songs[0]).into_artist_infos();
                for artist in artists.into_iter() {
                    if recent_names_set.contains(&artist.name)
                        && already_parsed.insert(artist.name.clone())
                    {
                        respond(artist.into());
                    }
                }
            }
        }

        Ok(())
    }

    pub async fn lsinfo(&self, path: String) -> ClientResult<Vec<INode>> {
        let (s, r) = oneshot::channel();
        self.background(Task::LsInfo(path, s), r)
            .await.map(|infos| infos.into_iter().map(INode::from).collect::<Vec<INode>>())
    }

    async fn get_playlist_song_infos<F>(&self, name: String, respond: &mut F) -> ClientResult<()>
    where F: FnMut(Vec<SongInfo>) {
        let client_version = self.client_version.borrow().ok_or(ClientError::NotConnected)?;
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
                let songs = self.background(Task::GetPlaylist(
                    name.clone(), Some(curr_len..(curr_len + BATCH_SIZE as u32)), s
                ), r).await?;
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
    where F: FnMut(Vec<Song>) {
        self.get_playlist_song_infos(name, &mut |song_infos: Vec<SongInfo>| {
            respond(
                song_infos
                    .into_iter()
                    .map(|mut si| Song::from(std::mem::take(&mut si)))
                    .collect()
            )
        }).await
    }

    /// Convenience function to get a single song by URI using the background client.
    async fn get_song_by_uri(&self, uri: String, fetch_stickers: bool) -> ClientResult<Option<(SongInfo, Option<Stickers>)>> {
        let mut query = Query::new();
        query.and(Term::File, uri.clone());
        let (s, r) = oneshot::channel();
        let mut found_songs = self.background(Task::Find(query, Window::from((0, 1)), s), r).await?;
        if !found_songs.is_empty() {
            let song = std::mem::take(&mut found_songs[0]);
            if fetch_stickers {
                let (s, r) = oneshot::channel();
                Ok(Some((song, Some(self.background(Task::GetKnownStickers("song", uri, s), r).await?))))
            } else {
                Ok(Some((song, None)))
            }
        } else {
            Ok(None)
        }
    }

    pub async fn get_recent_songs(&self, n: u32) -> ClientResult<Vec<Song>> {
        let to_fetch: Vec<(String, OffsetDateTime)> = sqlite::get_last_n_songs(n).expect("Sqlite DB error");
        let mut res: Vec<Song> = Vec::with_capacity(n as usize);
        for tup in to_fetch.into_iter() {
            if let Some(mut song) = self
                .get_song_by_uri(tup.0, false)
                .await
                .map(|opt| opt.map(|pair| pair.0))?
            {
                song.last_played = Some(tup.1);
                res.push(song.into())
            }
        }
        Ok(res)
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
                let batch = uris[inserted..(inserted + to_insert)].iter_mut().map(
                    std::mem::take
                ).collect();
                if let Some(pos) = insert_pos {
                    let (s, r) = oneshot::channel();
                    self.background(Task::InsertMultiple(batch, pos, s), r).await?;
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
            self.foreground(Task::Insert(std::mem::take(&mut uris[0]), pos, s), r).await?;
        } else {
            let (s, r) = oneshot::channel();
            self.foreground(Task::Add(std::mem::take(&mut uris[0]), s), r).await?;
        }

        Ok(())
    }

    async fn get_uris_by_sticker<F>(
        &self,
        obj: StickerObjectType,
        sticker: Cow<'static, str>,
        op: StickerOperation,
        rhs: Cow<'static, str>,
        only_in: Option<String>,
        mut respond: F,
    ) -> ClientResult<()> where F: FnMut(Vec<String>)
    {
        let mut curr_len: usize = 0;
        let mut more: bool = true;
        let only_in = only_in.unwrap_or(String::from(""));
        while more && (curr_len) < FETCH_LIMIT {
            let (s, r) = oneshot::channel();
            let names = self.background(Task::FindStickerOp(
                obj.to_str(),
                only_in.clone(),
                sticker.clone(),
                op.to_mpd_syntax(),
                rhs.clone(),
                Window::from((curr_len as u32, (curr_len + BATCH_SIZE) as u32)),
                s
            ), r).await?;
            if !names.is_empty() {
                // If not searching directly by song (for example by album rating), further resolve to URI.
                match obj {
                    StickerObjectType::Song => {
                        // In this case the names are the URIs themselves
                        respond(names);
                        curr_len += BATCH_SIZE;
                    }
                    StickerObjectType::Playlist => {
                        // Fetch playlist contents. Don't create GObjects yet.
                        for playlist_name in names.into_iter() {
                            self.get_playlist_song_infos(
                                playlist_name,
                                &mut |batch: Vec<SongInfo>| {
                                    respond(
                                        batch.into_iter().map(|song| song.uri).collect(),
                                    );
                                }
                            ).await?;
                        }
                    }
                    tag_type => {
                        let tag_type_str = tag_type.to_str();
                        // Fetch all songs for each tag
                        for tag_value in names.into_iter() {
                            let mut query = Query::new();
                            query.and(Term::Tag(Cow::Borrowed(tag_type_str)), tag_value);
                            self.get_song_infos_by_query(query, &mut |batch: Vec<SongInfo>| {
                                respond(batch.into_iter().map(|song| song.uri).collect())
                            }).await?;
                        }
                    }
                }
                curr_len += BATCH_SIZE;
            } else {
                more = false;
            }
        }
        Ok(())
    }

    async fn resolve_dynamic_playlist_rules(&self, rules: Vec<Rule>) -> ClientResult<Vec<String>> {
        // Resolve into concrete URIs.
        // First, separate the search query-based conditions from the sticker ones.
        let mut query_clauses: Vec<(QueryLhs, String)> = Vec::new();
        let mut sticker_clauses: Vec<(StickerObjectType, String, StickerOperation, String)> =
            Vec::new();
        for rule in rules.into_iter() {
            match rule {
                Rule::Sticker(obj, key, op, rhs) => {
                    sticker_clauses.push((obj, key, op, rhs));
                }
                Rule::Query(lhs, rhs) => {
                    query_clauses.push((lhs, rhs));
                }
                Rule::LastModified(secs) => {
                    // Special case: query current system datetime
                    query_clauses.push((QueryLhs::LastMod, get_past_unix_timestamp(secs).to_string()));
                }
            }
        }
        let mut res: FxHashSet<String> = FxHashSet::default();
        let mut mpd_query = Query::new();
        if !query_clauses.is_empty() {
            for (lhs, rhs) in query_clauses.into_iter() {
                lhs.add_to_query(&mut mpd_query, rhs);
            }
        } else {
            // Dummy term that basically matches everything.
            mpd_query.and(Term::AddedSince, i64::MIN.to_string());
        }
        // Avoid creating GObjects right now as they can still be filtered out
        self.get_song_infos_by_query(mpd_query, &mut |batch: Vec<SongInfo>| {
            for song in batch.into_iter() {
                res.insert(song.uri);
            }
        }).await?;
        println!("Length after query_clauses: {}", res.len());

        // Get matching URIs for each sticker condition
        // TODO: Optimise sticker operations by limiting to any found URI query clause.
        for clause in sticker_clauses.into_iter() {
            let mut set = FxHashSet::default();
            match clause.1.as_str() {
                Stickers::LAST_PLAYED_KEY | Stickers::LAST_SKIPPED_KEY => {
                    // Special case: treat RHS as relative to current time
                    self.get_uris_by_sticker(
                        clause.0,
                        clause.1.into(),
                        clause.2,
                        get_past_unix_timestamp(clause.3.parse::<i64>().unwrap()).to_string().into(),
                        None,
                        |batch| {
                            for uri in batch.into_iter() {
                                set.insert(uri);
                            }
                        }
                    ).await?;
                }
                _ => {
                    self.get_uris_by_sticker(
                        clause.0,
                        clause.1.into(),
                        clause.2,
                        clause.3.into(),
                        None,
                        |batch| {
                            for uri in batch.into_iter() {
                                set.insert(uri);
                            }
                        }
                    ).await?;
                }
            }

            println!("Length of matches of sticker_clause: {}", set.len());
            res.retain(move |elem| set.contains(elem));
            if res.is_empty() {
                // Return early
                return Ok(Vec::with_capacity(0));
            }
            println!("Length afterwards: {}", res.len());
        }

        Ok(res.into_iter().collect())
    }

    pub async fn get_dynamic_playlist_songs(
        &self,
        dp: DynamicPlaylist,
        cache: bool // If true, will cache resolved song URIs locally
    ) -> ClientResult<Vec<Song>> {
        // To reduce server & connection burden, temporarily turn off all tags in responses.
        let (s, r) = oneshot::channel();
        self.foreground(Task::ClearTagTypes(s), r).await?;
        // First, fetch just the URIs, without any sorting
        let uris = self.resolve_dynamic_playlist_rules(dp.rules).await?;

        // Then, fetch the tags and stickers needed for display and sorting.
        // These three are always needed for display.
        let mut tagtypes: Vec<&'static str> = vec!["title", "album", "artist", "albumartist"];
        for ordering in dp.ordering.iter() {
            match ordering {
                Ordering::Track => {
                    tagtypes.push("track");
                }
                Ordering::AscReleaseDate | Ordering::DescReleaseDate => {
                    tagtypes.push("originaldate");
                }
                _ => {
                    // the rest are either Random, always included (LastModified), or stickers-based
                }
            }
        }
        let (s, r) = oneshot::channel();
        self.foreground(Task::EnableTagTypes(Some(tagtypes), s), r).await?;
        let mut songs_stickers = Vec::with_capacity(uris.len());
        for uri in uris.into_iter() {
            if let Some((song, stickers)) = self.get_song_by_uri(uri, true).await? {
                let stickers = stickers.unwrap();
                songs_stickers.push((song, stickers));
            }
        }
        let songs: Vec<SongInfo>;
        if !songs_stickers.is_empty() {
            // Sort the song list now
            if dp.ordering.len() == 1 && dp.ordering[0] == Ordering::Random {
                let mut rng = rand::rng();
                songs_stickers.shuffle(&mut rng);
            } else {
                let cmp_func = build_comparator(&dp.ordering);
                songs_stickers.sort_by(cmp_func);
            }
            if let Some(limit) = dp.limit {
                songs_stickers.truncate(limit as usize);
            }
            songs = songs_stickers.into_iter().map(|tup| tup.0).collect();
            if cache {
                if let Err(db_err) = sqlite::cache_dynamic_playlist_results(&dp.name, &songs) {
                    println!("Failed to cache DP query result. Queuing will be incorrect!");
                    dbg!(db_err);
                }
            }
        } else {
            songs = Vec::with_capacity(0);
        }
        let (s, r) = oneshot::channel();
        self.foreground(Task::EnableTagTypes(None, s), r).await?;
        Ok(songs.into_iter().map(Song::from).collect())
    }

    pub async fn get_dynamic_playlist_songs_cached(&self, name: String) -> ClientResult<Vec<Song>> {
        let uris = gio::spawn_blocking(move || {
            sqlite::get_cached_dynamic_playlist_results(&name).map_err(|_| ClientError::Internal)
        }).await.unwrap().map_err(|_| ClientError::Internal)?;
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
        }).await.unwrap().map_err(|_| ClientError::Internal)?;
        let (s, r) = oneshot::channel();
        self.background(Task::AddMultiple(uris, s), r).await
    }


    // Should handle in Library controller
    // pub async fn get_songs_of_artist<F>(&self, name: String, respond: F) -> ClientResult<()>
    // where F: Fn(Song) {
    //     let mut query = Query::new();
    //     query.and_with_op(
    //         Term::Tag(Cow::Borrowed("artist")),
    //         QueryOperation::Contains,
    //         name,
    //     );
    //     self.get_songs_by_query(Query::new(), respond);
    // }
    //
    // pub fn fetch_albums_of_artist(
    //     client: &mut mpd::Client<stream::StreamWrapper>,
    //     sender_to_fg: &Sender<AsyncClientMessage>,
    //     artist_name: String,
    // ) {
    //     if let Err(mpd_error) = fetch_albums_by_query(
    //         client,
    //         Query::new().and_with_op(
    //             Term::Tag(Cow::Borrowed("artist")),
    //             QueryOperation::Contains,
    //             artist_name.clone(),
    //         ),
    //         |info| {
    //             sender_to_fg.send_blocking(AsyncClientMessage::ArtistAlbumBasicInfoDownloaded(
    //                 artist_name.clone(),
    //                 info,
    //             ))
    //         },
    //     ) {
    //         let _ = sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(mpd_error, None));
    //     }
    // }



    // fn on_songs_downloaded(&self, signal_name: &str, tag: Option<String>, songs: Vec<SongInfo>) {
    //     if let Some(tag) = tag {
    //         self.state.emit_by_name::<()>(
    //             signal_name,
    //             &[
    //                 &tag,
    //                 &BoxedAnyObject::new(songs.into_iter().map(Song::from).collect::<Vec<Song>>()),
    //             ],
    //         );
    //     } else {
    //         self.state.emit_by_name::<()>(
    //             signal_name,
    //             &[&BoxedAnyObject::new(
    //                 songs.into_iter().map(Song::from).collect::<Vec<Song>>(),
    //             )],
    //         );
    //     }
    // }

    // fn on_album_downloaded(&self, signal_name: &str, tag: Option<&str>, info: AlbumInfo) {
    //     let album = Album::from(info);
    //     {
    //         let mut stickers = album.get_stickers().borrow_mut();
    //         if let Some(val) = self.get_sticker("album", album.get_title(), Stickers::RATING_KEY) {
    //             stickers.set_rating(&val);
    //         }
    //     }
    //     // Append to listener lists
    //     if let Some(tag) = tag {
    //         self.state.emit_by_name::<()>(signal_name, &[&tag, &album]);
    //     } else {
    //         self.state.emit_by_name::<()>(signal_name, &[&album]);
    //     }
    // }

    // pub fn get_artist_content(&self, name: String) {
    //     // For artists, we will need to find by substring to include songs and albums that they
    //     // took part in
    //     self.queue_background(BackgroundTask::FetchArtistSongs(name.clone()), true);
    //     self.queue_background(BackgroundTask::FetchArtistAlbums(name.clone()), true);
    // }

    // pub fn on_folder_contents_downloaded(&self, uri: String, contents: Vec<LsInfoEntry>) {
    //     self.state.emit_by_name::<()>(
    //         "folder-contents-downloaded",
    //         &[
    //             &uri.to_value(),
    //             &BoxedAnyObject::new(
    //                 contents
    //                     .into_iter()
    //                     .map(INode::from)
    //                     .collect::<Vec<INode>>(),
    //             )
    //             .to_value(),
    //         ],
    //     );
    // }
}

impl Drop for MpdWrapper {
    fn drop(&mut self) {
        println!("App closed. Closing clients...");

        executor::block_on(
            async move {
                let _ = self.disconnect(true).await;
            }
        );
    }
}
