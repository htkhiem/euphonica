use crate::{
    common::ConnectionState,
    server::config::MpdConfig,
    utils::get_standalone_config_path,
};
use asyncified::Asyncified;
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
    io::Read,
    os::unix::net::UnixStream,
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

// Ten seconds of rapid fire tests so we don't add too much delay.
// FIXME: this is disgusting, maybe find something in stdout to listen to instead.
static MAX_STARTUP_POLLS: u32 = 50;
static POLL_INTERVAL_MS: u64 = 200;

/// Wrapper for managing our own server instance.
mod imp {
    use std::cell::Cell;

    use gtk::glib::Properties;

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

fn self_test(socket_path: &str) -> Result<()> {
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

impl ManagedMpdServer {
    fn set_status(&self, status: ConnectionState) {
        let old = self.imp().status.replace(status);
        if old != status {
            self.notify("status");
        }
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
    pub async fn start(&self) -> Result<()> {
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

        // Only return after having successfully verified online status
        let path = cfg.bind_to_address.unwrap();
        let asyncified = Asyncified::builder().build_ok(move || {()}).await;
        let success = asyncified.call(move |_| {
            for _i in 0..MAX_STARTUP_POLLS {
                match self_test(&path) {
                    Ok(()) => {
                        return true;
                    }
                    Err(_) => {
                        std::thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
                    }
                }
            }
            return false;
        }).await;

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
