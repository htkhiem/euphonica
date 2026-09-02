use gio::{Subprocess, SubprocessFlags};
use glib::subclass::prelude::*;
use gtk::{gio::{self, prelude::*, Cancellable}, glib::{self, clone}};
use std::{
    cell::RefCell,
    ffi::OsStr,
    fs::File,
    io::{Read, Write},
    process::{Child, Command, Stdio},
    rc::Rc, result,
};

use crate::{
    server::config::{MpdConfig, OutputConfig, get_minimal_config},
    utils::get_config_file_path,
};

#[derive(Debug)]
pub enum Error {
    NotConfigured,
    Config,
    Subprocess,
}

pub type Result<T> = result::Result<T, Error>;

/// Wrapper for managing our own server instance.
mod imp {
    use std::sync::OnceLock;

    use gtk::{gio::Cancellable, glib::subclass::Signal};

    use super::*;

    #[derive(Debug, Default)]
    pub struct ManagedMpdServer {
        pub handle: RefCell<Option<(Subprocess, gio::Cancellable)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ManagedMpdServer {
        const NAME: &'static str = "EuphonicaManagedMpdServer";
        type Type = super::ManagedMpdServer;

        fn new() -> Self {
            Self::default()
        }
    }

    impl ObjectImpl for ManagedMpdServer {
        fn dispose(&self) {
            if let Err(e) = self.obj().stop() {
                dbg!(e);
            }
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![Signal::builder("server-stopped").build()])
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
    /// No-op if not already running.
    pub fn stop(&self) -> Result<()> {
        if let Some((subprocess, cancellable)) = self.imp().handle.take() {
            cancellable.cancel();  // intentionally shutting subprocess down so silence this first.
            subprocess.force_exit();
            subprocess.wait(Cancellable::NONE).map_err(|_| Error::Subprocess)?;
        }
        Ok(())
    }

    /// Will call stop() by itself first.
    pub fn start(&self) -> Result<()> {
        self.stop()?;
        // Ensure there's a valid config file.
        // TODO: maybe just skip the validation and start MPD directly & pick up crashes afterwards as config errors.
        let config_path = get_config_file_path();
        let mut file = File::open(&config_path).map_err(|_| Error::NotConfigured)?;
        let mut txt = String::new();
        file.read_to_string(&mut txt).map_err(|_| Error::Config)?;
        let _ = MpdConfig::try_from(txt.as_str()).map_err(|_| Error::Config)?;

        // let mut output = File::create(&config_path).map_err(|_| Error::Config)?;
        // if !std::fs::exists(&config_path).map_err(|_| Error::Config)? {
        //     write!(output, "{}", default_config.to_string()).unwrap();
        // }
        let subprocess = Subprocess::newv(
            &[
                &OsStr::new("mpd"),
                &OsStr::new("--no-daemon"),
                config_path.as_os_str()
            ], // Command and args
            SubprocessFlags::NONE,
        )
        .expect("Failed to create subprocess");

        // Guard against unexpected subprocess exits.
        let guard_cancellable = gio::Cancellable::new();
        subprocess.wait_async(
            Some(&guard_cancellable),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.emit_by_name::<()>("server-stopped", &[]);
                }
            )
        );


        let _ = self.imp().handle.replace(Some((subprocess, guard_cancellable)));

        Ok(())
    }
}
