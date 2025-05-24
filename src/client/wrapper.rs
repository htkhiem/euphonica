use async_channel::{Receiver, SendError, Sender};
use futures::executor;
use glib::clone;
use gtk::{gio, glib};
use gtk::{gio::prelude::*, glib::BoxedAnyObject};
use keyring::{error::Error as KeyringError, Entry};
use mpd::error::ServerError;
use mpd::{
    client::Client,
    error::{Error as MpdError, ErrorCode as MpdErrorCode},
    lsinfo::LsInfoEntry,
    search::{Operation as QueryOperation, Query, Term, Window},
    song::Id,
    Channel, EditAction, Idle, Output, SaveMode, Subsystem,
};
use rustc_hash::FxHashSet;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
};
use uuid::Uuid;

use crate::common::Stickers;
use crate::{
    common::{Album, AlbumInfo, Artist, ArtistInfo, INode, Song, SongInfo},
    meta_providers::ProviderMessage,
    player::PlaybackFlow,
    utils,
};

use super::state::{ClientState, ConnectionState, StickersSupportLevel};

const BATCH_SIZE: u32 = 1024;
const FETCH_LIMIT: usize = 10000000; // Fetch at most ten million songs at once (same
                                     // folder, same tag, etc)

// Messages to be sent from child thread or synchronous methods
enum AsyncClientMessage {
    Connect, // Host and port are always read from gsettings
    Busy(bool), // A true will be sent when the work queue starts having tasks, and a false when it is empty again.
    Idle(Vec<Subsystem>), // Will only be sent from the child thread
    AlbumBasicInfoDownloaded(AlbumInfo), // Return new album to be added to the list model.
    AlbumSongInfoDownloaded(String, Vec<SongInfo>), // Return songs in the album with the given tag (batched)
    ArtistBasicInfoDownloaded(ArtistInfo), // Return new artist to be added to the list model.
    ArtistSongInfoDownloaded(String, Vec<SongInfo>), // Return songs of an artist (or had their participation)
    ArtistAlbumBasicInfoDownloaded(String, AlbumInfo), // Return albums that had this artist in their AlbumArtist tag.
    FolderContentsDownloaded(String, Vec<LsInfoEntry>),
    PlaylistSongInfoDownloaded(String, Vec<SongInfo>),
    DBUpdated,
}

// Work requests for sending to the child thread.
// Completed results will be reported back via MpdMessage.
#[derive(Debug)]
pub enum BackgroundTask {
    Update,
    DownloadAlbumArt(AlbumInfo, PathBuf, PathBuf), // folder-level URI
    FetchFolderContents(String), // Gradually get all inodes in folder at path
    FetchAlbums,                 // Gradually get all albums
    FetchAlbumSongs(String),     // Get songs of album with given tag
    FetchArtists(bool), // Gradually get all artists. If bool flag is true, will parse AlbumArtist tag
    FetchArtistSongs(String), // Get all songs of an artist with given name
    FetchArtistAlbums(String), // Get all albums of an artist with given name
    FetchPlaylistSongs(String), // Get songs of playlist with given name
}
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

mod background {
    use std::ops::Range;

    use super::*;
    pub fn update_mpd_database(
        client: &mut mpd::Client,
        sender_to_fg: &Sender<AsyncClientMessage>,
    ) {
        if let Ok(_) = client.update() {
            let _ = sender_to_fg.send_blocking(AsyncClientMessage::DBUpdated);
        }
    }

    pub fn download_album_art(
        client: &mut mpd::Client,
        sender_to_cache: &Sender<ProviderMessage>,
        key: AlbumInfo,
        path: PathBuf,
        thumbnail_path: PathBuf,
    ) {
        let uri = key.uri.to_owned();
        if !path.exists() || !thumbnail_path.exists() {
            // println!("Downloading album art for {:?}", &uri);
            if let Ok(bytes) = client.albumart(&uri) {
                // println!("Downloaded album art for {:?}", &uri);
                if let Some(dyn_img) = utils::read_image_from_bytes(bytes) {
                    let (hires, thumb) = utils::resize_convert_image(dyn_img);

                    if let (Ok(_), Ok(_)) = (hires.save(path), thumb.save(thumbnail_path)) {
                        sender_to_cache
                            .send_blocking(ProviderMessage::AlbumArtAvailable(uri))
                            .expect("Cannot notify main cache of album art download result.");
                    }
                }
            } else {
                // Fetch from local sources instead.
                sender_to_cache
                    .send_blocking(ProviderMessage::AlbumArtNotAvailable(key))
                    .expect("Album art not available from MPD, but cannot notify cache of this.");
            }
        } else {
            // println!("Skipped downloading album art for {:?}", &uri);
        }
    }

    fn fetch_albums_by_query<F>(client: &mut mpd::Client, query: &Query, respond: F)
    where
        F: Fn(AlbumInfo) -> Result<(), SendError<AsyncClientMessage>>,
    {
        // TODO: batched windowed retrieval
        // Get list of unique album tags
        // Will block child thread until info for all albums have been retrieved.
        if let Ok(tag_list) = client.list(&Term::Tag(Cow::Borrowed("album")), query) {
            for tag in &tag_list {
                if let Ok(mut songs) = client.find(
                    Query::new().and(Term::Tag(Cow::Borrowed("album")), tag),
                    Window::from((0, 1)),
                ) {
                    if !songs.is_empty() {
                        let info = SongInfo::from(std::mem::take(&mut songs[0]))
                            .into_album_info()
                            .unwrap_or_default();
                        let _ = respond(info);
                    }
                }
            }
        }
    }

    fn fetch_songs_by_query<F>(client: &mut mpd::Client, query: &Query, respond: F)
    where
        F: Fn(Vec<SongInfo>) -> Result<(), SendError<AsyncClientMessage>>,
    {
        let mut curr_len: u32 = 0;
        let mut more: bool = true;
        while more && (curr_len as usize) < FETCH_LIMIT {
            let songs: Vec<SongInfo> = client
                .find(query, Window::from((curr_len, curr_len + BATCH_SIZE)))
                .unwrap()
                .iter_mut()
                .map(|mpd_song| SongInfo::from(std::mem::take(mpd_song)))
                .collect();
            if !songs.is_empty() {
                let _ = respond(songs);
                curr_len += BATCH_SIZE;
            } else {
                more = false;
            }
        }
    }

    pub fn fetch_all_albums(client: &mut mpd::Client, sender_to_fg: &Sender<AsyncClientMessage>) {
        println!("FETCH_ALL_ALBUMS called");
        fetch_albums_by_query(client, &Query::new(), |info| {
            sender_to_fg.send_blocking(AsyncClientMessage::AlbumBasicInfoDownloaded(info))
        });
    }

    pub fn fetch_albums_of_artist(
        client: &mut mpd::Client,
        sender_to_fg: &Sender<AsyncClientMessage>,
        artist_name: String,
    ) {
        fetch_albums_by_query(
            client,
            Query::new().and_with_op(
                Term::Tag(Cow::Borrowed("artist")),
                QueryOperation::Contains,
                artist_name.clone(),
            ),
            |info| {
                sender_to_fg.send_blocking(AsyncClientMessage::ArtistAlbumBasicInfoDownloaded(
                    artist_name.clone(),
                    info,
                ))
            },
        );
    }

    pub fn fetch_album_songs(
        client: &mut mpd::Client,
        sender_to_fg: &Sender<AsyncClientMessage>,
        tag: String,
    ) {
        fetch_songs_by_query(
            client,
            Query::new().and(Term::Tag(Cow::Borrowed("album")), tag.clone()),
            |songs| {
                sender_to_fg.send_blocking(AsyncClientMessage::AlbumSongInfoDownloaded(
                    tag.clone(),
                    songs,
                ))
            },
        );
    }

    pub fn fetch_artists(
        client: &mut mpd::Client,
        sender_to_fg: &Sender<AsyncClientMessage>,
        use_album_artist: bool,
    ) {
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
        if let Ok(tag_list) = client.list(&Term::Tag(Cow::Borrowed(tag_type)), &Query::new()) {
            // TODO: Limit tags to only what we need locally
            for tag in &tag_list {
                if let Ok(mut songs) = client.find(
                    Query::new().and(Term::Tag(Cow::Borrowed(tag_type)), tag),
                    Window::from((0, 1)),
                ) {
                    if !songs.is_empty() {
                        let first_song = SongInfo::from(std::mem::take(&mut songs[0]));
                        let artists = first_song.into_artist_infos();
                        // println!("Got these artists: {artists:?}");
                        for artist in artists.into_iter() {
                            if already_parsed.insert(artist.name.clone()) {
                                // println!("Never seen {artist:?} before, inserting...");
                                let _ = sender_to_fg.send_blocking(
                                    AsyncClientMessage::ArtistBasicInfoDownloaded(artist),
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn fetch_songs_of_artist(
        client: &mut mpd::Client,
        sender_to_fg: &Sender<AsyncClientMessage>,
        name: String,
    ) {
        fetch_songs_by_query(
            client,
            Query::new().and_with_op(
                Term::Tag(Cow::Borrowed("artist")),
                QueryOperation::Contains,
                name.clone(),
            ),
            |songs| {
                sender_to_fg.send_blocking(AsyncClientMessage::ArtistSongInfoDownloaded(
                    name.clone(),
                    songs,
                ))
            },
        );
    }

    pub fn fetch_folder_contents(
        client: &mut mpd::Client,
        sender_to_fg: &Sender<AsyncClientMessage>,
        path: String,
    ) {
        if let Ok(contents) = client.lsinfo(&path) {
            println!("Downloaded {} folder entries", contents.len());
            let _ = sender_to_fg
                .send_blocking(AsyncClientMessage::FolderContentsDownloaded(path, contents));
        }
    }

    pub fn fetch_playlist_songs(
        client: &mut mpd::Client,
        sender_to_fg: &Sender<AsyncClientMessage>,
        name: String,
    ) {
        // Uncomment these once MPD 0.24 is released
        // let mut curr_len: u32 = 0;
        // let mut more: bool = true;
        // while more && (curr_len as usize) < FETCH_LIMIT {
        //     let songs: Vec<SongInfo> = client
        //         .playlist(&name, curr_len..(curr_len + BATCH_SIZE))
        //         .unwrap()
        //         .iter_mut()
        //         .map(|mpd_song| SongInfo::from(std::mem::take(mpd_song)))
        //         .collect();
        //     more = songs.len() >= BATCH_SIZE as usize;
        //     if !songs.is_empty() {
        //         curr_len += songs.len() as u32;
        //         let _ = sender_to_fg.send_blocking(AsyncClientMessage::PlaylistSongInfoDownloaded(
        //             name.clone(),
        //             songs,
        //         ));
        //     }
        // }
        // For now download all songs at once before sending to main thread
        let songs: Vec<SongInfo> = client
            .playlist(&name, Option::<Range<u32>>::None)
            .unwrap()
            .iter_mut()
            .map(|mpd_song| SongInfo::from(std::mem::take(mpd_song)))
            .collect();
        if !songs.is_empty() {
            let _ = sender_to_fg.send_blocking(AsyncClientMessage::PlaylistSongInfoDownloaded(
                name.clone(),
                songs,
            ));
        }
    }
}

#[derive(Debug)]
pub struct MpdWrapper {
    // Corresponding sender, for cloning into child thread.
    main_sender: Sender<AsyncClientMessage>,
    // The main client living on the main thread. Every single method of
    // mpd::Client is mutating so we'll just rely on a RefCell for now.
    main_client: RefCell<Option<Client>>,
    // The state GObject, used for communicating client status & changes to UI elements
    state: ClientState,
    // Handle to the child thread.
    bg_handle: RefCell<Option<gio::JoinHandle<()>>>,
    bg_channel: Channel, // For waking up the child client
    bg_sender: RefCell<Option<Sender<BackgroundTask>>>, // For sending tasks to background thread
    bg_sender_high: RefCell<Option<Sender<BackgroundTask>>>, // For sending high-priority tasks to background thread
    meta_sender: Sender<ProviderMessage>, // For sending album arts to cache controller
    pending_idle: Arc<AtomicBool>,
    // Stored here so we can use them to get queue diffs.
    // It will be updated every time get_status() is called.
    queue_version: Cell<u32>,
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
        });

        // For future noob self: these are shallow
        wrapper.clone().setup_channel(receiver);
        wrapper
    }

    pub fn get_client_state(&self) -> ClientState {
        self.state.clone()
    }

    fn start_bg_thread(&self, addr: &str, password: Option<&str>) {
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
        if let Ok(mut client) = Client::connect(addr) {
            // If we're unauthenticated, this will fail and no child thread
            // will be spawned.
            if let Some(password) = password {
                client
                    .login(password)
                    .expect("Main client logged in successfully but child thread did not");
            }
            client
                .subscribe(self.bg_channel.clone())
                .expect("Child thread could not subscribe to inter-client channel");
            let bg_handle = gio::spawn_blocking(move || {
                println!("Starting idle loop...");
                let mut busy: bool = false;
                'outer: loop {
                    // Try to fetch a task
                    let curr_task: Option<BackgroundTask>;
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
                    else {
                        curr_task = None;
                    }
                    let skip_to_idle = pending_idle.load(Ordering::Relaxed);
                    if !skip_to_idle && curr_task.is_some() {
                        let task = curr_task.unwrap();
                        if !busy {
                            // We have tasks now, set state to busy
                            busy = true;
                            let _ = sender_to_fg.send_blocking(AsyncClientMessage::Busy(true));
                        }
                        match task {
                            BackgroundTask::Update => {
                                background::update_mpd_database(&mut client, &sender_to_fg)
                            }
                            BackgroundTask::DownloadAlbumArt(key, path, thumbnail_path) => {
                                background::download_album_art(
                                    &mut client,
                                    &meta_sender,
                                    key,
                                    path,
                                    thumbnail_path,
                                )
                            }
                            BackgroundTask::FetchAlbums => {
                                background::fetch_all_albums(&mut client, &sender_to_fg)
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
                        }
                    } else {
                        // If not, go into idle mode
                        if busy {
                            busy = false;
                            let _ = sender_to_fg.send_blocking(AsyncClientMessage::Busy(false));
                        }
                        if skip_to_idle {
                            println!("Background MPD thread skipping to idle mode as there are pending messages");
                            pending_idle.store(false, Ordering::Relaxed);
                        }
                        if let Ok(changes) = client.wait(&[]) {
                            println!("Change: {:?}", changes);
                            if changes.contains(&Subsystem::Message) {
                                if let Ok(msgs) = client.readmessages() {
                                    for msg in msgs {
                                        let content = msg.message.as_str();
                                        println!("Received msg: {}", content);
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
                            break 'outer;
                        }
                    }
                }
            });
            self.bg_handle.replace(Some(bg_handle));
        } else {
            // Since many features now run in the child thread, it is no longer acceptable
            // to run without one.
            panic!("Could not spawn a child thread for the background client!")
        }
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
                while let Some(request) = receiver.next().await {
                    this.respond(request).await;
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
                    if res.is_ok() {
                        println!("[KeepAlive]");
                    }
                    else {
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

    async fn respond(&self, request: AsyncClientMessage) -> glib::ControlFlow {
        // println!("Received MpdMessage {:?}", request);
        match request {
            AsyncClientMessage::Connect => self.connect_async().await,
            // AsyncClientMessage::Disconnect => self.disconnect_async().await,
            AsyncClientMessage::Idle(changes) => self.handle_idle_changes(changes).await,
            AsyncClientMessage::AlbumBasicInfoDownloaded(info) => {
                self.on_album_downloaded("album-basic-info-downloaded", None, info)
            }
            AsyncClientMessage::AlbumSongInfoDownloaded(tag, songs) => {
                self.on_songs_downloaded("album-songs-downloaded", tag, songs)
            }
            AsyncClientMessage::ArtistBasicInfoDownloaded(info) => self
                .state
                .emit_result("artist-basic-info-downloaded", Artist::from(info)),
            AsyncClientMessage::ArtistSongInfoDownloaded(name, songs) => {
                self.on_songs_downloaded("artist-songs-downloaded", name, songs)
            }
            AsyncClientMessage::ArtistAlbumBasicInfoDownloaded(artist_name, album_info) => self
                .on_album_downloaded(
                    "artist-album-basic-info-downloaded",
                    Some(&artist_name),
                    album_info,
                ),
            AsyncClientMessage::FolderContentsDownloaded(uri, contents) => {
                self.on_folder_contents_downloaded(uri, contents)
            }
            AsyncClientMessage::PlaylistSongInfoDownloaded(name, songs) => {
                self.on_songs_downloaded("playlist-songs-downloaded", name, songs)
            }
            AsyncClientMessage::DBUpdated => {}
            AsyncClientMessage::Busy(busy) => self.state.set_busy(busy),
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
            sender
                .send_blocking(task)
                .expect("Cannot queue background task");
            if let Some(client) = self.main_client.borrow_mut().as_mut() {
                // Wake background thread
                let _ = client.sendmessage(self.bg_channel.clone(), "WAKE");
            } else {
                println!("Warning: cannot wake child thread. Task might be delayed.");
            }
        } else {
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

    async fn connect_async(&self) {
        // Close current clients
        self.disconnect_async().await;

        let conn = utils::settings_manager().child("client");

        let addr = format!("{}:{}", conn.string("mpd-host"), conn.uint("mpd-port"));
        println!("Connecting to {}", &addr);
        self.state.set_connection_state(ConnectionState::Connecting);
        let addr_clone = addr.clone();
        let handle = gio::spawn_blocking(move || mpd::Client::connect(addr_clone)).await;
        if let Ok(Ok(mut client)) = handle {
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
            let password_access_failed: bool;
            let client_password: Option<String>;
            match Entry::new("euphonica", "mpd-password") {
                Ok(entry) => {
                    password_access_failed = false;
                    match entry.get_password() {
                        Ok(password) => {
                            let password_res = client.login(&password);
                            client_password = Some(password);
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
                        Err(e) => {
                            client_password = None;
                            match e {
                                KeyringError::NoEntry => {}
                                _ => {
                                    println!("{:?}", e);
                                    let _ = client.close();
                                    self.state.set_connection_state(
                                        ConnectionState::CredentialStoreError,
                                    );
                                    return;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    client_password = None;
                    match e {
                        KeyringError::NoStorageAccess(_) | KeyringError::PlatformFailure(_) => {
                            // Note this down in case we really needed a password (different error
                            // message).
                            password_access_failed = true;
                        }
                        _ => {
                            password_access_failed = false;
                        }
                    }
                }
            }
            // Doubles as a litmus test to see if we are authenticated.
            if let Err(MpdError::Server(se)) = client.subscribe(self.bg_channel.clone()) {
                if se.code == MpdErrorCode::Permission {
                    self.state.set_connection_state(if password_access_failed {
                        ConnectionState::CredentialStoreError
                    } else {
                        ConnectionState::Unauthenticated
                    });
                }
            } else {
                self.main_client.replace(Some(client));
                self.start_bg_thread(addr.as_ref(), client_password.as_deref());
                self.state.set_connection_state(ConnectionState::Connected);
            }
        } else {
            self.state
                .set_connection_state(ConnectionState::NotConnected);
        }
    }

    fn force_idle(&self) {
        if !self.pending_idle.load(Ordering::Relaxed) {
            self.pending_idle.store(true, Ordering::Relaxed);
        }
    }

    pub fn add(&self, uri: String, recursive: bool) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            if recursive {
                let _ = client.findadd(Query::new().and(Term::Base, uri));

            } else {
                let _ = client.push(uri);
            }
            self.force_idle();
        }
    }

    pub fn add_multi(&self, uris: &[String]) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            client.push_multiple(uris).expect("Could not add multiple songs into queue");
            self.force_idle();
        }
    }

    pub fn volume(&self, vol: i8) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            let _ = client.volume(vol);
            self.force_idle();
        }
    }

    pub fn get_outputs(&self) -> Option<Vec<Output>> {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            if let Ok(outputs) = client.outputs() {
                return Some(outputs);
            }
            return None;
        }
        return None;
    }

    pub fn set_output(&self, id: u32, state: bool) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            let _ = client.output(id, state);
            self.force_idle();
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
                    match error {
                        MpdError::Server(server_err) => {
                            self.handle_sticker_server_error(server_err);
                        }
                        _ => {
                            println!("{:?}", error);
                            // Not handled yet
                        }
                    };
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
                Err(err) => match err {
                    MpdError::Server(server_err) => {
                        self.handle_sticker_server_error(server_err);
                    }
                    _ => {
                        println!("{:?}", err);
                        // Not handled yet
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
                Err(err) => match err {
                    MpdError::Server(server_err) => {
                        self.handle_sticker_server_error(server_err);
                    }
                    _ => {
                        // Not handled yet
                    }
                },
            }
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
                Err(e) => match e {
                    MpdError::Server(server_err) => {
                        self.state.set_supports_playlists(false);
                        if server_err.detail.contains("disabled") {
                            println!("Playlists are not supported.");
                        } else {
                            println!("get_playlists: {:?}", server_err);
                        }
                    }
                    _ => {
                        println!("get_playlists: {:?}", e);
                        // Not handled yet
                    }
                },
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
                    match &e {
                        MpdError::Server(server_err) => {
                            if server_err.detail.contains("disabled") {
                                self.state.set_supports_playlists(false);
                            }
                        }
                        _ => {
                            // Not handled yet
                        }
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
                    match &e {
                        MpdError::Server(server_err) => {
                            if server_err.detail.contains("disabled") {
                                self.state.set_supports_playlists(false);
                            }
                        }
                        _ => {
                            // Not handled yet
                        }
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
                Err(e) => Err(Some(e)),
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
                Err(e) => Err(Some(e)),
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
                Err(e) => Err(Some(e)),
            }
        } else {
            Err(None)
        }
    }

    pub fn get_status(&self) -> Option<mpd::Status> {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            let res = client.status();
            match res {
                Ok(status) => {
                    self.queue_version.replace(status.queue_version);
                    return Some(status);
                }
                Err(e) => {
                    println!("{:?}", e);
                    return None;
                }
            }
        }
        return None;
    }

    pub fn set_playback_flow(&self, flow: PlaybackFlow) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            match flow {
                PlaybackFlow::Sequential => {
                    let _ = client.repeat(false);
                    let _ = client.single(false);
                }
                PlaybackFlow::Repeat => {
                    let _ = client.repeat(true);
                    let _ = client.single(false);
                }
                PlaybackFlow::Single => {
                    let _ = client.repeat(false);
                    let _ = client.single(true);
                }
                PlaybackFlow::RepeatSingle => {
                    let _ = client.repeat(true);
                    let _ = client.single(true);
                }
            }
            self.force_idle();
        }
    }

    pub fn set_crossfade(&self, fade: f64) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            if client.crossfade(fade as i64).is_ok() {
                self.force_idle();
            }
        }
    }

    pub fn set_replaygain(&self, mode: mpd::status::ReplayGain) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            if client.replaygain(mode).is_ok() {
                self.force_idle();
            }
        }
    }

    pub fn set_mixramp_db(&self, db: f32) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            if client.mixrampdb(db).is_ok() {
                self.force_idle();
            }
        }
    }

    pub fn set_mixramp_delay(&self, delay: f64) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            if client.mixrampdelay(delay).is_ok() {
                self.force_idle();
            }
        }
    }

    pub fn set_random(&self, state: bool) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            if client.random(state).is_ok() {
                self.force_idle();
            }
        }
    }

    pub fn set_consume(&self, state: bool) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            if client.consume(state).is_ok() {
                self.force_idle();
            }
        }
    }

    pub fn pause(&self, is_pause: bool) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            if client.pause(is_pause).is_ok() {
                self.force_idle();
            }
        }
    }

    pub fn stop(&self) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            if client.stop().is_ok() {
                self.force_idle();
            }
        }
    }

    pub fn prev(&self) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            // TODO: Make it stop/play base on toggle
            if client.prev().is_ok() {
                self.force_idle();
            }
        } else {
            // TODO: handle error
        }
    }

    pub fn next(&self) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            // TODO: Make it stop/play base on toggle
            if client.next().is_ok() {
                self.force_idle();
            }
        } else {
            // TODO: handle error
        }
    }

    pub fn play_at(&self, id_or_pos: u32, is_id: bool) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            if is_id {
                client.switch(Id(id_or_pos)).expect("Could not switch song");
            } else {
                client.switch(id_or_pos).expect("Could not switch song");
            }
            self.force_idle();
        }
    }

    pub fn swap(&self, id1: u32, id2: u32, is_id: bool) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            if is_id {
                client
                    .swap(Id(id1), Id(id2))
                    .expect("Could not swap songs by ID");
            } else {
                client.swap(id1, id2).expect("Could not swap songs by pos");
            }
            self.force_idle();
        }
    }

    pub fn delete_at(&self, id_or_pos: u32, is_id: bool) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            if is_id {
                client
                    .delete(Id(id_or_pos))
                    .expect("Could not delete song from queue");
            } else {
                client
                    .delete(id_or_pos)
                    .expect("Could not delete song from queue");
            }
            self.force_idle();
        }
    }

    pub fn clear_queue(&self) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            client.clear().expect("Could not clear queue");
            self.force_idle();
        }
    }

    pub fn get_queue_changes(&self) -> Option<Vec<Song>> {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            // TODO: move to background thread
            if let Ok(mut changes) = client.changes(self.queue_version.get()) {
                return Some(
                    changes
                        .iter_mut()
                        .map(|mpd_song| Song::from(std::mem::take(mpd_song)))
                        .collect(),
                );
            }
            return None;
        }
        return None;
    }

    pub fn seek_current_song(&self, position: f64) {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            client.rewind(position).expect("Failed to seek song");
            self.force_idle();
        }
    }

    pub fn get_current_queue(&self) -> Option<Vec<Song>> {
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            if let Ok(mut queue) = client.queue() {
                return Some(
                    queue
                        .iter_mut()
                        .map(|mpd_song| Song::from(std::mem::take(mpd_song)))
                        .collect(),
                );
            }
            return None;
        }
        return None;
    }

    fn on_songs_downloaded(&self, signal_name: &str, tag: String, songs: Vec<SongInfo>) {
        if !songs.is_empty() {
            // Append to listener lists
            self.state.emit_by_name::<()>(
                signal_name,
                &[
                    &tag,
                    &BoxedAnyObject::new(songs.into_iter().map(Song::from).collect::<Vec<Song>>()),
                ],
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

    pub fn find_add(&self, query: Query) {
        // Convert back to mpd::search::Query
        if let Some(client) = self.main_client.borrow_mut().as_mut() {
            // println!("Running findadd query: {:?}", &terms);
            // let mut query = Query::new();
            // for term in terms.into_iter() {
            //     query.and(term.0.into(), term.1);
            // }
            client.findadd(&query).expect("Failed to run query!");
            self.force_idle();
        }
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
