use async_channel::{Receiver, Sender};
use gio::prelude::SettingsExt;
use mpd::{
    Channel, Client, EditAction, GroupedValues, Id, Idle, Output, Query, ReplayGain, SaveMode, Status, Subsystem, Term, Version, error::{Error as MpdError, ProtoError, Result as MpdResult}, search::Window, song::PosIdChange
};
use oneshot::Sender as OneShotSender;
use rand::seq::SliceRandom;
use resolve_path::PathResolveExt;
use rustc_hash::FxHashSet;
use std::{
    borrow::Cow, cmp::Ordering as StdOrdering, net::TcpStream, ops::Range, os::unix::net::UnixStream, result
};

use crate::{
    cache::sqlite, client::stream::StreamWrapper, common::{AlbumInfo, DynamicPlaylist, SongInfo, Stickers, dynamic_playlist::{Ordering, QueryLhs, Rule, StickerObjectType, StickerOperation}, inode::INodeInfo}, player::PlaybackFlow, utils::{self, save_and_register_image}
};

use super::{get_past_unix_timestamp, password, FETCH_LIMIT, BATCH_SIZE};
use super::StickerSetMode;

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
pub enum Error {
    Mpd(MpdError),
    Internal,
    Socket,
    CredentialStore,
    Tcp,
    NotConnected,
    InsufficientStickersSupportLevel, // any better name for this? not a native speaker
    PlaylistNotEnabled,
}

pub type Result<T> = result::Result<T, Error>;

pub type Responder<T> = OneShotSender<Result<T>>;

/// The successor to BackgroundTask.
pub enum Task {
    /// Connects to MPD. Credentials will be read from settings.
    Connect(Responder<Version>),
    /// Disconnects from MPD
    Disconnect(
        /// If true, will also terminate run.
        bool,
        Responder<()>,
    ),
    Ping(Responder<()>),
    /// Send a message to the inter-client channel
    SendMessage(
        /// Content
        String,
        Responder<()>,
    ),
    GetVolume(Responder<i8>),
    SetVolume(i8, Responder<()>),
    GetOutputs(Responder<Vec<Output>>),
    SetOutput(
        /// Output ID
        u32,
        /// On or off?
        bool,
        Responder<()>,
    ),
    GetSticker(
        /// Type
        &'static str,
        /// URI
        String,
        /// Name
        Cow<'static, str>,
        Responder<String>,
    ),
    GetKnownStickers(
        /// Type
        &'static str,
        /// URI
        String,
        Responder<Stickers>,
    ),
    SetSticker(
        /// Type
        &'static str,
        /// URI
        String,
        /// Name
        Cow<'static, str>,
        /// Value
        Cow<'static, str>,
        /// Set mode (overwrite, increment, decrement)
        StickerSetMode,
        Responder<()>,
    ),
    DeleteSticker(
        /// Type
        &'static str,
        /// URI
        String,
        /// Name
        Cow<'static, str>,
        Responder<()>,
    ),
    FindStickerOp(
        /// Type
        &'static str,
        /// Base URI
        String,
        /// Name (LHS)
        Cow<'static, str>,
        /// Operator
        &'static str,
        /// Value (RHS)
        Cow<'static, str>,
        Window,
        Responder<Vec<String>>
    ),
    GetPlaylists(Responder<Vec<INodeInfo>>),
    LoadPlaylist(String, Responder<()>),
    SaveQueueAsPlaylist(
        /// Name to save as
        String,
        /// Save mode
        SaveMode,
        Responder<()>,
    ),
    RenamePlaylist(
        /// Old name
        String,
        /// New name
        String,
        Responder<()>,
    ),
    EditPlaylist(Vec<EditAction<'static>>, Responder<()>),
    DeletePlaylist(String, Responder<()>),
    /// Get status object from MPD. Won't automatically update queue.
    GetStatus(Responder<Status>),
    /// Get the current song at the given queue ID, if any.
    GetSongAtQueueId(Id, Responder<Option<SongInfo>>),
    SetPlaybackFlow(PlaybackFlow, Responder<()>),
    SetCrossfade(i64, Responder<()>),
    SetReplayGain(ReplayGain, Responder<()>),
    SetMixRampDb(f32, Responder<()>),
    SetMixRampDelay(f64, Responder<()>),
    SetRandom(bool, Responder<()>),
    SetConsume(bool, Responder<()>),
    Pause(bool, Responder<()>),
    Stop(Responder<()>),
    Prev(Responder<()>),
    Next(Responder<()>),
    PlayAtId(Id, Responder<()>),
    PlayAtPos(u32, Responder<()>),
    // SwapId(Id, Id, Responder<()>),
    SwapPos(u32, u32, Responder<()>),
    // DeleteAtId(Id, Responder<()>),
    DeleteAtPos(u32, Responder<()>),
    ClearQueue(Responder<()>),
    Seek(f64, Responder<()>),
    GetQueue(Window, Responder<Vec<SongInfo>>),
    GetQueueChanges(
        /// From version
        u32,
        Window,
        Responder<Vec<PosIdChange>>
    ),
    UpdateDb(Responder<u32>),
    /// Get a song's embedded cover.
    /// Will try to download from MPD if one isn't already available locally.
    GetEmbeddedCover(
        /// URI to song file
        String,
        /// Full paths to high-resolution and low-resolution file, respectively
        Responder<Option<(String, String)>>
    ),
    /// Get a song's folder cover (cover.jpg/png/webp in the same folder).
    /// Will try to download from MPD if one isn't already available locally.
    GetFolderCover(
        /// URI to folder with trailing slash
        String,
        /// Full paths to high-resolution and low-resolution file, respectively
        Responder<Option<(String, String)>>
    ),
    /// Query distinct values of a tag, optionally grouped by another
    List(Term<'static>, Query<'static>, Option<&'static str>, Responder<GroupedValues>),
    Find(Query<'static>, Window, Responder<Vec<SongInfo>>),
    LsInfo(String, Responder<Vec<INodeInfo>>),
    GetPlaylist(
        /// Playlist name
        String,
        /// Fetch window. Do NOT use when connected to clients older than v0.24.
        Option<std::ops::Range<u32>>,
        Responder<Vec<SongInfo>>
    ),
    /// Append song at URI to queue.
    Add(String, Responder<Id>),
    /// Append multiple URIs to the queue.
    /// This utilises commandlists for better efficiency.
    AddMultiple(Vec<String>, Responder<Vec<Id>>),
    /// Insert song at URI into queue at given position.
    Insert(String, usize, Responder<usize>),
    /// Insert multiple URIs into given position on queue.
    /// This utilises commandlists for better efficiency.
    InsertMultiple(Vec<String>, usize, Responder<Vec<usize>>),
    FindAdd(Query<'static>, Responder<()>),
    ClearTagTypes(Responder<()>),
    EnableTagTypes(
        /// If none, will enable all tag types
        Option<Vec<&'static str>>,
        Responder<()>
    ),
    ResolveDynamicPlaylist(
        /// The DP itself
        DynamicPlaylist,
        /// Cache to SQLite?
        bool,
        Responder<Vec<SongInfo>>
    )
}

/// Asynchronous wrapper around an rust-mpd client instance.
/// This is meant to be run on a background thread. Internally
/// we remain synchronous, using a task queue to process UI
/// requests sequentially. We respond to the main thread via
/// oneshot channels to appear synchronous.
///
/// If constructed as a background client, we will go into
/// idle mode after exhausting both queues. In this mode we will
/// listen to server-side changes, but will be unable to respond
/// to incoming tasks. To break out of idle mode, the wrapper must
/// send a WAKE message via the MPD channel given at connect time.
///
/// The design is very similar to asyncified, but implementing
/// this manually allows for custom behaviour such as going into
/// idle listener mode after clearing the task queue.
pub struct Connection {
    receiver: Receiver<Task>,
    // high_receiver: Receiver<Task<'a>>,
    client: Option<Client<StreamWrapper>>,
    /// MPD inter-client channel for communication between Euphonica connections
    wake_channel: Channel,
    /// For sending idle subsystem notifications to the wrapper.
    idle_sender: Option<Sender<Subsystem>>,
    max_retries: u32,
    retries_left: u32
}

fn respond<T>(result: Result<T>, resp: Responder<T>) {
    resp.send(result).expect("Broken oneshot sender");
}

impl Connection {
    /// If idle_sender is given, will initialise this client as background
    pub fn new(
        receiver: Receiver<Task>,
        // high_receiver: Receiver<Task<'a>>,
        wake_channel: Channel,
        idle_sender: Option<Sender<Subsystem>>,
        max_retries: u32
    ) -> Self {
        Self {
            receiver,
            // high_receiver,
            client: None,
            wake_channel,
            idle_sender,
            max_retries,
            retries_left: max_retries
        }
    }

    pub fn connect(&mut self) -> Result<Version> {
        if let Err(e) = self.disconnect() {
            println!("Warning: did not cleanly disconnect");
        }
        let settings = utils::settings_manager().child("client");
        dbg!("Disconnected successfully");

        // self.state.set_connection_state(ConnectionState::Connecting);
        let use_unix_socket = settings.boolean("mpd-use-unix-socket");
        let mut client = if use_unix_socket {
            let path = settings.string("mpd-unix-socket");
            let path = path.as_str();
            println!("Connecting to local socket {}", &path);
            if let Ok(resolved) = path.try_resolve() {
                mpd::Client::new(StreamWrapper::new_unix(
                    UnixStream::connect(resolved).map_err(|_| Error::Socket)?,
                ))
                .map_err(Error::Mpd)?
            } else {
                mpd::Client::new(StreamWrapper::new_unix(
                    UnixStream::connect(path).map_err(|_| Error::Socket)?,
                ))
                .map_err(Error::Mpd)?
            }
        } else {
            let addr = format!(
                "{}:{}",
                settings.string("mpd-host"),
                settings.uint("mpd-port")
            );
            println!("Connecting to TCP socket {}", &addr);
            mpd::Client::new(StreamWrapper::new_tcp(
                TcpStream::connect(addr).map_err(|_| Error::Tcp)?,
            ))
            .map_err(Error::Mpd)?
        };

        dbg!("Connected, now authenticating");

        // If there is a password configured, use it to authenticate.
        if let Some(password) = password::get_mpd_password().map_err(|_| Error::CredentialStore)? {
            client.login(&password).map_err(Error::Mpd)?;
        }

        dbg!("Successfully authenticated");

        // Doubles as a litmus test to see if we are authenticated.
        client
            .subscribe(&self.wake_channel)
            .map_err(Error::Mpd)?;

        dbg!("Subscribed to wake channel");

        let version = client.version;
        self.client.replace(client);

        Ok(version)
    }

    pub fn disconnect(&mut self) -> Result<()> {
        if let Some(mut client) = self.client.take() {
            println!("Closing connection");

            // Now close the main client
            client.close().map_err(Error::Mpd)?;
        }

        Ok(())
    }

    #[inline]
    fn client_then<F, T>(&mut self, then: F) -> Result<T>
    where
        F: Fn(&mut Client<StreamWrapper>) -> MpdResult<T>,
    {
        let final_res: Result<T>;
        loop {
            match self.client.as_mut().map_or(Err(Error::NotConnected), |client| then(client).map_err(Error::Mpd)) {
                Ok(res) => {
                    final_res = Ok(res);
                    self.retries_left = self.max_retries;
                    break;
                }
                Err(e) => {
                    match e {
                        Error::Mpd(MpdError::Io(_)) => {
                            println!("Connection error while performing an action. Reconnecting...");
                            dbg!(&e);
                            let _ = self.disconnect();
                            if self.retries_left > 0 {
                                self.retries_left -= 1;
                                let _ = self.connect();
                            } else {
                                final_res = Err(e);
                                break;
                            }
                        }
                        _ => {
                            final_res = Err(e);
                            break;
                        }
                    }
                }
            }
        }
        final_res
    }

    // Will also handle reconnection & retrying if the first attempt returns MpdError::Io.
    // Subsequent attempts
    #[inline]
    fn respond_with_client<F, T>(&mut self, then: F, resp: Responder<T>)
    where
        F: Fn(&mut Client<StreamWrapper>) -> MpdResult<T>,
    {
        respond(self.client_then(then), resp);
    }

    fn maybe_download_image<F>(&mut self, uri: String, download_func: F, resp: Responder<Option<(String, String)>>)
    where F: Fn(&mut Client<StreamWrapper>, &String) -> MpdResult<Vec<u8>> {
        // Always check with our DB first, as multiple calls may be spawned
        // asynchronously when no cover was locally available.
        // Only one of those calls should cause a download; other calls
        // should start using the local cached version as soon as possible.
        let hires = sqlite::find_image_by_key(&uri, None, false).expect("Sqlite DB error");
        let thumb = sqlite::find_image_by_key(&uri, None, true).expect("Sqlite DB error");
        if let (Some(hires), Some(thumb)) = (hires, thumb) {
            resp.send(Ok(Some((hires, thumb)))).expect("Broken oneshot sender");
        } else {
            // Not available locally => try to download
            self.respond_with_client(|c| {
                match download_func(c, &uri) {
                    Ok(bytes) => {
                        let dyn_img = image::load_from_memory(&bytes).expect("Unable to read image from bytes");
                        Ok(save_and_register_image(Some(dyn_img), &uri, None))
                    }
                    Err(MpdError::Proto(ProtoError::NotPair)) => {
                        println!("GetEmbeddedCover: empty output");
                        // Empty output. Treat as not available.
                        Ok(None)
                    }
                    Err(e) => {
                        Err(e)
                    }
                }
            }, resp);
        }
    }

    fn get_uris_by_sticker(
        &mut self,
        obj: StickerObjectType,
        sticker: Cow<'static, str>,
        op: StickerOperation,
        rhs: Cow<'static, str>,
        only_in: Option<String>,
    ) -> Result<Vec<String>> {
        let mut curr_len: usize = 0;
        let mut more: bool = true;
        let only_in = only_in.unwrap_or(String::from(""));
        let mut res: Vec<String> = Vec::new();
        while more && (curr_len) < FETCH_LIMIT {
            let mut names = self.client_then(|c| c.find_sticker_op(
                obj.to_str(),
                &only_in,
                &sticker,
                op.to_mpd_syntax(),
                &rhs,
                Window::from((curr_len as u32, (curr_len + BATCH_SIZE) as u32))
            ))?;
            if !names.is_empty() {
                // If not searching directly by song (for example by album rating), further resolve to URI.
                match obj {
                    StickerObjectType::Song => {
                        // In this case the names are the URIs themselves
                        res.append(&mut names);
                        curr_len += BATCH_SIZE;
                    }
                    StickerObjectType::Playlist => {
                        // Fetch playlist contents. Don't create GObjects yet.
                        for playlist_name in names.into_iter() {
                            res.append(&mut self.client_then(|c| {
                                // Basically the same as get_playlist_song_infos but does not translate
                                // to SongInfo. Instead we pluck the URI straight from the raw mpd::Song
                                // object. Should be faster.
                                if c.version.1 < 24 {
                                    Ok(
                                        c.playlist(&playlist_name, None::<Range<u32>>)?
                                         .into_iter().map(|s| s.file).collect::<Vec<String>>()
                                    )
                                } else {
                                    // For MPD 0.24+, use the new paged loading
                                    let mut curr_len: u32 = 0;
                                    let mut more: bool = true;
                                    let mut inner_res: Vec<String> = Vec::new();
                                    while more && (curr_len as usize) < FETCH_LIMIT {
                                        let songs = c.playlist(&playlist_name, Some(curr_len..(curr_len + BATCH_SIZE as u32)))?;
                                        more = songs.len() >= BATCH_SIZE;
                                        if !songs.is_empty() {
                                            curr_len += songs.len() as u32;
                                            inner_res.append(&mut songs.into_iter().map(|s| s.file).collect());
                                        }
                                    }
                                    Ok(inner_res)
                                }
                            })?);
                        }
                    }
                    tag_type => {
                        let tag_type_str = tag_type.to_str();
                        // Fetch all songs for each tag
                        for tag_value in names.into_iter() {
                            let mut query = Query::new();
                            query.and(Term::Tag(Cow::Borrowed(tag_type_str)), tag_value);
                            while more && (curr_len) < FETCH_LIMIT {
                                let songs = self.client_then(|c| c.find(
                                    &query,
                                    Window::from((curr_len as u32, (curr_len + BATCH_SIZE) as u32))
                                ))?;
                                if !songs.is_empty() {
                                    res.append(&mut songs.into_iter().map(|s| s.file).collect());
                                    curr_len += BATCH_SIZE;
                                } else {
                                    more = false;
                                }
                            }
                        }
                    }
                }
                curr_len += BATCH_SIZE;
            } else {
                more = false;
            }
        }
        Ok(res)
    }

    fn resolve_dynamic_playlist_rules(&mut self, dp: DynamicPlaylist, cache: bool) -> Result<Vec<SongInfo>> {
        // Resolve filter rules
        // First, separate the search query-based conditions from the sticker ones.
        self.client_then(|c| c.tagtypes_clear())?;
        let mut query_clauses: Vec<(QueryLhs, String)> = Vec::new();
        let mut sticker_clauses: Vec<(StickerObjectType, String, StickerOperation, String)> =
            Vec::new();
        for rule in dp.rules.into_iter() {
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
        let mut uris: FxHashSet<String> = FxHashSet::default();
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
        let mut curr_len: usize = 0;
        let mut more: bool = true;
        while more && (curr_len) < FETCH_LIMIT {
            let songs = self.client_then(|c| c.find(
                &mpd_query,
                Window::from((curr_len as u32, (curr_len + BATCH_SIZE) as u32))
            ))?;
            if !songs.is_empty() {
                for song in songs.into_iter() {
                    uris.insert(song.file);
                }
                curr_len += BATCH_SIZE;
            } else {
                more = false;
            }
        }
        println!("Length after query_clauses: {}", uris.len());

        // Get matching URIs for each sticker condition
        // TODO: Optimise sticker operations by limiting to any found URI query clause.
        for clause in sticker_clauses.into_iter() {
            let mut set = FxHashSet::default();
            match clause.1.as_str() {
                Stickers::LAST_PLAYED_KEY | Stickers::LAST_SKIPPED_KEY => {
                    // Special case: treat RHS as relative to current time
                    for uri in self.get_uris_by_sticker(
                        clause.0,
                        clause.1.into(),
                        clause.2,
                        get_past_unix_timestamp(clause.3.parse::<i64>().unwrap()).to_string().into(),
                        None
                    )? {set.insert(uri);}
                }
                _ => {
                    for uri in self.get_uris_by_sticker(
                        clause.0,
                        clause.1.into(),
                        clause.2,
                        clause.3.into(),
                        None
                    )? {set.insert(uri);}
                }
            }

            println!("Length of matches of sticker_clause: {}", set.len());
            uris.retain(move |elem| set.contains(elem));
            if uris.is_empty() {
                // Return early
                return Ok(Vec::with_capacity(0));
            }
            println!("Length afterwards: {}", uris.len());
        }

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
        self.client_then(|c| c.tagtypes_enable(&tagtypes))?;

        let mut songs_stickers: Vec<(SongInfo, Stickers)> = Vec::with_capacity(uris.len());
        for uri in uris.into_iter() {
            let mut found_songs = self.client_then(|c| c.find(
                Query::new().and(Term::File, &uri),
                        Window::from((0, 1))
            ))?;
            if !found_songs.is_empty() {
                let song = std::mem::take(&mut found_songs[0]);
                let stickers = Stickers::from_mpd_kv(self.client_then(|c| c.stickers("song", &uri))?);
                songs_stickers.push((song.into(), stickers));
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
        self.client_then(|c| c.tagtypes_all())?;
        Ok(songs)
    }

    pub fn run(&mut self) -> Result<()> {
        loop {
            let mut curr_task: Option<Task> = None;
            if !self.receiver.is_empty() || self.idle_sender.is_none() {
                curr_task = Some(
                    self.receiver
                        .recv_blocking()
                        .expect("Unable to read from task queue"),
                );
            }

            if let Some(task) = curr_task {
                match task {
                    Task::Connect(resp) => {
                        respond(self.connect(), resp);
                    }
                    Task::Disconnect(stop, resp) => {
                        let res = self.disconnect();
                        let is_ok = res.is_ok();
                        respond(res, resp);
                        if is_ok && stop {
                            break;
                        }
                    }
                    Task::Ping(resp) => {
                        self.respond_with_client(move |c| c.ping(), resp);
                    }
                    Task::SendMessage(content, resp) => {
                        let wake_channel = self.wake_channel.clone();
                        self.respond_with_client(
                            move |c| c.sendmessage(&wake_channel, &content), resp
                        )
                    },
                    Task::GetVolume(resp) => self.respond_with_client(|c| c.getvol(), resp),
                    Task::SetVolume(val, resp) => self.respond_with_client(|c| c.volume(val), resp),
                    Task::GetOutputs(resp) => self.respond_with_client(|c| c.outputs(), resp),
                    Task::SetOutput(id, state, resp) => self.respond_with_client(
                        |c| c.output(id, state), resp
                    ),
                    Task::GetSticker(typ, uri, name, resp) => self.respond_with_client(
                        |c| c.sticker(typ, &uri, &name), resp
                    ),
                    Task::GetKnownStickers(typ, uri, resp) => self
                        .respond_with_client(|c| c.stickers(typ, &uri).map(Stickers::from_mpd_kv), resp),
                    Task::SetSticker(typ, uri, name, val, mode, resp) => self.respond_with_client(
                        |c| match mode {
                            StickerSetMode::Inc => c.inc_sticker(typ, &uri, &name, &val),
                            StickerSetMode::Set => c.set_sticker(typ, &uri, &name, &val),
                            StickerSetMode::Dec => c.dec_sticker(typ, &uri, &name, &val),
                        },
                        resp,
                    ),
                    Task::DeleteSticker(typ, uri, name, resp) => self.respond_with_client(
                        |c| c.delete_sticker(typ, &uri, &name), resp
                    ),
                    Task::FindStickerOp(typ, base_uri, name, op, value, window, resp) => self.respond_with_client(
                        |c| c.find_sticker_op(typ, &base_uri, &name, op, &value, window), resp
                    ),
                    Task::GetPlaylists(resp) => self.respond_with_client(
                        |c| c.playlists().map(
                            |playlists| playlists.into_iter().map(INodeInfo::from).collect()
                        ), resp
                    ),
                    Task::LoadPlaylist(name, resp) => self.respond_with_client(|c| c.load(&name, ..), resp),
                    Task::SaveQueueAsPlaylist(name, mode, resp) => self.respond_with_client(
                        |c| c.save(&name, Some(mode)), resp
                    ),
                    Task::RenamePlaylist(old, new, resp) => self.respond_with_client(
                        |c| c.pl_rename(&old, &new), resp
                    ),
                    Task::EditPlaylist(actions, resp) => {
                        self.respond_with_client(|c| c.pl_edit(&actions), resp)
                    }
                    Task::DeletePlaylist(name, resp) => {
                        self.respond_with_client(|c| c.pl_remove(&name), resp)
                    }
                    Task::GetStatus(resp) => self.respond_with_client(|c| c.status(), resp),
                    Task::GetSongAtQueueId(id, resp) => self.respond_with_client(
                        |c| {
                            c.songs(id).map(|mut songs| {
                                if !songs.is_empty() {
                                    // Found a song. Now fetch its stickers.
                                    let res = SongInfo::from(std::mem::take(&mut songs[0]));
                                    Some(res)
                                } else {
                                    None
                                }
                            })
                        },
                        resp,
                    ),
                    Task::SetPlaybackFlow(flow, resp) => self.respond_with_client(
                        |c| {
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
                            c.repeat(repeat).and_then(|_| c.single(single))
                        },
                        resp,
                    ),
                    Task::SetCrossfade(fade, resp) => self.respond_with_client(|c| c.crossfade(fade), resp),
                    Task::SetReplayGain(mode, resp) => {
                        self.respond_with_client(|c| c.replaygain(mode), resp)
                    }
                    Task::SetMixRampDb(db, resp) => self.respond_with_client(|c| c.mixrampdb(db), resp),
                    Task::SetMixRampDelay(delay, resp) => {
                        self.respond_with_client(|c| c.mixrampdelay(delay), resp)
                    }
                    Task::SetRandom(state, resp) => self.respond_with_client(|c| c.random(state), resp),
                    Task::SetConsume(state, resp) => self.respond_with_client(|c| c.consume(state), resp),
                    Task::Pause(state, resp) => self.respond_with_client(|c| c.pause(state), resp),
                    Task::Stop(resp) => self.respond_with_client(|c| c.stop(), resp),
                    Task::Prev(resp) => self.respond_with_client(|c| c.prev(), resp),
                    Task::Next(resp) => self.respond_with_client(|c| c.next(), resp),
                    Task::PlayAtId(id, resp) => self.respond_with_client(|c| c.switch(id), resp),
                    Task::PlayAtPos(pos, resp) => self.respond_with_client(|c| c.switch(pos), resp),
                    Task::SwapPos(p1, p2, resp) => self.respond_with_client(|c| c.swap(p1, p2), resp),
                    Task::DeleteAtPos(p, resp) => self.respond_with_client(|c| c.delete(p), resp),
                    Task::ClearQueue(resp) => self.respond_with_client(|c| c.clear(), resp),
                    Task::Seek(pos, resp) => self.respond_with_client(|c| c.rewind(pos), resp),
                    Task::GetQueue(window, resp) => self.respond_with_client(
                        |c| c.queue(window).map(
                            |mpd_songs| mpd_songs.into_iter().map(SongInfo::from).collect()
                        ),
                        resp
                    ),
                    Task::GetQueueChanges(since, window, resp) => self.respond_with_client(
                        |c| c.changesposid(since, window), resp
                    ),
                    Task::UpdateDb(resp) => self.respond_with_client(|c| c.update(), resp),
                    Task::GetEmbeddedCover(uri, resp) => self.maybe_download_image(
                        uri,
                        |client, uri| {client.readpicture(uri)},
                        resp
                    ),
                    Task::GetFolderCover(folder_uri, resp) => self.maybe_download_image(
                        folder_uri,
                        |client, uri| {client.albumart(uri)},
                        resp
                    ),
                    Task::List(term, query, groupby, resp) => self.respond_with_client(
                        |c| c.list(&term, &query, groupby), resp
                    ),
                    Task::Find(query, window, resp) => self.respond_with_client(
                        |c| c.find(&query, window).map(|mpd_songs| {
                            mpd_songs.into_iter().map(SongInfo::from).collect()
                        }
                    ), resp),
                    Task::LsInfo(path, resp) => self.respond_with_client(|c| {
                        c.lsinfo(&path)
                         .map(|entries| entries.into_iter().map(INodeInfo::from).collect())
                    }, resp),
                    Task::GetPlaylist(name, window, resp) => self.respond_with_client(
                        |c| c.playlist(&name, window.clone()).map(
                            |mpd_songs| mpd_songs.into_iter().map(SongInfo::from).collect()
                        ), resp
                    ),
                    Task::Add(uri, resp) => self.respond_with_client(|c| c.push(&uri), resp),
                    Task::AddMultiple(uris, resp) => self.respond_with_client(|c| c.push_multiple(&uris), resp),
                    Task::Insert(uri, pos, resp) => self.respond_with_client(|c| c.insert(&uri, pos), resp),
                    Task::InsertMultiple(uris, pos, resp) => self.respond_with_client(|c| c.insert_multiple(&uris, pos), resp),
                    Task::FindAdd(query, resp) => self.respond_with_client(|c| c.findadd(&query), resp),
                    Task::ClearTagTypes(resp) => self.respond_with_client(|c| c.tagtypes_clear(), resp),
                    Task::EnableTagTypes(types, resp) => self.respond_with_client(move |c| {
                        if let Some(types) = types.as_deref() {
                            c.tagtypes_enable(types)
                        } else {
                            c.tagtypes_all()
                        }
                    }, resp),
                    Task::ResolveDynamicPlaylist(dp, cache, resp) => {
                        respond(self.resolve_dynamic_playlist_rules(dp, cache), resp);
                    }
                }
            } else if let (Some(sender), Some(client)) =
                (self.idle_sender.as_ref(), self.client.as_mut())
            {
                let changes = client.wait(&[]).map_err(Error::Mpd)?;
                for change in changes.iter() {
                    match change {
                        Subsystem::Message => {
                            if let Ok(msgs) = client.readmessages() {
                                for msg in msgs {
                                    let content = msg.message.as_str();
                                    // Send any message to get out of wait().
                                    println!("Client received message: {content}");
                                }
                            }
                        }
                        other => {
                            sender.send_blocking(*other).map_err(|_| Error::Internal)?;
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
