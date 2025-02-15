use keyring::{Entry, Error as KeyringError};
use std::{rc::Rc, str::FromStr};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{glib, CompositeTemplate};

use glib::clone;

use mpd::status::AudioFormat;

use crate::{
    client::{ClientState, ConnectionState, MpdWrapper},
    player::{FftStatus, Player},
    utils,
};

// Allows us to implicitly grant read access to files outside of the sandbox.
// The default FileDialog will simply copy the file to /run/..., which is
// not applicable for opening namedpipes.
use ashpd::desktop::file_chooser::{Choice, FileFilter, SelectedFiles};

const FFT_SIZES: &'static [u32; 4] = &[512, 1024, 2048, 4096];

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/preferences/client.ui")]
    pub struct ClientPreferences {
        // MPD
        #[template_child]
        pub mpd_host: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub mpd_port: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub mpd_password: TemplateChild<adw::PasswordEntryRow>,
        #[template_child]
        pub mpd_status: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub reconnect: TemplateChild<gtk::Button>,
        #[template_child]
        pub mpd_download_album_art: TemplateChild<adw::SwitchRow>,

        // FIFO output
        #[template_child]
        pub fifo_path: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub fifo_browse: TemplateChild<gtk::Button>,
        #[template_child]
        pub fifo_format: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub fifo_fps: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub fft_n_samples: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub fft_n_bins: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub fifo_status: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub fifo_reconnect: TemplateChild<gtk::Button>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ClientPreferences {
        const NAME: &'static str = "EuphonicaClientPreferences";
        type Type = super::ClientPreferences;
        type ParentType = adw::PreferencesPage;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ClientPreferences {
        fn constructed(&self) {
            self.parent_constructed();

            let fifo_settings = utils::settings_manager().child("client");
            let fifo_path_row = self.fifo_path.get();
            fifo_settings
                .bind("mpd-fifo-path", &fifo_path_row, "subtitle")
                .get_only()
                .build();
            self.fifo_browse.connect_clicked(|_| {
                utils::tokio_runtime().spawn(async move {
                    let maybe_files = SelectedFiles::open_file()
                        .title("Select the FIFO output file")
                        .modal(true)
                        .multiple(false)
                        .send()
                        .await
                        .expect("ashpd file open await failure")
                        .response();

                    if let Ok(files) = maybe_files {
                        let fifo_settings = utils::settings_manager().child("client");
                        let uris = files.uris();
                        if uris.len() > 0 {
                            fifo_settings
                                .set_string(
                                    "mpd-fifo-path",
                                    uris[0].as_str(),
                                )
                                .expect("Unable to save FIFO path");
                        }
                    }
                    else {
                        println!("{:?}", maybe_files);
                    }
                });
            });
        }
    }
    impl WidgetImpl for ClientPreferences {}
    impl PreferencesPageImpl for ClientPreferences {}
}

glib::wrapper! {
    pub struct ClientPreferences(ObjectSubclass<imp::ClientPreferences>)
        @extends adw::PreferencesPage,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Widget;
}

impl Default for ClientPreferences {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl ClientPreferences {
    fn on_connection_state_changed(&self, cs: &ClientState) {
        match cs.get_connection_state() {
            ConnectionState::NotConnected => {
                self.imp().mpd_status.set_subtitle("Failed to connect");
                if !self.imp().mpd_port.has_css_class("error") {
                    self.imp().reconnect.set_sensitive(true);
                }
            }
            ConnectionState::Connecting => {
                self.imp().mpd_status.set_subtitle("Connecting...");
                self.imp().reconnect.set_sensitive(false);
            }
            ConnectionState::Unauthenticated => {
                self.imp().mpd_status.set_subtitle("Authentication failed");
                if !self.imp().mpd_port.has_css_class("error") {
                    self.imp().reconnect.set_sensitive(true);
                }
            }
            ConnectionState::CredentialStoreError => {
                self.imp().mpd_status.set_subtitle("Credential store error");
                if !self.imp().mpd_port.has_css_class("error") {
                    self.imp().reconnect.set_sensitive(true);
                }
            }
            ConnectionState::WrongPassword => {
                self.imp().mpd_status.set_subtitle("Incorrect password");
                if !self.imp().mpd_port.has_css_class("error") {
                    self.imp().reconnect.set_sensitive(true);
                }
            }
            ConnectionState::Connected => {
                self.imp().mpd_status.set_subtitle("Connected");
                if !self.imp().mpd_port.has_css_class("error") {
                    self.imp().reconnect.set_sensitive(true);
                }
            }
        }
    }

    fn on_fifo_changed(&self, state: FftStatus) {
        self.imp().fifo_status.set_subtitle(state.get_description());
    }

    pub fn setup(&self, client: Rc<MpdWrapper>, player: &Player) {
        let imp = self.imp();
        let client_state = client.clone().get_client_state();
        // Populate with current gsettings values
        let settings = utils::settings_manager();

        // These should only be saved when the Apply button is clicked.
        // As such we won't bind the widgets directly to the settings.
        let conn_settings = settings.child("client");
        imp.mpd_host.set_text(&conn_settings.string("mpd-host"));
        imp.mpd_port
            .set_text(&conn_settings.uint("mpd-port").to_string());
        let maybe_keyring_entry = Entry::new("euphonica", "mpd-password");
        if let Ok(ref keyring_entry) = maybe_keyring_entry {
            // At startup the password entry is disabled with a tooltip stating that
            // the credential store is not available.
            imp.mpd_password.set_sensitive(true);
            imp.mpd_password.set_tooltip_text(None);
            match keyring_entry.get_password() {
                Ok(password) => {
                    imp.mpd_password.set_text(&password);
                }
                _ => {}
            }
        }

        // TODO: more input validation
        // Prevent entering anything other than digits into the port entry row
        // This is needed since using a spinbutton row for port entry feels a bit weird
        imp.mpd_port.connect_changed(clone!(
            #[weak(rename_to = this)]
            self,
            move |entry| {
                if entry.text().parse::<u32>().is_err() {
                    if !entry.has_css_class("error") {
                        entry.add_css_class("error");
                        this.imp().reconnect.set_sensitive(false);
                    }
                } else if entry.has_css_class("error") {
                    entry.remove_css_class("error");
                    this.imp().reconnect.set_sensitive(true);
                }
            }
        ));

        // Display connection status
        self.on_connection_state_changed(&client_state);
        client_state.connect_notify_local(
            Some("connection-state"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |cs, _| {
                    this.on_connection_state_changed(cs);
                }
            ),
        );

        imp.reconnect.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            #[strong]
            conn_settings,
            #[weak]
            client,
            move |_| {
                let _ = conn_settings.set_string("mpd-host", &this.imp().mpd_host.text());
                let _ = conn_settings.set_uint(
                    "mpd-port",
                    this.imp().mpd_port.text().parse::<u32>().unwrap(),
                );

                if let Ok(ref keyring_entry) = maybe_keyring_entry {
                    let password = this.imp().mpd_password.text();
                    if password.is_empty() {
                        if let Err(KeyringError::NoEntry) = keyring_entry.delete_credential() {
                        } else {
                            panic!("Unable to clear MPD password from keyring");
                        }
                    } else {
                        keyring_entry
                            .set_password(password.as_str())
                            .expect("Unable to save MPD password to keyring");
                    }
                }
                let _ = client.queue_connect();
            }
        ));
        let mpd_download_album_art = imp.mpd_download_album_art.get();
        conn_settings
            .bind("mpd-download-album-art", &mpd_download_album_art, "active")
            .build();

        // FIFO
        self.on_fifo_changed(player.fft_status());
        player.connect_notify_local(
            Some("fft-status"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |player, _| {
                    this.on_fifo_changed(player.fft_status());
                }
            ),
        );
        let player_settings = settings.child("player");
        imp.fifo_format
            .set_text(&conn_settings.string("mpd-fifo-format"));

        // TODO: more input validation
        // Only accept valid MPD format strings
        imp.fifo_format.connect_changed(clone!(
            #[strong(rename_to = this)]
            self,
            move |entry| {
                if let Err(_) = AudioFormat::from_str(entry.text().as_str()) {
                    if !entry.has_css_class("error") {
                        entry.add_css_class("error");
                        this.imp().fifo_reconnect.set_sensitive(false);
                    }
                } else if entry.has_css_class("error") {
                    entry.remove_css_class("error");
                    this.imp().fifo_reconnect.set_sensitive(true);
                }
            }
        ));

        imp.fifo_fps
            .set_value(player_settings.uint("visualizer-fps") as f64);
        // 512 1024 2048 4096
        imp.fft_n_samples
            .set_selected(match &player_settings.uint("visualizer-fft-samples") {
                512 => 0,
                1024 => 1,
                2048 => 2,
                4096 => 3,
                _ => unreachable!(),
            });
        imp.fft_n_bins
            .set_value(player_settings.uint("visualizer-spectrum-bins") as f64);
        imp.fifo_reconnect.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            #[strong]
            conn_settings,
            #[strong]
            player_settings,
            #[weak]
            player,
            move |_| {
                println!("Restarting FFT thread...");
                let imp = this.imp();
                conn_settings
                    .set_string("mpd-fifo-format", &imp.fifo_format.text())
                    .expect("Cannot save FIFO settings");
                player_settings
                    .set_uint("visualizer-fps", imp.fifo_fps.value().round() as u32)
                    .expect("Cannot save visualizer settings");
                player_settings
                    .set_uint(
                        "visualizer-fft-samples",
                        FFT_SIZES[imp.fft_n_samples.selected() as usize],
                    )
                    .expect("Cannot save FFT settings");
                player_settings
                    .set_uint(
                        "visualizer-spectrum-bins",
                        imp.fft_n_bins.value().round() as u32,
                    )
                    .expect("Cannot save visualizer settings");
                player.reconnect_fifo();
            }
        ));
    }
}
