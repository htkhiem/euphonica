use crate::{
    client::{Error as ClientError, StreamWrapper},
    common::ConnectionState,
    server::config::{MpdConfig, OutputConfig},
    utils::{get_config_basepath, get_standalone_config_path},
};
use gio::{Subprocess, SubprocessFlags};
use glib::subclass::prelude::*;
use gtk::{
    gio::{self, Cancellable, prelude::*},
    glib::{self, clone},
};
use mpd::Client;
use resolve_path::PathResolveExt;
use std::{
    cell::RefCell,
    ffi::OsStr,
    fs::File,
    io::{Read, Write},
    os::unix::net::UnixStream,
    process::{Child, Command, Stdio},
    rc::Rc,
    result,
    time::Duration,
};

#[derive(Debug)]
pub enum Error {
    NotConfigured,
    Config,
    Subprocess,
    Client,
}

pub type Result<T> = result::Result<T, Error>;

static MAX_STARTUP_POLLS: u32 = 5;
static POLL_INTERVAL_MS: u64 = 200;

/// Wrapper for managing our own server instance.
mod imp {
    use std::{cell::Cell, sync::OnceLock};

    use gtk::{
        gio::Cancellable,
        glib::{ParamSpec, ParamSpecEnum, Properties, subclass::Signal},
    };

    use super::*;

    #[derive(Debug, Default, Properties)]
    #[properties(wrapper_type = super::ManagedMpdServer)]
    pub struct ManagedMpdServer {
        pub handle: RefCell<Option<(Subprocess, gio::Cancellable)>>,
        #[property(get, builder(ConnectionState::default()))]
        pub status: Cell<ConnectionState>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ManagedMpdServer {
        const NAME: &'static str = "EuphonicaManagedMpdServer";
        type Type = super::ManagedMpdServer;

        fn new() -> Self {
            Self::default()
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for ManagedMpdServer {
        fn dispose(&self) {
            if let Err(e) = self.obj().stop() {
                dbg!(e);
            }
        }
    }
}

glib::wrapper! {
    pub struct ManagedMpdServer(ObjectSubclass<imp::ManagedMpdServer>);
}

impl Default for ManagedMpdServer {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl ManagedMpdServer {
    fn set_status(&self, status: ConnectionState) {
        let old = self.imp().status.replace(status);
        if old != status {
            self.notify("status");
        }
    }

    fn self_test(&self, socket_path: &str) -> Result<()> {
        let _: Client<UnixStream> = if let Ok(resolved) = socket_path.try_resolve() {
            UnixStream::connect(resolved)
                .map_err(|_| Error::Client)
                .and_then(|s| mpd::Client::new(s).map_err(|_| Error::Client))?
        } else {
            UnixStream::connect(socket_path)
                .map_err(|_| Error::Client)
                .and_then(|s| mpd::Client::new(s).map_err(|_| Error::Client))?
        };
        Ok(())
    }

    /// No-op if not already running.
    pub fn stop(&self) -> Result<()> {
        if let Some((subprocess, cancellable)) = self.imp().handle.take() {
            cancellable.cancel(); // intentionally shutting subprocess down so silence this first.
            subprocess.force_exit();
            subprocess
                .wait(Cancellable::NONE)
                .map_err(|_| Error::Subprocess)?;
        }
        self.set_status(ConnectionState::NotConnected);
        Ok(())
    }

    /// Will call stop() by itself first.
    pub fn start(&self) -> Result<()> {
        self.stop()?;
        // Ensure there's a valid config file.
        // TODO: maybe just skip the validation and start MPD directly & pick up crashes afterwards as config errors.
        let config_path = get_standalone_config_path();
        let mut file = File::open(&config_path).map_err(|_| Error::NotConfigured)?;
        let mut txt = String::new();
        file.read_to_string(&mut txt).map_err(|_| Error::Config)?;
        let cfg = MpdConfig::try_from(txt.as_str()).map_err(|_| Error::Config)?;

        if cfg.bind_to_address.is_none() {
            return Err(Error::Config);
        }

        let subprocess = Subprocess::newv(
            &[
                &OsStr::new("mpd"),
                &OsStr::new("--no-daemon"),
                config_path.as_os_str(),
            ], // Command and args
            SubprocessFlags::NONE,
        )
        .expect("Failed to create subprocess");

        // Block until ready by using a really simple ping thread.
        let path = cfg.bind_to_address.unwrap();
        let mut success = false;
        for i in 0..MAX_STARTUP_POLLS {
            match self.self_test(&path) {
                Ok(()) => {
                    success = true;
                    break;
                }
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
                }
            }
        }
        if !success {
            eprintln!(
                "FATAL: failed to verify MPD readiness after {} tries",
                MAX_STARTUP_POLLS
            );
            return Err(Error::Subprocess);
        }

        self.set_status(ConnectionState::Connected);

        // Guard against unexpected subprocess exits.
        let guard_cancellable = gio::Cancellable::new();
        subprocess.wait_async(
            Some(&guard_cancellable),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.set_status(ConnectionState::NotConnected);
                }
            ),
        );

        let _ = self
            .imp()
            .handle
            .replace(Some((subprocess, guard_cancellable)));

        Ok(())
    }
}
