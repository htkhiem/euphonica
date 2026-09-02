use gio::{Subprocess, SubprocessFlags};
use glib::subclass::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::RefCell,
    ffi::OsStr,
    fs::File,
    io::Write,
    process::{Child, Command, Stdio},
    rc::Rc, result,
};

use crate::{
    server::config::{MpdConfig, OutputConfig, get_minimal_config},
    utils::get_config_file_path,
};

#[derive(Debug)]
pub enum Error {
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
        handle: RefCell<Option<Subprocess>>,
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
            if let Some(child) = self.handle.take() {
                child.force_exit();
                child.wait(Cancellable::NONE);
                eprintln!("Managed MPD process has exited");
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
    pub fn start(&self) -> Result<()> {
        // Ensure there's a config file
        let config_path = get_config_file_path();
        if !std::fs::exists(&config_path).map_err(|_| Error::Config)? {
            let mut output = File::create(&config_path).map_err(|_| Error::Config)?;
            let default_config = get_minimal_config();
            write!(output, "{}", default_config.to_string());
        }
        let subprocess = Subprocess::newv(
            &[
                &OsStr::new("mpd"),
                &OsStr::new("--no-daemon"),
                &OsStr::new(config_path.to_str().unwrap_or("")),
            ], // Command and args
            SubprocessFlags::NONE,
        )
        .expect("Failed to create subprocess");
        Ok(())
    }
}
