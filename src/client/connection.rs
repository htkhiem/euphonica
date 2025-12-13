use async_channel::{Receiver, Sender};
use gio::prelude::SettingsExt;
use mpd::{
    error::{Error as MpdError, Result as MpdResult},
    Channel, Client, EditAction, Id, Idle, Output, ReplayGain, SaveMode, Status, Subsystem,
    Version,
};
use oneshot::Sender as OneShotSender;
use resolve_path::PathResolveExt;
use std::{
    borrow::Cow, cell::RefCell, net::TcpStream, os::unix::net::UnixStream, path::Path, result,
};

use crate::{
    client::stream::StreamWrapper,
    common::{inode::INodeInfo, SongInfo, Stickers},
    player::PlaybackFlow,
    utils,
};

use super::state::StickersSupportLevel;
use super::StickerSetMode;

pub enum Error {
    Mpd(MpdError),
    Internal,
    Socket,
    Tcp,
    NotConnected,
}

pub type Result<T> = result::Result<T, Error>;

pub type Responder<T> = OneShotSender<Result<T>>;

/// The successor to BackgroundTask.
pub enum Task {
    /// Connects to MPD. Credentials will be read from settings.
    Connect(
        /// Password
        Option<String>,
        Responder<Version>,
    ),
    /// Disconnects from MPD
    Disconnect(
        /// If true, will also terminate run.
        bool,
        Responder<()>,
    ),
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
        String,
        /// URI
        String,
        /// Name
        String,
        Responder<String>,
    ),
    GetKnownStickers(
        /// Type
        String,
        /// URI
        String,
        Responder<Stickers>,
    ),
    SetSticker(
        /// Type
        String,
        /// URI
        String,
        /// Name
        String,
        /// Value
        String,
        /// Set mode (overwrite, increment, decrement)
        StickerSetMode,
        Responder<()>,
    ),
    DeleteSticker(
        /// Type
        String,
        /// URI
        String,
        /// Name
        String,
        Responder<()>,
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
    GetSongAtQueueId(u32, Responder<Option<SongInfo>>),
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
pub struct Connection {
    receiver: Receiver<Task>,
    // high_receiver: Receiver<Task<'a>>,
    client: Option<Client<StreamWrapper>>,
    /// MPD inter-client channel for communication between Euphonica connections
    wake_channel: Channel,
    /// For sending idle subsystem notifications to the wrapper.
    idle_sender: Option<Sender<Subsystem>>,
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
    ) -> Self {
        Self {
            receiver,
            // high_receiver,
            client: None,
            wake_channel,
            idle_sender,
        }
    }

    pub fn connect(&mut self, password: Option<&str>) -> Result<Version> {
        self.disconnect()?;
        let settings = utils::settings_manager().child("client");

        // self.state.set_connection_state(ConnectionState::Connecting);
        let use_unix_socket = settings.boolean("mpd-use-unix-socket");
        let mut client = if use_unix_socket {
            let path = settings.string("mpd-unix-socket");
            let path = path.as_str();
            println!("Connecting to local socket {}", &path);
            if let Ok(resolved) = path.try_resolve() {
                mpd::Client::new(StreamWrapper::new_unix(
                    UnixStream::connect(resolved).map_err(|_| Error::SocketError)?,
                ))
                .map_err(Error::Mpd)?
            } else {
                mpd::Client::new(StreamWrapper::new_unix(
                    UnixStream::connect(path).map_err(|_| Error::SocketError)?,
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
                TcpStream::connect(addr).map_err(|_| Error::TcpError)?,
            ))
            .map_err(Error::Mpd)?
        };

        // If there is a password configured, use it to authenticate.
        if let Some(password) = password {
            client.login(password).map_err(Error::Mpd)?;
        }

        // Doubles as a litmus test to see if we are authenticated.
        client
            .subscribe(self.wake_channel.clone())
            .map_err(Error::Mpd)?;
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

    fn client_then<F, T>(&mut self, then: F, resp: Responder<T>)
    where
        F: FnOnce(&mut Client<StreamWrapper>) -> MpdResult<T>,
    {
        respond(
            self.client
                .as_mut()
                .ok_or(Error::NotConnected)
                .and_then(|client| then(client).map_err(Error::Mpd)),
            resp,
        )
    }

    pub fn run(&mut self) -> Result<()> {
        loop {
            let mut curr_task: Option<Task> = None;
            // let n_tasks = self.high_receiver.len() + bg_receiver.len();
            if !self.receiver.is_empty() {
                curr_task = Some(
                    self.receiver.recv_blocking().expect("Unable to read from high-priority queue")
                );
            }
            // else if !self.low_receiver.is_empty() {
            //     curr_task = Some(
            //         self.low_receiver
            //             .recv_blocking()
            //             .expect("Unable to read from low-priority queue"),
            //     );
            // }

            if let Some(task) = curr_task {
                match task {
                    Task::Connect(password, resp) => {
                        respond(self.connect(password.as_deref()), resp);
                    }
                    Task::Disconnect(stop, resp) => {
                        let res = self.disconnect();
                        let is_ok = res.is_ok();
                        respond(res, resp);
                        if is_ok && stop {
                            break;
                        }
                    }
                    Task::SendMessage(content, resp) => {
                        let ch = self.wake_channel.clone();
                        self.client_then(move |c| c.sendmessage(ch, &content), resp);
                    }
                    Task::GetVolume(resp) => self.client_then(|c| c.getvol(), resp),
                    Task::SetVolume(val, resp) => self.client_then(|c| c.volume(val), resp),
                    Task::GetOutputs(resp) => self.client_then(|c| c.outputs(), resp),
                    Task::SetOutput(id, state, resp) => {
                        self.client_then(|c| c.output(id, state), resp)
                    }
                    Task::GetSticker(typ, uri, name, resp) => {
                        self.client_then(|c| c.sticker(&typ, &uri, &name), resp)
                    }
                    Task::GetKnownStickers(typ, uri, resp) => self
                        .client_then(|c| c.stickers(&typ, &uri).map(Stickers::from_mpd_kv), resp),
                    Task::SetSticker(typ, uri, name, val, mode, resp) => self.client_then(
                        |c| match mode {
                            StickerSetMode::Inc => c.inc_sticker(&typ, &uri, &name, &val),
                            StickerSetMode::Set => c.set_sticker(&typ, &uri, &name, &val),
                            StickerSetMode::Dec => c.dec_sticker(&typ, &uri, &name, &val),
                        },
                        resp,
                    ),
                    Task::DeleteSticker(typ, uri, name, resp) => {
                        self.client_then(|c| c.delete_sticker(&typ, &uri, &name), resp)
                    }
                    Task::GetPlaylists(resp) => self.client_then(
                        |c| {
                            c.playlists().map(|playlists| {
                                playlists
                                    .into_iter()
                                    .map(INodeInfo::from)
                                    .collect::<Vec<INodeInfo>>()
                            })
                        },
                        resp,
                    ),
                    Task::LoadPlaylist(name, resp) => self.client_then(|c| c.load(&name, ..), resp),
                    Task::SaveQueueAsPlaylist(name, mode, resp) => {
                        self.client_then(|c| c.save(&name, Some(mode)), resp)
                    }
                    Task::RenamePlaylist(old, new, resp) => {
                        self.client_then(|c| c.pl_rename(&old, &new), resp)
                    }
                    Task::EditPlaylist(actions, resp) => {
                        self.client_then(|c| c.pl_edit(&actions), resp)
                    }
                    Task::DeletePlaylist(name, resp) => {
                        self.client_then(|c| c.pl_remove(&name), resp)
                    }
                    Task::GetStatus(resp) => self.client_then(|c| c.status(), resp),
                    Task::GetSongAtQueueId(id, resp) => self.client_then(
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
                    Task::SetPlaybackFlow(flow, resp) => self.client_then(
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
					Task::SetCrossfade(fade, resp) => self.client_then(|c| c.crossfade(fade), resp),
                    Task::SetReplayGain(mode, resp) => self.client_then(|c| c.replaygain(mode), resp),
                    Task::SetMixRampDb(db, resp) => self.client_then(|c| c.mixrampdb(db), resp),
                    Task::SetMixRampDelay(delay, resp) => self.client_then(|c| c.mixrampdelay(delay), resp),
                    Task::SetRandom(state, resp) => self.client_then(|c| c.random(state), resp),
                    Task::SetConsume(state, resp) => self.client_then(|c| c.consume(state), resp),
                    Task::Pause(state, resp) => self.client_then(|c| c.pause(state), resp),
                    Task::Stop(resp) => self.client_then(|c| c.stop(), resp),
                    Task::Prev(resp) => self.client_then(|c| c.prev(), resp),
                    Task::Next(resp) => self.client_then(|c| c.next(), resp),
                    Task::PlayAtId(id, resp) => self.client_then(|c| c.switch(id), resp),
                    Task::PlayAtPos(pos, resp) => self.client_then(|c| c.switch(pos), resp),
                    Task::SwapPos(p1, p2, resp) => self.client_then(|c| c.swap(p1, p2), resp),
                    Task::DeleteAtPos(p, resp) => self.client_then(|c| c.delete(p), resp),
                    Task::ClearQueue(resp) => self.client_then(|c| c.clear(), resp),
                    Task::Seek(pos, resp) => self.client_then(|c| c.rewind(pos), resp)
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
                                    println!("Client received message: {}", content);
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
