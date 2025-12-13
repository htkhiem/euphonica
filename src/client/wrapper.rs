use async_channel::{Receiver, Sender};
use futures::executor;
use glib::clone;
use gtk::{gio, glib};
use gtk::{gio::prelude::*, glib::BoxedAnyObject};
use lru::LruCache;
use mpd::Status;
use mpd::error::ServerError;
use mpd::search::Window;
use mpd::{
    Channel, EditAction, Idle, Output, SaveMode, Subsystem, Version,
    client::Client,
    error::{Error as MpdError, ErrorCode as MpdErrorCode},
    lsinfo::LsInfoEntry,
    song::Id,
};
use nohash_hasher::NoHashHasher;
use once_cell::sync::Lazy;
use resolve_path::PathResolveExt;
use zbus::{Connection as ZConnection, Proxy as ZProxy};

use std::borrow::Cow;
use std::hash::BuildHasherDefault;
use std::net::TcpStream;
use std::num::NonZero;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use uuid::Uuid;

use crate::common::Stickers;
use crate::utils::settings_manager;
use crate::{
    common::{Album, AlbumInfo, Artist, INode, Song, SongInfo},
    meta_providers::ProviderMessage,
    player::PlaybackFlow,
    utils,
};

use super::background;
use super::connection::{Connection, Error as ClientError, Result as ClientResult, Task};
use super::password::get_mpd_password;
use super::state::{ClientState, ConnectionState, StickersSupportLevel};
use super::stream::StreamWrapper;
use super::{AsyncClientMessage, BackgroundTask, StickerSetMode};

// Thin wrapper around the blocking mpd::Client. It contains two separate client
// objects connected to the same address. One lives on the main thread along
// with the GUI and takes care of sending user commands to the daemon, while the
// other lives on a child thread. It is often in idle mode in order to
// receive all server-side changes, including those resulting from commands from
// other clients, such as MPRIS controls in the notification centre or another
// frontend. Note that this second client will not notify the main thread on
// seekbar progress. That will have to be polled by the main thread.

// Heavy operations such as streaming lots of album arts from a remote server
// should be performed by the background child client, which will receive them
// through an unbounded async_channel serving as a work queue. On each loop,
// the child client checks whether there's anything to handle in the work queue.
// If there is, it will take & handle one item. If the queue is instead empty, it
// will go into idle() mode.

// Once in the idle mode, the child client is blocked and thus cannot check the
// work queue. As such, after inserting a work item into the queue, the main
// thread will also post a message to an mpd inter-client channel also listened
// to by the child client. This will trigger an idle notification for the Message
// subsystem, allowing the child client to break out of the blocking idle.
//
// The reverse also needs to be taken care of. In case there are too many background
// tasks (such as mass album art downloading upon a cold start), the child client
// might spend too much time completing these tasks without listening to idle updates.
// This is unacceptable as our entire UI relies on idle updates. To solve this, we
// conversely break the child thread out of background tasks when there are foreground
// actions that would cause an idle update using an atomic flag.
// 1. Prior to processing a work item, the child client checks whether this flag is
// true. If it is, it is guaranteed that it could switch back to idle mode for a
// quick update and won't be stuck there for too long.
// 2. The child thread then switches to idle mode, which should return immediately
// as there should be at least one idle message in the queue. The child client
// forwards all queued-up messages to the main thread, sets the atomic flag to false,
// then ends the iteration.
// 3. When there is nothing left in the work queue, simply enter idle mode without
// checking thelag.

// The child thread never modifies the main state directly. It instead sends
// messagecontaining a list of subsystems with updated states to the main thread
// via a bounded async_channel. The main thread receives these messages in an async
// loop, contacts the daemon again to get information for each of the changed
// suystems, then update the relevant state objects accordingly, saving us the
// trouble of putting state objects behind mutexes.

// Reconnection is a bit convoluted. There is no way to abort the child thread
// from the main one, but we can make the child thread check a flag before idling.
// The child thread will only be able to do so after finishing idling, but
// incidentally, disconnecting the main thread's client will send an idle message,
// unblocking the child thread and allowing it to check the flag.


const BATCH_SIZE: usize = 128;
const FETCH_LIMIT: usize = 10000000; // Fetch at most ten million songs at once (same
// folder, same tag, etc)


#[derive(Debug)]
pub struct MpdWrapper {
    // Handles return bool to indicate whether the threads stopped due to an error
    // (true) or disconnection request (false).
    fg_handle: thread::JoinHandle<bool>,
    bg_handle: thread::JoinHandle<bool>,
    state: ClientState,
    fg_sender: Sender<Task>, // For sending tasks to the interactive client
    bg_sender: Sender<Task>, // For sending tasks to the background client
    idle_receiver: async_channel::Receiver<Subsystem>,
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
        let wrapper = Rc::new(Self {
            fg_handle: thread::spawn(|| {
                Connection::new(fg_receiver, wake_channel, None)
                    .run()
                    .is_err()
            }),
            bg_handle: thread::spawn(|| {
                Connection::new(bg_receiver, wake_channel_bg, Some(idle_sender))
                    .run()
                    .is_err()
            }),
            state: ClientState::default(),
            fg_sender,
            bg_sender,
            idle_receiver,
            client_version: RefCell::new(None),
            // Cache song infos so we can reuse them on queue updates.
            // Song IDs are u32s anyway, and I don't think there's any risk of a HashDoS attack
            // from a self-hosted music server so we'll just use identity hash for speed.
            song_cache: LruCache::with_hasher(
                NonZero::new(16384).unwrap(),
                BuildHasherDefault::default(),
            )
        });

        wrapper
    }

    pub fn get_client_state(&self) -> ClientState {
        self.state.clone()
    }

    fn start_bg_thread(&self, password: Option<String>) {
        let sender_to_fg = self.main_sender.clone();
        let pending_idle = self.pending_idle.clone();
        // We have two queues here:
        // A "normal" queue for tasks that don't require immediacy, like batch album art downloading
        // on cold startups.
        let (bg_sender, bg_receiver) = async_channel::unbounded::<BackgroundTask>();
        // A "high-priority" queue for tasks queued as a direct result of a user action, such as fetching
        // album content.
        let (bg_sender_high, bg_receiver_high) = async_channel::unbounded::<BackgroundTask>();
        // The high-priority queue will always be exhausted first before the normal queue is processed.
        // Since right now we only have two priority levels, having two queues is much simpler and faster
        // than an actual heap/hash-based priority queue.
        let meta_sender = self.meta_sender.clone();
        self.bg_sender.replace(Some(bg_sender));
        self.bg_sender_high.replace(Some(bg_sender_high));
        let bg_channel = self.wake_channel.clone();

        let bg_handle = gio::spawn_blocking(move || {
            // Create a new connection for the child thread
            let conn = utils::settings_manager().child("client");

            let mut client: Client<StreamWrapper>;

            let error_msg = "Unable to start background client using current connection settings";
            if conn.boolean("mpd-use-unix-socket") {
                let stream: StreamWrapper;
                let path = conn.string("mpd-unix-socket");
                if let Ok(resolved_path) = path.try_resolve() {
                    stream = StreamWrapper::new_unix(
                        UnixStream::connect(resolved_path)
                            .map_err(mpd::error::Error::Io)
                            .expect(error_msg),
                    );
                } else {
                    stream = StreamWrapper::new_unix(
                        UnixStream::connect(path.as_str())
                            .map_err(mpd::error::Error::Io)
                            .expect(error_msg),
                    );
                }
                if let Ok(new_client) = mpd::Client::new(stream) {
                    client = new_client;
                } else {
                    // For early errors like this it's best to just disconnect.
                    let _ = sender_to_fg.send_blocking(AsyncClientMessage::Disconnect);
                    return;
                }
            } else {
                let addr = format!("{}:{}", conn.string("mpd-host"), conn.uint("mpd-port"));
                println!("Connecting to TCP socket {}", &addr);
                let stream = StreamWrapper::new_tcp(
                    TcpStream::connect(addr)
                        .map_err(mpd::error::Error::Io)
                        .expect(error_msg),
                );
                client = mpd::Client::new(stream).expect(error_msg);
            }
            if let Some(password) = password {
                if client.login(&password).is_err() {
                    // For early errors like this it's best to just disconnect.
                    println!(
                        "Background client failed to authenticate in the same manner as main client"
                    );
                    let _ = sender_to_fg.send_blocking(AsyncClientMessage::Disconnect);
                    return;
                }
            }
            if let Err(MpdError::Io(_)) = client.subscribe(bg_channel) {
                // For early errors like this it's best to just disconnect.
                println!("Background client could not subscribe to inter-client channel");
                let _ = sender_to_fg.send_blocking(AsyncClientMessage::Disconnect);
                return;
            }

            'outer: loop {
                let skip_to_idle = pending_idle.load(Ordering::Relaxed);

                let mut curr_task: Option<BackgroundTask> = None;
                let n_tasks = bg_receiver_high.len() + bg_receiver.len();
                let _ = sender_to_fg.send_blocking(AsyncClientMessage::Status(n_tasks));
                if !skip_to_idle {
                    if !bg_receiver_high.is_empty() {
                        curr_task = Some(
                            bg_receiver_high
                                .recv_blocking()
                                .expect("Unable to read from high-priority queue"),
                        );
                    } else if !bg_receiver.is_empty() {
                        curr_task = Some(
                            bg_receiver
                                .recv_blocking()
                                .expect("Unable to read from background queue"),
                        );
                    }
                }

                if !skip_to_idle && curr_task.is_some() {
                    let task = curr_task.unwrap();
                    match task {
                        BackgroundTask::Update => {
                            background::update_mpd_database(&mut client, &sender_to_fg)
                        }
                        BackgroundTask::FetchQueue => {
                            background::get_current_queue(&mut client, &sender_to_fg);
                        }
                        BackgroundTask::FetchQueueChanges(version, total_len) => {
                            background::get_queue_changes(
                                &mut client,
                                &sender_to_fg,
                                version,
                                total_len,
                            );
                        }
                        BackgroundTask::DownloadFolderCover(key) => {
                            background::download_folder_cover(&mut client, &meta_sender, key)
                        }
                        BackgroundTask::DownloadEmbeddedCover(key) => {
                            background::download_embedded_cover(&mut client, &meta_sender, key)
                        }
                        BackgroundTask::FetchAlbums => {
                            background::fetch_all_albums(&mut client, &sender_to_fg)
                        }
                        BackgroundTask::FetchRecentAlbums => {
                            background::fetch_recent_albums(&mut client, &sender_to_fg)
                        }
                        BackgroundTask::FetchAlbumSongs(tag) => {
                            background::fetch_album_songs(&mut client, &sender_to_fg, tag)
                        }
                        BackgroundTask::FetchArtists(use_albumartist) => {
                            background::fetch_artists(&mut client, &sender_to_fg, use_albumartist)
                        }
                        BackgroundTask::FetchRecentArtists => {
                            background::fetch_recent_artists(&mut client, &sender_to_fg)
                        }
                        BackgroundTask::FetchArtistSongs(name) => {
                            background::fetch_songs_of_artist(&mut client, &sender_to_fg, name)
                        }
                        BackgroundTask::FetchArtistAlbums(name) => {
                            background::fetch_albums_of_artist(&mut client, &sender_to_fg, name)
                        }
                        BackgroundTask::FetchFolderContents(uri) => {
                            background::fetch_folder_contents(&mut client, &sender_to_fg, uri)
                        }
                        BackgroundTask::FetchPlaylistSongs(name) => {
                            background::fetch_playlist_songs(&mut client, &sender_to_fg, name)
                        }
                        BackgroundTask::FetchRecentSongs(count) => {
                            background::fetch_last_n_songs(&mut client, &sender_to_fg, count);
                        }
                        BackgroundTask::QueueUris(uris, recursive, play_from, insert_pos) => {
                            background::add_multi(
                                &mut client,
                                &sender_to_fg,
                                &uris,
                                recursive,
                                play_from,
                                insert_pos,
                            );
                        }
                        BackgroundTask::QueueQuery(query, play_from) => {
                            background::find_add(&mut client, &sender_to_fg, query, play_from);
                        }
                        BackgroundTask::QueuePlaylist(name, play_from) => {
                            background::load_playlist(&mut client, &sender_to_fg, &name, play_from);
                        }
                        BackgroundTask::FetchDynamicPlaylistSongs(dp, cache) => {
                            background::fetch_dynamic_playlist(
                                &mut client,
                                &sender_to_fg,
                                dp,
                                cache,
                            );
                        }
                        BackgroundTask::FetchCachedDynamicPlaylistSongs(name) => {
                            background::fetch_dynamic_playlist_cached(
                                &mut client,
                                &sender_to_fg,
                                &name,
                            );
                        }
                        BackgroundTask::QueueDynamicPlaylist(name, play) => {
                            background::queue_cached_dynamic_playlist(
                                &mut client,
                                &sender_to_fg,
                                &name,
                                play,
                            );
                        }
                    }
                } else {
                    // If not, go into idle mode
                    if skip_to_idle {
                        // println!("Background MPD thread skipping to idle mode as there are pending messages");
                        pending_idle.store(false, Ordering::Relaxed);
                    }
                    if let Ok(changes) = client.wait(&[]) {
                        if changes.contains(&Subsystem::Message) {
                            if let Ok(msgs) = client.readmessages() {
                                for msg in msgs {
                                    let content = msg.message.as_str();
                                    match content {
                                        // More to come
                                        "STOP" => {
                                            let _ = client.close();
                                            break 'outer;
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                        let _ = sender_to_fg.send_blocking(AsyncClientMessage::Idle(changes));
                    } else {
                        let _ = client.close();
                        println!(
                            "Child thread encountered a client error while idling. Stopping..."
                        );
                        let _ = sender_to_fg.send_blocking(AsyncClientMessage::Disconnect);
                        break 'outer;
                    }
                }
            }
        });
        self.bg_handle.replace(Some(bg_handle));
    }

    fn setup_channel(self: Rc<Self>, receiver: Receiver<AsyncClientMessage>) {
        // Set up a listener to the receiver we got from Application.
        // This will be the loop that handles user interaction and idle updates.
        glib::MainContext::default().spawn_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                use futures::prelude::*;
                // Allow receiver to be mutated, but keep it at the same memory address.
                // See Receiver::next doc for why this is needed.
                let mut receiver = std::pin::pin!(receiver);
                let mut recently_connected: bool = false;
                while let Some(request) = receiver.next().await {
                    let old_recently_connected = recently_connected;
                    recently_connected = matches!(request, AsyncClientMessage::Connect);
                    this.respond(request, old_recently_connected).await;
                    // Prevent rapid-fire reconnections
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
                    if let Some(client) = this.main_client.borrow_mut().as_mut() {
                        let res = client.ping();
                        if res.is_err() {
                            println!("[KeepAlive] [FATAL] Could not ping mpd. The connection might have already timed out, or the daemon might have crashed.");
                            break;
                        }
                    }
                    else {
                        println!("[KeepAlive] There is no client currently running. Won't ping.");
                    }
                    glib::timeout_future_seconds(ping_interval).await;
                }
            }));

        // A new loop to watch for system suspend/wake actions. We proactively disconnect before suspend
        // and connect again upon wake to avoid freezing upon a timed-out connection.
        // let (suspend_sender, suspend_receiver): (Sender<bool>, Receiver<bool>) = async_channel::unbounded();
        // TODO: Avoid sending more updates if the current one hasn't completed yet.
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
                        fg_sender.send(Task::Disconnect(false, s)).await;
                        r.await;
                        let (s, r) = oneshot::channel();
                        bg_sender.send(Task::Disconnect(false, s)).await;
                        r.await;
                    } else {
                        println!("System has woken up. Reconnecting...");
                        let mut client_password: Option<String> = None;
                        match get_mpd_password().await {
                            Ok(maybe_password) => {
                                client_password = maybe_password;
                            }
                            _ => {}
                        }
                        let (s, r) = oneshot::channel();
                        fg_sender.send(Task::Connect(client_password, s)).await;
                        r.await;
                        let (s, r) = oneshot::channel();
                        bg_sender.send(Task::Connect(client_password, s)).await;
                        r.await;
                    }
                }
            }
        });
    }

    async fn respond(
        &self,
        request: AsyncClientMessage,
        recently_connected: bool,
    ) -> glib::ControlFlow {
        // println!("Received MpdMessage {:?}", request);
        match request {
            AsyncClientMessage::Connect => {
                if !recently_connected {
                    self.connect_async().await;
                }
            }
            AsyncClientMessage::Disconnect => self.disconnect_async().await,
            AsyncClientMessage::Idle(changes) => self.handle_idle_changes(changes).await,
            AsyncClientMessage::QueueSongsDownloaded(songs) => {
                self.on_songs_downloaded("queue-songs-downloaded", None, songs)
            }
            AsyncClientMessage::QueueChangesReceived(changes) => {
                self.state.emit_boxed_result("queue-changed", changes);
            }
            AsyncClientMessage::AlbumBasicInfoDownloaded(info) => {
                self.on_album_downloaded("album-basic-info-downloaded", None, info)
            }
            AsyncClientMessage::RecentAlbumDownloaded(info) => {
                self.on_album_downloaded("recent-album-downloaded", None, info)
            }
            AsyncClientMessage::AlbumSongInfoDownloaded(tag, songs) => {
                self.on_songs_downloaded("album-songs-downloaded", Some(tag), songs)
            }
            AsyncClientMessage::ArtistBasicInfoDownloaded(info) => self
                .state
                .emit_result("artist-basic-info-downloaded", Artist::from(info)),
            AsyncClientMessage::RecentArtistDownloaded(info) => self
                .state
                .emit_result("recent-artist-downloaded", Artist::from(info)),
            AsyncClientMessage::ArtistSongInfoDownloaded(name, songs) => {
                self.on_songs_downloaded("artist-songs-downloaded", Some(name), songs)
            }
            AsyncClientMessage::ArtistAlbumBasicInfoDownloaded(artist_name, song_info) => self
                .on_album_downloaded(
                    "artist-album-basic-info-downloaded",
                    Some(&artist_name),
                    song_info,
                ),
            AsyncClientMessage::FolderContentsDownloaded(uri, contents) => {
                self.on_folder_contents_downloaded(uri, contents)
            }
            AsyncClientMessage::PlaylistSongInfoDownloaded(name, songs) => {
                self.on_songs_downloaded("playlist-songs-downloaded", Some(name), songs)
            }
            AsyncClientMessage::DBUpdated => {}
            AsyncClientMessage::Status(n_tasks) => {
                self.state.set_n_background_tasks(n_tasks as u64)
            }
            AsyncClientMessage::RecentSongInfoDownloaded(songs) => {
                self.on_songs_downloaded("recent-songs-downloaded", None, songs)
            }
            AsyncClientMessage::Queuing(block) => {
                self.state.set_queuing(block);
            }
            AsyncClientMessage::BackgroundError(error, or) => {
                self.handle_common_mpd_error(&error, or);
            }
            AsyncClientMessage::DynamicPlaylistSongInfoDownloaded(name, songs) => {
                self.on_songs_downloaded("dynamic-playlist-songs-downloaded", Some(name), songs)
            }
        }
        glib::ControlFlow::Continue
    }

    async fn handle_idle_changes(&self, changes: Vec<Subsystem>) {
        for subsystem in changes {
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
    }

    pub async fn disconnect(&self, stop: bool) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.fg_sender.send(Task::Disconnect(stop, s)).await;
        r.await.expect("Broken oneshot receiver")?;
        println!("Stopped foreground client");
        let (s, r) = oneshot::channel();
        self.bg_sender.send(Task::Disconnect(stop, s)).await;
        r.await.expect("Broken oneshot receiver")?;
        println!("Stopped background client");
        self.state
            .set_connection_state(ConnectionState::NotConnected);
        self.state.set_queuing(false);
        self.queue_version.set(0);
        self.expected_queue_version.set(0);
        self.client_version.take();
        Ok(())
    }

    async fn handle_error<T>(&self, res: ClientResult<T>) -> ClientResult<T> {
        match &res {
            Err(e) => {
                match e {
                    ClientError::Internal => {
                        // TODO
                    }
                    ClientError::Mpd(e) => {
                        match e {
                            MpdError::Io(e) => {
                                self.state
                                    .set_connection_state(ConnectionState::NotConnected);
                                // TODO
                            }
                            MpdError::Parse(e) => {}
                            MpdError::Proto(e) => {}
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
                        let settings = settings_manager().child("client");
                        if settings.boolean("mpd-auto-reconnect") {
                            self.connect().await;
                        }
                    }
                }
            }
            _ => {}
        }

        res
    }

    async fn handle_connect_error(
        &self,
        res: ClientResult<Version>,
        access_error: bool,
        password_unset: bool,
    ) -> ClientResult<Version> {
        match &res {
            Err(e) => match e {
                ClientError::Mpd(MpdError::Server(e)) => match e.code {
                    MpdErrorCode::Password => {
                        self.state
                            .set_connection_state(ConnectionState::WrongPassword);
                    }
                    MpdErrorCode::Permission => {
                        self.state.set_connection_state(if access_error {
                            ConnectionState::CredentialStoreError
                        } else if password_unset {
                            ConnectionState::PasswordNotAvailable
                        } else {
                            ConnectionState::Unauthenticated
                        });
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
        // Close current clients
        self.disconnect(false).await;

        self.state.set_connection_state(ConnectionState::Connecting);

        let mut password_access_failed = false;
        let mut password_unset = false;
        let client_password: Option<String>;
        match get_mpd_password().await {
            Ok(maybe_password) => {
                client_password = maybe_password;
                password_unset = client_password.is_none();
            }
            Err(e) => {
                // Only reachable by glib async runner errors
                client_password = None;
                println!("{:?}", &e);
                password_access_failed = true;
            }
        }

        let (s, r) = oneshot::channel();
        self.fg_sender
            .send(Task::Connect(client_password.clone(), s))
            .await;
        let version = self
            .handle_connect_error(
                r.await.expect("Broken oneshot receiver"),
                password_access_failed,
                password_unset,
            )
            .await?;
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
        println!("Connected foreground client");

        let (s, r) = oneshot::channel();
        self.bg_sender.send(Task::Connect(client_password, s)).await;
        self.handle_connect_error(
            r.await.expect("Broken oneshot receiver"),
            password_access_failed,
            password_unset,
        )
        .await?;
        println!("Connected background client");

        Ok(())
    }

    async fn foreground<T>(
        &self,
        task: Task,
        receiver: oneshot::Receiver<ClientResult<T>>,
    ) -> ClientResult<T> {
        self.fg_sender.send(task).await;
        self.handle_error(receiver.await.expect("Broken oneshot receiver"))
            .await
    }

    async fn background<T>(
        &self,
        task: Task,
        receiver: oneshot::Receiver<ClientResult<T>>,
    ) -> ClientResult<T> {
        self.bg_sender.send(task).await;
        // Wake background thread
        let (s, r) = oneshot::channel();
        self.foreground(Task::SendMessage(String::from("wake"), s), r)
            .await?;
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

    pub async fn get_sticker(&self, typ: &str, uri: &str, name: &str) -> ClientResult<String> {
        let min_lvl = if typ == "song" {
            StickersSupportLevel::SongsOnly
        } else {
            StickersSupportLevel::All
        };
        if self.state.get_stickers_support_level() >= min_lvl {
            let (s, r) = oneshot::channel();
            self.handle_sticker_error(
                self.foreground(
                    Task::GetSticker(typ.to_owned(), uri.to_owned(), name.to_owned(), s),
                    r,
                )
                .await,
            )
        } else {
            Err(ClientError::InsufficientStickersSupportLevel)
        }
    }

    pub async fn get_known_stickers(&self, typ: &str, uri: &str) -> ClientResult<Stickers> {
        let min_lvl = if typ == "song" {
            StickersSupportLevel::SongsOnly
        } else {
            StickersSupportLevel::All
        };
        if self.state.get_stickers_support_level() >= min_lvl {
            let (s, r) = oneshot::channel();
            self.handle_sticker_error(
                self.foreground(Task::GetKnownStickers(typ.to_owned(), uri.to_owned(), s), r)
                    .await,
            )
        } else {
            Err(ClientError::InsufficientStickersSupportLevel)
        }
    }

    pub async fn set_sticker(
        &self,
        typ: &str,
        uri: &str,
        name: &str,
        value: &str,
        mode: StickerSetMode,
    ) -> ClientResult<()> {
        let min_lvl = if typ == "song" {
            StickersSupportLevel::SongsOnly
        } else {
            StickersSupportLevel::All
        };
        if self.state.get_stickers_support_level() >= min_lvl {
            let (s, r) = oneshot::channel();
            self.handle_sticker_error(
                self.foreground(
                    Task::SetSticker(
                        typ.to_owned(),
                        uri.to_owned(),
                        name.to_owned(),
                        value.to_owned(),
                        mode,
                        s,
                    ),
                    r,
                )
                .await,
            )
        } else {
            Err(ClientError::InsufficientStickersSupportLevel)
        }
    }

    pub async fn delete_sticker(&self, typ: &str, uri: &str, name: &str) -> ClientResult<()> {
        let min_lvl = if typ == "song" {
            StickersSupportLevel::SongsOnly
        } else {
            StickersSupportLevel::All
        };
        if self.state.get_stickers_support_level() >= min_lvl {
            let (s, r) = oneshot::channel();
            self.handle_sticker_error(
                self.foreground(
                    Task::DeleteSticker(typ.to_owned(), uri.to_owned(), name.to_owned(), s),
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

    pub async fn load_playlist(&self, name: &str) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.handle_playlist_error(
            self.foreground(Task::LoadPlaylist(name.to_owned(), s), r)
                .await,
        )
    }

    pub async fn save_queue_as_playlist(
        &self,
        name: &str,
        save_mode: SaveMode,
    ) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.handle_playlist_error(
            self.foreground(Task::SaveQueueAsPlaylist(name.to_owned(), save_mode, s), r)
                .await,
        )
    }

    pub async fn rename_playlist(&self, old_name: &str, new_name: &str) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.handle_playlist_error(
            self.foreground(
                Task::RenamePlaylist(old_name.to_owned(), new_name.to_owned(), s),
                r,
            )
            .await,
        )
    }

    pub async fn edit_playlist(&self, actions: Vec<EditAction<'static>>) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.handle_playlist_error(self.foreground(Task::EditPlaylist(actions, s), r).await)
    }

    pub async fn delete_playlist(&self, name: &str) -> ClientResult<()> {
        let (s, r) = oneshot::channel();
        self.handle_playlist_error(
            self.foreground(Task::DeletePlaylist(name.to_owned(), s), r)
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
    where F: Fn(Vec<Song>) -> () {
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
                        cached_song = self.song_cache.borrow_mut().get(&change.id.0).map(|s| s.clone());
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
                    Task::GetKnownStickers("song".to_owned(), res.get_uri().to_owned(), s), r
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

    fn on_songs_downloaded(&self, signal_name: &str, tag: Option<String>, songs: Vec<SongInfo>) {
        if let Some(tag) = tag {
            self.state.emit_by_name::<()>(
                signal_name,
                &[
                    &tag,
                    &BoxedAnyObject::new(songs.into_iter().map(Song::from).collect::<Vec<Song>>()),
                ],
            );
        } else {
            self.state.emit_by_name::<()>(
                signal_name,
                &[&BoxedAnyObject::new(
                    songs.into_iter().map(Song::from).collect::<Vec<Song>>(),
                )],
            );
        }
    }

    fn on_album_downloaded(&self, signal_name: &str, tag: Option<&str>, info: AlbumInfo) {
        let album = Album::from(info);
        {
            let mut stickers = album.get_stickers().borrow_mut();
            if let Some(val) = self.get_sticker("album", album.get_title(), Stickers::RATING_KEY) {
                stickers.set_rating(&val);
            }
        }
        // Append to listener lists
        if let Some(tag) = tag {
            self.state.emit_by_name::<()>(signal_name, &[&tag, &album]);
        } else {
            self.state.emit_by_name::<()>(signal_name, &[&album]);
        }
    }

    pub fn get_artist_content(&self, name: String) {
        // For artists, we will need to find by substring to include songs and albums that they
        // took part in
        self.queue_background(BackgroundTask::FetchArtistSongs(name.clone()), true);
        self.queue_background(BackgroundTask::FetchArtistAlbums(name.clone()), true);
    }

    pub fn on_folder_contents_downloaded(&self, uri: String, contents: Vec<LsInfoEntry>) {
        self.state.emit_by_name::<()>(
            "folder-contents-downloaded",
            &[
                &uri.to_value(),
                &BoxedAnyObject::new(
                    contents
                        .into_iter()
                        .map(INode::from)
                        .collect::<Vec<INode>>(),
                )
                .to_value(),
            ],
        );
    }
}

// impl Drop for MpdWrapper {
//     fn drop(&mut self) {
//         println!("App closed. Closing clients...");

//         executor::block_on(clone!(
//             #[strong(rename_to = this)]
//             self,
//             async move {
//                 this.disconnect(true).await;
//             }
//         ));
//         if let Some(mut main_client) = self.main_client.borrow_mut().take() {

//             // First, send stop message
//             let _ = main_client.sendmessage(self.wake_channel.clone(), "STOP");
//             // Now close the main client, which will trigger an idle message.
//             let _ = main_client.close();
//             // Now the child thread really should have read the stop_flag.
//             // Wait for it to stop.
//             if let Some(handle) = self.bg_handle.take() {
//                 let _ = executor::block_on(handle);
//             }
//         }
//     }
// }
