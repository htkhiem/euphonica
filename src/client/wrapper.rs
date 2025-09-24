use async_channel::{Receiver, Sender};
use futures::executor;
use glib::clone;
use gtk::{gio, glib};
use gtk::{gio::prelude::*, glib::BoxedAnyObject};
use resolve_path::PathResolveExt;
use mpd::error::ServerError;
use mpd::{
    client::Client,
    error::{Error as MpdError, ErrorCode as MpdErrorCode},
    lsinfo::LsInfoEntry,
    song::Id,
    Channel, EditAction, Idle, Output, SaveMode, Subsystem,
};

use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::{
    cell::{Cell, RefCell},
    rc::Rc
};
use uuid::Uuid;

use crate::common::Stickers;
use crate::{
    common::{Album, AlbumInfo, Artist, INode, Song, SongInfo},
    meta_providers::ProviderMessage,
    player::PlaybackFlow,
    utils,
};

use super::{AsyncClientMessage, BackgroundTask};
use super::state::{ClientState, ConnectionState, StickersSupportLevel};
use super::stream::StreamWrapper;
use super::password::get_mpd_password;
use super::background;
use super::ClientError;

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
// checking the flag.

// The child thread never modifies the main state directly. It instead sends
// messages containing a list of subsystems with updated states to the main thread
// via a bounded async_channel. The main thread receives these messages in an async
// loop, contacts the daemon again to get information for each of the changed
// subsystems, then update the relevant state objects accordingly, saving us the
// trouble of putting state objects behind mutexes.

// Reconnection is a bit convoluted. There is no way to abort the child thread
// from the main one, but we can make the child thread check a flag before idling.
// The child thread will only be able to do so after finishing idling, but
// incidentally, disconnecting the main thread's client will send an idle message,
// unblocking the child thread and allowing it to check the flag.

#[derive(Debug)]
pub struct MpdWrapper {
    // Corresponding sender, for cloning into child thread.
    main_sender: Sender<AsyncClientMessage>,
    // The main client living on the main thread. Every single method of
    // mpd::Client is mutating so we'll just rely on a RefCell for now.
    main_client: RefCell<Option<Client<StreamWrapper>>>, 
    // The state GObject, used for communicating client status & changes to UI elements
    state: ClientState,
    // Handle to the child thread.
    bg_handle: RefCell<Option<gio::JoinHandle<()>>>,
    bg_channel: Channel, // For waking up the child client
    bg_sender: RefCell<Option<Sender<BackgroundTask>>>, // For sending tasks to background thread
    bg_sender_high: RefCell<Option<Sender<BackgroundTask>>>, // For sending high-priority tasks to background thread
    meta_sender: Sender<ProviderMessage>, // For sending album arts to cache controller
    pending_idle: Arc<AtomicBool>,

    // To improve efficiency & avoid UI scroll resetting problems we'll
    // cheat by applying queue edits locally first, then send the commands
    // afterwards. This requires us to carefully skip the next updates
    // from the idle client by tracking the expected queue version after
    // performing the updates.
    // Local changes increment the expected queue version by the expected number
    // of version changes (depending on their logic) BEFORE actually sending
    // the commands to MPD.
    // On every update_status() call, if the newest version gets ahead of
    // expected_queue version, we are out of sync and must perform a refresh
    // using the old logic. Else do nothing.
    queue_version: Cell<u32>,
    expected_queue_version: Cell<u32>
}

impl MpdWrapper {
    pub fn new(meta_sender: Sender<ProviderMessage>) -> Rc<Self> {
        // Set up channels for communication with client object
        let (sender, receiver): (Sender<AsyncClientMessage>, Receiver<AsyncClientMessage>) =
            async_channel::unbounded();
        let ch_name = Uuid::new_v4().simple().to_string();
        println!("Channel name: {}", &ch_name);
        let wrapper = Rc::new(Self {
            main_sender: sender,
            state: ClientState::default(),
            main_client: RefCell::new(None), // Must be initialised later
            bg_handle: RefCell::new(None),   // Will be spawned later
            bg_channel: Channel::new(&ch_name).unwrap(),
            bg_sender: RefCell::new(None),
            bg_sender_high: RefCell::new(None),
            pending_idle: Arc::new(AtomicBool::new(false)),
            meta_sender,
            queue_version: Cell::new(0),
            expected_queue_version: Cell::new(0)
        });

        // For future noob self: these are shallow
        wrapper.clone().setup_channel(receiver);
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
        let bg_channel = self.bg_channel.clone();

        let bg_handle = gio::spawn_blocking(move || {
            // Create a new connection for the child thread
            let conn = utils::settings_manager().child("client");

            let mut client: Client<StreamWrapper>;

            let error_msg = "Unable to start background client using current connection settings";
            if conn.boolean("mpd-use-unix-socket") {
                let stream: StreamWrapper;
                let path = conn.string("mpd-unix-socket");
                if let Ok(resolved_path) = path.try_resolve() {
                    stream = StreamWrapper::new_unix(UnixStream::connect(resolved_path).map_err(mpd::error::Error::Io).expect(error_msg));
                }
                else {
                    stream = StreamWrapper::new_unix(UnixStream::connect(path.as_str()).map_err(mpd::error::Error::Io).expect(error_msg));
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
                let stream = StreamWrapper::new_tcp(TcpStream::connect(addr).map_err(mpd::error::Error::Io).expect(error_msg));
                client = mpd::Client::new(stream).expect(error_msg);
            }
            if let Some(password) = password {
                if client.login(&password).is_err() {
                    // For early errors like this it's best to just disconnect.
                    println!("Background client failed to authenticate in the same manner as main client");
                    let _ = sender_to_fg.send_blocking(AsyncClientMessage::Disconnect);
                    return;
                }
            }
            if let Err(MpdError::Io(_)) = client
                .subscribe(bg_channel) {
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
                            background::get_queue_changes(&mut client, &sender_to_fg, version, total_len);
                        }
                        BackgroundTask::DownloadFolderCover(key) => {
                            background::download_folder_cover(
                                &mut client,
                                &meta_sender,
                                key
                            )
                        }
                        BackgroundTask::DownloadEmbeddedCover(key) => {
                            background::download_embedded_cover(
                                &mut client,
                                &meta_sender,
                                key
                            )
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
                            background::fetch_artists(
                                &mut client,
                                &sender_to_fg,
                                use_albumartist,
                            )
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
                            background::add_multi(&mut client, &sender_to_fg, &uris, recursive, play_from, insert_pos);
                        }
                        BackgroundTask::QueueQuery(query, play_from) => {
                            background::find_add(&mut client, &sender_to_fg, query, play_from);
                        }
                        BackgroundTask::QueuePlaylist(name, play_from) => {
                            background::load_playlist(&mut client, &sender_to_fg, &name, play_from);
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
    }

    async fn respond(&self, request: AsyncClientMessage, recently_connected: bool) -> glib::ControlFlow {
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
            AsyncClientMessage::Status(n_tasks) => self.state.set_n_background_tasks(n_tasks as u64),
            AsyncClientMessage::RecentSongInfoDownloaded(songs) => self
                .on_songs_downloaded("recent-songs-downloaded", None, songs),
            AsyncClientMessage::Queuing(block) => {
                self.state.set_queuing(block);
            }
            AsyncClientMessage::BackgroundError(error, or) => {
                self.handle_common_mpd_error(&error, or);
            }
        }
        glib::ControlFlow::Continue
    }

    async fn handle_idle_changes(&self, changes: Vec<Subsystem>) {
        for subsystem in changes {
            self.state.emit_boxed_result("idle", subsystem);            // Handle some directly here
            match subsystem {
                Subsystem::Database => {
                    // Database changed after updating. Perform a reconnection,
                    // which will also trigger views to refresh their contents.
                    let _ = self.main_sender.send_blocking(AsyncClientMessage::Connect);
                }
                // More to come
                _ => {}
            }
        }
    }

    pub fn queue_background(&self, task: BackgroundTask, high_priority: bool) {
        let maybe_sender = if high_priority {
            self.bg_sender_high.borrow()
        } else {
            self.bg_sender.borrow()
        };
        if let Some(sender) = maybe_sender.as_ref() {
            if sender
                .send_blocking(task)
                .is_err() {
                    // These errors can only happen after we've successfully connected both clients, so
                    // we should attempt a reconnection.
                    println!("[Warning] Lost child client. Reconnecting...");
                    let _ = self.main_sender.send_blocking(AsyncClientMessage::Connect);
                }
            if let Some(client) = self.main_client.borrow_mut().as_mut() {
                // Wake background thread
                let _ = client.sendmessage(self.bg_channel.clone(), "WAKE");
            } else {
                println!("Warning: cannot wake child thread. Task might be delayed.");
            }
        } else {
            // This is nasty (something happened way out of order). Don't attempt to recover.
            panic!("Cannot queue background task (background sender not initialised)");
        }
    }

    pub fn queue_connect(&self) {
        self.main_sender
            .send_blocking(AsyncClientMessage::Connect)
            .expect("Cannot call reconnection asynchronously");
    }

    async fn disconnect_async(&self) {
        if let Some(mut main_client) = self.main_client.borrow_mut().take() {
            println!("Closing existing clients");
            // Stop child thread by sending a "STOP" message through mpd itself
            let _ = main_client.sendmessage(self.bg_channel.clone(), "STOP");
            // Now close the main client
            let _ = main_client.close();
        }
        // Wait for child client to stop.
        if let Some(handle) = self.bg_handle.take() {
            let _ = handle.await;
            println!("Stopped all clients successfully.");
        }
        self.state
            .set_connection_state(ConnectionState::NotConnected);
    }

    pub async fn connect_async(&self) {
        // Close current clients
        self.disconnect_async().await;
        self.state.set_queuing(false);
        self.queue_version.set(0);
        self.expected_queue_version.set(0);

        let conn = utils::settings_manager().child("client");

        self.state.set_connection_state(ConnectionState::Connecting);
        let handle: gio::JoinHandle<Result<mpd::Client<StreamWrapper>, MpdError>>;
        let use_unix_socket = conn.boolean("mpd-use-unix-socket");
        if use_unix_socket {
            let path = conn.string("mpd-unix-socket");
            println!("Connecting to local socket {}", &path);
            if let Ok(resolved_path) = path.as_str().try_resolve() {
                let resolved_path = resolved_path.into_owned();
                handle = gio::spawn_blocking(move || {
                    let stream = StreamWrapper::new_unix(UnixStream::connect(&resolved_path).map_err(mpd::error::Error::Io)?);
                    mpd::Client::new(stream)
                });
            } else {
                handle = gio::spawn_blocking(move || {
                    let stream = StreamWrapper::new_unix(UnixStream::connect(&path.as_str()).map_err(mpd::error::Error::Io)?);
                    mpd::Client::new(stream)
                });
            }
        } else {
            let addr = format!("{}:{}", conn.string("mpd-host"), conn.uint("mpd-port"));
            println!("Connecting to TCP socket {}", &addr);
            handle = gio::spawn_blocking(move || {
                let stream = StreamWrapper::new_tcp(TcpStream::connect(addr).map_err(mpd::error::Error::Io)?);
                mpd::Client::new(stream)
            });
        }
        match handle.await {
            Ok(Ok(mut client)) => {
                // Set to maximum supported level first. Any subsequent sticker command will then
                // update it to a lower state upon encountering related errors.
                // Euphonica relies on 0.24+ stickers capabilities. Disable if connected to
                // an older daemon.
                if client.version.1 < 24 {
                    self.state.set_stickers_support_level(StickersSupportLevel::SongsOnly);
                }
                else {
                    self.state.set_stickers_support_level(StickersSupportLevel::All);
                }
                // If there is a password configured, use it to authenticate.
                let mut password_access_failed = false;
                let client_password: Option<String>;
                match get_mpd_password().await {
                    Ok(maybe_password) => {
                        match maybe_password {
                            Some(password) => {
                                let password_res = client.login(password.as_str());
                                client_password = Some(password.as_str().to_owned());
                                if let Err(MpdError::Server(se)) = password_res {
                                    let _ = client.close();
                                    if se.code == MpdErrorCode::Password {
                                        self.state
                                            .set_connection_state(ConnectionState::WrongPassword);
                                    } else {
                                        self.state
                                            .set_connection_state(ConnectionState::NotConnected);
                                    }
                                    return;
                                }
                            }
                            None => {
                                // We'll also reach here if denied keyring access.
                                client_password = None;
                            }
                        }
                    }
                    Err(e) => {
                        // Only reachable by glib async runner errors
                        client_password = None;
                        println!("{:?}", &e);
                        password_access_failed = true;
                    }
                }
                // Doubles as a litmus test to see if we are authenticated.
                if let Err(MpdError::Server(se)) = client.subscribe(self.bg_channel.clone()) {
                    if se.code == MpdErrorCode::Permission {
                        self.state.set_connection_state(
                            if password_access_failed {
                                ConnectionState::CredentialStoreError
                            } else if client_password.is_none() {
                                ConnectionState::PasswordNotAvailable
                            } else {
                                ConnectionState::Unauthenticated
                            }
                        );
                    }
                } else {
                    self.main_client.replace(Some(client));
                    self.start_bg_thread(client_password);
                    self.state.set_connection_state(ConnectionState::Connected);
                }
            }
            e => {
                let _ = dbg!(e);
                self.state
                    .set_connection_state(
                        if use_unix_socket {
                            ConnectionState::SocketNotFound
                        } else {
                            ConnectionState::ConnectionRefused
                        }
                    );
            }
        }
    }

    fn force_idle(&self) {
        if !self.pending_idle.load(Ordering::Relaxed) {
            self.pending_idle.store(true, Ordering::Relaxed);
        }
    }

    fn handle_common_mpd_error(&self, e: &MpdError, or: Option<ClientError>) {
        let mut handled = true;
        match *e {
            MpdError::Io(_) => {
                let _ = self.main_sender.send_blocking(AsyncClientMessage::Connect);
            }
            _ => {
                handled = false;
            }
        }

        if !handled {
            if let Some(or_msg) = or {
                self.state.emit_error(or_msg);
            }
        }
    }

    fn handle_set_error<T>(&self, res: Result<T, MpdError>) {
        match res {
            Ok(_) => {
                self.force_idle();
            }
            Err(mpd_err) => {
                self.handle_common_mpd_error(&mpd_err, None);
            }
        }
    }

    fn handle_set_error_or<T>(&self, res: Result<T, MpdError>, msg: ClientError) {
        match res {
            Ok(_) => {
                self.force_idle();
            }
            Err(mpd_err) => {
                self.handle_common_mpd_error(&mpd_err, Some(msg));
            }
        }
    }

    fn handle_get_error<T>(&self, res: Result<T, MpdError>) -> Option<T> {
        match res {
            Ok(val) => {
                Some(val)
            }
            Err(mpd_err) => {
                self.handle_common_mpd_error(&mpd_err, None);
                None
            }
        }
    }

    pub fn get_volume(&self) -> Option<i8> {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            self.handle_get_error(client.getvol())
        }
        else {
            None
        }
    }

    pub fn volume(&self, vol: i8) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            // Don't attempt reconnection here since this thing can rapid-fire.
            let _ = client.volume(vol);
            self.force_idle();
        }
    }

    pub fn get_outputs(&self) -> Option<Vec<Output>> {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            self.handle_get_error(client.outputs())
        } else {
            None
        }
    }

    pub fn set_output(&self, id: u32, state: bool) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            self.handle_set_error(client.output(id, state));
        }
    }

    fn handle_sticker_server_error(&self, err: ServerError) {
        match err.code {
            MpdErrorCode::UnknownCmd => {
                self.state.set_stickers_support_level(StickersSupportLevel::Disabled);
            }
            MpdErrorCode::Argument => {
                self.state.set_stickers_support_level(StickersSupportLevel::SongsOnly);
            }
            _ => {}
        }
    }

    pub fn get_sticker(&self, typ: &str, uri: &str, name: &str) -> Option<String> {
        let min_lvl = if typ == "song" { StickersSupportLevel::SongsOnly } else { StickersSupportLevel::All };
        if let (true, Some(client)) = (self.state.get_stickers_support_level() >= min_lvl, self.main_client.borrow_mut().as_mut()) {
            match client.sticker(typ, uri, name) {
                Ok(sticker) => {
                    return Some(sticker);
                }
                Err(error) => {
                    if let MpdError::Server(server_err) = error {
                        self.handle_sticker_server_error(server_err);
                    } else {
                        self.handle_common_mpd_error(&error, None);
                    }
                    return None;
                }
            }
        }
        return None;
    }

    pub fn set_sticker(&self, typ: &str, uri: &str, name: &str, value: &str) {
        let min_lvl = if typ == "song" { StickersSupportLevel::SongsOnly } else { StickersSupportLevel::All };
        if let (true, Some(client)) = (self.state.get_stickers_support_level() >= min_lvl, self.main_client.borrow_mut().as_mut()) {
            match client.set_sticker(typ, uri, name, value) {
                Ok(()) => {self.force_idle();},
                Err(error) => {
                    if let MpdError::Server(server_err) = error {
                        self.handle_sticker_server_error(server_err);
                    } else {
                        self.handle_common_mpd_error(&error, None);
                    }
                },
            }
        }
    }

    pub fn delete_sticker(&self, typ: &str, uri: &str, name: &str) {
        let min_lvl = if typ == "song" { StickersSupportLevel::SongsOnly } else { StickersSupportLevel::All };
        if let (true, Some(client)) = (self.state.get_stickers_support_level() > min_lvl, self.main_client.borrow_mut().as_mut()) {
            match client.delete_sticker(typ, uri, name) {
                Ok(()) => {self.force_idle();},
                Err(error) => {
                    if let MpdError::Server(server_err) = error {
                        self.handle_sticker_server_error(server_err);
                    } else {
                        self.handle_common_mpd_error(&error, None);
                    }
                }
            }
        }
    }

    fn handle_playlist_error(&self, err: &ServerError) {
        if err.detail.contains("disabled") {
            self.state.set_supports_playlists(false);
            println!("Playlists are not supported.");
        } else {
            println!("Playlist operation error: {:?}", err);
        }
    }

    pub fn get_playlists(&self) -> Vec<INode> {
        // TODO: Might want to move to child thread
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            match client.playlists() {
                Ok(playlists) => {
                    self.state.set_supports_playlists(true);
                    // Convert mpd::Playlist to our INode GObject
                    return playlists
                        .into_iter()
                        .map(INode::from)
                        .collect::<Vec<INode>>();
                }
                Err(e) => {
                    if let MpdError::Server(server_err) = &e {
                        self.handle_playlist_error(server_err);
                    } else {
                        self.handle_common_mpd_error(&e, None);
                    }
                }
            }
        }
        return Vec::with_capacity(0);
    }

    pub fn load_playlist(&self, name: &str) -> Result<(), Option<MpdError>> {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            match client.load(name, ..) {
                Ok(()) => {
                    self.force_idle();
                    self.state.set_supports_playlists(true);
                    return Ok(());
                }
                Err(e) => {
                    if let MpdError::Server(server_err) = &e {
                        self.handle_playlist_error(server_err);
                    } else {
                        self.handle_common_mpd_error(&e, None);
                    }
                    return Err(Some(e));
                }
            }
        }
        return Err(None);
    }

    pub fn save_queue_as_playlist(
        &self,
        name: &str,
        save_mode: SaveMode,
    ) -> Result<(), Option<MpdError>> {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            match client.save(name, Some(save_mode)) {
                Ok(()) => {
                    self.force_idle();
                    self.state.set_supports_playlists(true);
                    return Ok(());
                }
                Err(e) => {
                    if let MpdError::Server(server_err) = &e {
                        self.handle_playlist_error(server_err);
                    } else {
                        // TODO: auto retry
                        self.handle_common_mpd_error(&e, None);
                    }
                    return Err(Some(e));
                }
            }
        }
        return Err(None);
    }

    pub fn rename_playlist(&self, old_name: &str, new_name: &str) -> Result<(), Option<MpdError>> {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            match client.pl_rename(old_name, new_name) {
                Ok(()) => {
                    self.force_idle();
                    Ok(())
                },
                Err(e) => {
                    if let MpdError::Server(server_err) = &e {
                        self.handle_playlist_error(server_err);
                    } else {
                        self.handle_common_mpd_error(&e, None);
                    }
                    return Err(Some(e));
                },
            }
        } else {
            Err(None)
        }
    }

    pub fn edit_playlist(&self, actions: &[EditAction]) -> Result<(), Option<MpdError>> {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            match client.pl_edit(actions) {
                Ok(()) => {
                    self.force_idle();
                    Ok(())
                },
                Err(e) => {
                    if let MpdError::Server(server_err) = &e {
                        self.handle_playlist_error(server_err);
                    } else {
                        self.handle_common_mpd_error(&e, None);
                    }
                    return Err(Some(e));
                }
            }
        } else {
            Err(None)
        }
    }

    pub fn delete_playlist(&self, name: &str) -> Result<(), Option<MpdError>> {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            match client.pl_remove(name) {
                Ok(()) => {
                    self.force_idle();
                    Ok(())
                },
                Err(e) => {
                    if let MpdError::Server(server_err) = &e {
                        self.handle_playlist_error(server_err);
                    } else {
                        self.handle_common_mpd_error(&e, None);
                    }
                    return Err(Some(e));
                }
            }
        } else {
            Err(None)
        }
    }

    pub fn get_status(&self, sync_queue: bool) -> Option<mpd::Status> {
        // Stop borrowing main client as soon as possible
        let res: Option<Result<mpd::Status, MpdError>>;
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            res = Some(client.status());
        }
        else {
            res = None;
        }
        match res {
            Some(Ok(status)) => {
                // Check whether we need to sync queue with server side (inefficient)
                if sync_queue {
                    let old_version = self.queue_version.replace(status.queue_version);
                    if status.queue_version > old_version {
                        if status.queue_version > self.expected_queue_version.get() {
                            self.expected_queue_version.set(status.queue_version);
                            self.queue_background(
                                if old_version == 0 {
                                    BackgroundTask::FetchQueue
                                } else {
                                    BackgroundTask::FetchQueueChanges(old_version, status.queue_len)
                                },
                                true
                            );
                        }
                    }
                }
                Some(status)
            }
            Some(Err(err)) => {
                self.handle_common_mpd_error(&err, None);
                None
            }
            None => None
        }
    }

    pub fn get_song_at_queue_id(&self, id: u32) -> Option<Song> {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            match client.songs(mpd::Id(id)) {
                Ok(mut songs) => {
                    if songs.len() > 0 {
                        Some(Song::from(std::mem::take(&mut songs[0])))
                    } else {
                        None
                    }
                }
                Err(err) => {
                    self.handle_common_mpd_error(&err, None);
                    None
                }
            }
        } else {
            None
        }
    }

    pub fn set_playback_flow(&self, flow: PlaybackFlow) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            let repeat: bool;
            let single: bool;
            match flow {
                PlaybackFlow::Sequential => {
                    repeat = false;
                    single = false;
                }
                PlaybackFlow::Repeat => {
                    repeat = true;
                    single = false;
                }
                PlaybackFlow::Single => {
                    repeat = false;
                    single = true;
                }
                PlaybackFlow::RepeatSingle => {
                    repeat = true;
                    single = true;
                }
            }
            self.handle_set_error(
                client.repeat(repeat).and_then(|_| {
                    client.single(single)
                })
            );
        }
    }

    pub fn set_crossfade(&self, fade: f64) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            self.handle_set_error(client.crossfade(fade as i64));
        }
    }

    pub fn set_replaygain(&self, mode: mpd::status::ReplayGain) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            self.handle_set_error(client.replaygain(mode));
        }
    }

    pub fn set_mixramp_db(&self, db: f32) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            self.handle_set_error(client.mixrampdb(db));
        }
    }

    pub fn set_mixramp_delay(&self, delay: f64) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            self.handle_set_error(client.mixrampdelay(delay));
        }
    }

    pub fn set_random(&self, state: bool) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            self.handle_set_error(client.random(state));
        }
    }

    pub fn set_consume(&self, state: bool) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            self.handle_set_error(client.consume(state));
        }
    }

    pub fn pause(&self, is_pause: bool) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            self.handle_set_error(client.pause(is_pause));
        }
    }

    pub fn stop(&self) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            self.handle_set_error(client.stop());
        }
    }

    pub fn prev(&self) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            self.handle_set_error(client.prev());
        }
    }

    pub fn next(&self) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            self.handle_set_error(client.next());
        }
    }

    pub fn play_at(&self, id_or_pos: u32, is_id: bool) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            let res = if is_id {
                client.switch(Id(id_or_pos)).map(|_| ())
            } else {
                client.switch(id_or_pos).map(|_| ())
            };
            self.handle_set_error(res);
        }
    }

    pub fn swap(&self, id1: u32, id2: u32, is_id: bool) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            let res = if is_id {
                client
                    .swap(Id(id1), Id(id2))
            } else {
                client.swap(id1, id2)
            };
            self.handle_set_error(res);
        }
    }

    pub fn delete_at(&self, id_or_pos: u32, is_id: bool) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            let res = if is_id {
                client
                    .delete(Id(id_or_pos))
            } else {
                client
                    .delete(id_or_pos)
            };
            self.handle_set_error(res);
        }
    }

    pub fn clear_queue(&self) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            self.handle_set_error(client.clear());
        }
    }

    pub fn register_local_queue_changes(&self, n_changes: u32) {
        self.expected_queue_version.set(self.expected_queue_version.get() + n_changes);
    }

    pub fn seek_current_song(&self, position: f64) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            self.handle_set_error(client.rewind(position));
        }
    }

    fn on_songs_downloaded(&self, signal_name: &str, tag: Option<String>, songs: Vec<SongInfo>) {
        if !songs.is_empty() {
            if let Some(tag) = tag {
                self.state.emit_by_name::<()>(
                    signal_name,
                    &[
                        &tag,
                        &BoxedAnyObject::new(songs.into_iter().map(Song::from).collect::<Vec<Song>>()),
                    ]
                );
            }
            else {
                self.state.emit_by_name::<()>(
                    signal_name,
                    &[
                        &BoxedAnyObject::new(songs.into_iter().map(Song::from).collect::<Vec<Song>>()),
                    ]
                );
            }
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
            self.state
                .emit_by_name::<()>(signal_name, &[&tag, &album]);
        } else {
            self.state
                .emit_by_name::<()>(signal_name, &[&album]);
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

impl Drop for MpdWrapper {
    fn drop(&mut self) {
        if let Some(mut main_client) = self.main_client.borrow_mut().take() {
            println!("App closed. Closing clients...");
            // First, send stop message
            let _ = main_client.sendmessage(self.bg_channel.clone(), "STOP");
            // Now close the main client, which will trigger an idle message.
            let _ = main_client.close();
            // Now the child thread really should have read the stop_flag.
            // Wait for it to stop.
            if let Some(handle) = self.bg_handle.take() {
                let _ = executor::block_on(handle);
            }
        }
    }
}
