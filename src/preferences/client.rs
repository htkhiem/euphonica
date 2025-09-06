use duplicate::duplicate;
use ::glib::closure_local;
use std::{rc::Rc, str::FromStr};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{glib, CompositeTemplate};

use glib::clone;

use mpd::status::AudioFormat;

use crate::{
    client::{password::{get_mpd_password, set_mpd_password}, state::StickersSupportLevel, ClientState, ConnectionState, MpdWrapper},
    player::{FftStatus, Player},
    utils,
};

// Allows us to implicitly grant read access to files outside of the sandbox.
// The default FileDialog will simply copy the file to /run/..., which is
// not applicable for opening namedpipes.
use ashpd::desktop::file_chooser::SelectedFiles;

const FFT_SIZES: &'static [u32; 4] = &[512, 1024, 2048, 4096];

pub enum StatusIconState {
    Disabled,
    Loading,
    Partial,
    Full
}

impl StickersSupportLevel {
    // TODO: translatable
    pub fn get_ui_elements(&self) -> (StatusIconState, String, String) {
        match self {
            StickersSupportLevel::Disabled => (
                StatusIconState::Disabled,
                String::from("Stickers support: disabled"),
                String::from("Features such as song and album rating are unavailable. Enable stickers DB in your mpd.conf first.")
            ),
            StickersSupportLevel::SongsOnly => (
                StatusIconState::Partial,
                String::from("Stickers support: partial"),
                String::from("Album-level stickers are unavailable on MPD older than 0.24.")
            ),
            StickersSupportLevel::All => (
                StatusIconState::Full,
                String::from("Stickers support: full"),
                String::from("All stickers-based features are enabled.")
            )
        }
    }
}

fn set_status_icon(img: &gtk::Image, state: StatusIconState) {
    match state {
        StatusIconState::Disabled => {
            img.set_css_classes(&["error"]);
            img.set_icon_name(Some("disabled-feature-symbolic"));
        }
        StatusIconState::Loading => {
            img.set_css_classes(&["dim-label"]);
            img.set_icon_name(Some("content-loading-symbolic"));
        }
        StatusIconState::Partial => {
            img.set_css_classes(&["warning"]);
            img.set_icon_name(Some("enabled-feature-symbolic"));
        }
        StatusIconState::Full => {
            img.set_css_classes(&["success"]);
            img.set_icon_name(Some("enabled-feature-symbolic"));
        }
    }
}

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/preferences/client.ui")]
    pub struct ClientPreferences {
        // MPD
        #[template_child]
        pub mpd_use_unix_socket: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub mpd_unix_socket: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub mpd_host: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub mpd_port: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub mpd_password: TemplateChild<adw::PasswordEntryRow>,
        #[template_child]
        pub mpd_status: TemplateChild<adw::ExpanderRow>,
        #[template_child]
        pub mpd_status_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub playlists_status: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub playlists_status_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub stickers_status: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub stickers_status_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub reconnect: TemplateChild<adw::ButtonRow>,
        #[template_child]
        pub mpd_download_album_art: TemplateChild<adw::SwitchRow>,

        // Visualiser data source
        #[template_child]
        pub viz_source: TemplateChild<adw::ComboRow>,
        // PipeWire
        #[template_child]
        pub pipewire_devices: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub pipewire_restart_between_songs: TemplateChild<adw::SwitchRow>,
        // FIFO
        #[template_child]
        pub fifo_path: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub fifo_browse: TemplateChild<gtk::Button>,
        #[template_child]
        pub fifo_format: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub fft_fps: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub fft_n_samples: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub fft_n_bins: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub fifo_status: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub fft_reconnect: TemplateChild<gtk::Button>,
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

            self.mpd_use_unix_socket
                .bind_property(
                    "active",
                    &self.mpd_unix_socket.get(),
                    "visible"
                )
                .sync_create()
                .build();

            self.mpd_use_unix_socket
                .bind_property(
                    "active",
                    &self.mpd_host.get(),
                    "visible"
                )
                .invert_boolean()
                .sync_create()
                .build();

            self.mpd_use_unix_socket
                .bind_property(
                    "active",
                    &self.mpd_port.get(),
                    "visible"
                )
                .invert_boolean()
                .sync_create()
                .build();

            let viz_settings = utils::settings_manager().child("client");
            let fifo_path_row = self.fifo_path.get();
            viz_settings
                .bind("mpd-fifo-path", &fifo_path_row, "subtitle")
                .get_only()
                .build();
            viz_settings
                .bind("pipewire-restart-between-songs", &self.pipewire_restart_between_songs.get(), "active")
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
            let viz_source = self.viz_source.get();
            viz_settings
                .bind("mpd-visualizer-pcm-source", &viz_source, "selected")
                .mapping(|var, _| {
                    if let Some(typ) = var.get::<String>() {
                        match typ.as_str() {
                            "fifo" => Some(0u32.to_value()),
                            "pipewire" => Some(1u32.to_value()),
                            _ => unimplemented!()
                        }
                    }
                    else {
                         Option::<glib::Value>::None
                    }
                })
                .set_mapping(|val, _| {
                    println!("Setting backend idx...");
                    if let Ok(idx) = val.get::<u32>() {
                        match idx {
                            0 => Some("fifo".to_variant()),
                            1 => Some("pipewire".to_variant()),
                            _ => unimplemented!()
                        }
                    }
                    else {
                        Option::<glib::Variant>::None
                    }
                })
                .build();
            // Hide FIFO-specific rows when PipeWire is selected as data source
            duplicate!{
                [name; [fifo_path]; [fifo_format];]
                viz_source
                    .bind_property("selected", &self.name.get(), "visible")
                    .transform_to(|_, val: u32| Some(val == 0))
                    .sync_create()
                    .build();
            }
            // Hide PipeWire-specific rows when FIFO is selected as data source
            duplicate!{
                [name; [pipewire_devices]; [pipewire_restart_between_songs];]
                viz_source
                    .bind_property("selected", &self.name.get(), "visible")
                    .transform_to(|_, val: u32| Some(val == 1))
                    .sync_create()
                    .build();
            }
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
                self.imp().mpd_status.set_enable_expansion(false);
                set_status_icon(&self.imp().mpd_status_icon.get(), StatusIconState::Disabled);
                if !self.imp().mpd_port.has_css_class("error") {
                    self.imp().reconnect.set_sensitive(true);
                }
            }
            ConnectionState::Connecting => {
                self.imp().mpd_status.set_subtitle("Connecting...");
                self.imp().mpd_status.set_enable_expansion(false);
                set_status_icon(&self.imp().mpd_status_icon.get(), StatusIconState::Loading);
                self.imp().reconnect.set_sensitive(false);
            }
            ConnectionState::Unauthenticated | ConnectionState::PasswordNotAvailable => {
                self.imp().mpd_status.set_subtitle("Authentication failed");
                self.imp().mpd_status.set_enable_expansion(false);
                set_status_icon(&self.imp().mpd_status_icon.get(), StatusIconState::Disabled);
                if !self.imp().mpd_port.has_css_class("error") {
                    self.imp().reconnect.set_sensitive(true);
                }
            }
            ConnectionState::CredentialStoreError => {
                self.imp().mpd_status.set_subtitle("Credential store error");
                self.imp().mpd_status.set_enable_expansion(false);
                set_status_icon(&self.imp().mpd_status_icon.get(), StatusIconState::Disabled);
                if !self.imp().mpd_port.has_css_class("error") {
                    self.imp().reconnect.set_sensitive(true);
                }
            }
            ConnectionState::WrongPassword => {
                self.imp().mpd_status.set_subtitle("Incorrect password");
                self.imp().mpd_status.set_enable_expansion(false);
                set_status_icon(&self.imp().mpd_status_icon.get(), StatusIconState::Disabled);
                if !self.imp().mpd_port.has_css_class("error") {
                    self.imp().reconnect.set_sensitive(true);
                }
            }
            ConnectionState::ConnectionRefused => {
                self.imp().mpd_status.set_subtitle("Connection refused");
                self.imp().mpd_status.set_enable_expansion(false);
                set_status_icon(&self.imp().mpd_status_icon.get(), StatusIconState::Disabled);
                if !self.imp().mpd_port.has_css_class("error") {
                    self.imp().reconnect.set_sensitive(true);
                }
            }
            ConnectionState::SocketNotFound => {
                self.imp().mpd_status.set_subtitle("Socket not found");
                self.imp().mpd_status.set_enable_expansion(false);
                set_status_icon(&self.imp().mpd_status_icon.get(), StatusIconState::Disabled);
                if !self.imp().mpd_port.has_css_class("error") {
                    self.imp().reconnect.set_sensitive(true);
                }
            }
            ConnectionState::Connected => {
                self.imp().mpd_status.set_subtitle("Connected");
                self.imp().mpd_status.set_enable_expansion(true);
                set_status_icon(&self.imp().mpd_status_icon.get(), StatusIconState::Full);
                if !self.imp().mpd_port.has_css_class("error") {
                    self.imp().reconnect.set_sensitive(true);
                }
            }
        }
    }

    fn on_playlists_status_changed(&self, cs: &ClientState) {
        // TODO: translatable
        let row = self.imp().playlists_status.get();
        let icon = self.imp().playlists_status_icon.get();
        if cs.supports_playlists() {
            set_status_icon(&icon, StatusIconState::Full);
            row.set_title("Playlists support: enabled");
            row.set_subtitle("Playlist-related features are enabled.");
        }
        else {
            set_status_icon(&icon, StatusIconState::Disabled);
            row.set_title("Playlists support: disabled");
            row.set_subtitle("Enable playlists DB in your mpd.conf first.");
        }
    }

    fn on_stickers_status_changed(&self, cs: &ClientState) {
        let row = self.imp().stickers_status.get();
        let icon = self.imp().stickers_status_icon.get();

        let (icon_state, title, subtitle) = cs.get_stickers_support_level().get_ui_elements();
        set_status_icon(&icon, icon_state);
        row.set_title(&title);
        row.set_subtitle(&subtitle);
    }

    pub fn setup(&self, client: Rc<MpdWrapper>, player: &Player) {
        let imp = self.imp();
        let client_state = client.clone().get_client_state();
        // Populate with current gsettings values
        let settings = utils::settings_manager();

        // These should only be saved when the Apply button is clicked.
        // As such we won't bind the widgets directly to the settings.
        let conn_settings = settings.child("client");
        conn_settings.bind(
            "mpd-use-unix-socket",
            &imp.mpd_use_unix_socket.get(),
            "active"
        ).build();
        imp.mpd_host.set_text(&conn_settings.string("mpd-host"));
        imp.mpd_unix_socket.set_text(&conn_settings.string("mpd-unix-socket"));
        imp.mpd_port
            .set_text(&conn_settings.uint("mpd-port").to_string());
        let password_field = imp.mpd_password.get();
        glib::spawn_future_local(async move {
            match get_mpd_password().await {
                Ok(maybe_password) => {
                    // At startup the password entry is disabled with a tooltip stating that
                    // the credential store is not available.
                    password_field.set_sensitive(true);
                    password_field.set_tooltip_text(None);
                    if let Some(password) = maybe_password {
                        password_field.set_text(&password);
                    }
                }
                Err(e) => {
                    println!("{:?}", e);
                }
            }
        });


        // TODO: more input validation
        // Prevent entering anything other than digits into the port entry row
        // This is needed since using a spinbutton row for port entry feels a bit weird
        // Don't perform this check when we're connecting to a local socket.
        imp.mpd_port.connect_changed(clone!(
            #[weak(rename_to = this)]
            self,
            move |entry| {
                if !this.imp().mpd_use_unix_socket.is_active() {
                    if entry.text().parse::<u32>().is_err() {
                        if !entry.has_css_class("error") {
                            entry.add_css_class("error");
                            this.imp().reconnect.set_sensitive(false);
                        }
                    } else if entry.has_css_class("error") {
                        entry.remove_css_class("error");
                        this.imp().reconnect.set_sensitive(true);
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

        self.on_playlists_status_changed(&client_state);
        client_state.connect_notify_local(
            Some("supports-playlists"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |cs, _| {
                    this.on_playlists_status_changed(&cs);
                }
            ),
        );

        self.on_stickers_status_changed(&client_state);
        client_state.connect_notify_local(
            Some("stickers-support-level"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |cs, _| {
                    this.on_stickers_status_changed(&cs);
                }
            ),
        );

        imp.reconnect.connect_activated(clone!(
            #[weak(rename_to = this)]
            self,
            #[strong]
            conn_settings,
            #[weak]
            client,
            move |_| {
                if this.imp().mpd_use_unix_socket.is_active() {
                    let _ = conn_settings.set_string("mpd-unix-socket", &this.imp().mpd_unix_socket.text());
                }
                else {
                    let _ = conn_settings.set_string("mpd-host", &this.imp().mpd_host.text());
                    let _ = conn_settings.set_uint(
                        "mpd-port",
                        this.imp().mpd_port.text().parse::<u32>().unwrap(),
                    );
                }

                let password_val = this.imp().mpd_password.text();
                glib::spawn_future_local(clone!(
                    #[weak]
                    client,
                    async move {
                        let password: Option<&str> = if password_val.is_empty() { None } else { Some(password_val.as_str()) };
                        match set_mpd_password(password).await {
                            Ok(()) => {
                                client.connect_async().await;
                            }
                            Err(msg) => {println!("{}", msg);}
                        }
                    }
                ));
            }
        ));
        let mpd_download_album_art = imp.mpd_download_album_art.get();
        conn_settings
            .bind("mpd-download-album-art", &mpd_download_album_art, "active")
            .build();

        // Visualiser
        player
            .bind_property(
                "fft-status",
                &self.imp().fifo_status.get(),
                "subtitle"
            )
            .transform_to(|_, status: FftStatus| Some(status.get_description()))
            .sync_create()
            .build();

        // Get PipeWire devices, if the PipeWire backend is running
        self.update_pipewire_devices(
            player
                .get_fft_param(Some("pipewire"), "devices")
                .and_then(|variant| variant.get::<Vec<String>>())
        );
        self.update_pipewire_current_device(
            player
                .get_fft_param(Some("pipewire"), "current-device")
                .and_then(|variant| variant.get::<i32>())
        );

        player.connect_closure(
            "fft-param-changed",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: Player, name: String, key: String, new_val: glib::Variant| {
                    // Currently only need to handle PipeWire
                    if name == "pipewire" {
                        match key.as_str() {
                            "devices" => {this.update_pipewire_devices(new_val.get::<Vec<String>>());}
                            "current-device" => {this.update_pipewire_current_device(new_val.get::<i32>());}
                            _ => {}
                        }
                    }
                }
            )
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
                        this.imp().fft_reconnect.set_sensitive(false);
                    }
                } else if entry.has_css_class("error") {
                    entry.remove_css_class("error");
                    this.imp().fft_reconnect.set_sensitive(true);
                }
            }
        ));

        imp.fft_fps
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
        imp.fft_reconnect.connect_clicked(clone!(
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
                let pw_dev_idx = imp.pipewire_devices.selected();
                if pw_dev_idx != gtk::INVALID_LIST_POSITION {
                    player.set_fft_param(Some("pipewire"), "current-device", (pw_dev_idx as i32 - 1).to_variant());
                }
                conn_settings
                    .set_string("mpd-fifo-format", &imp.fifo_format.text())
                    .expect("Cannot save FIFO settings");
                player_settings
                    .set_uint("visualizer-fps", imp.fft_fps.value().round() as u32)
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
                player.restart_fft_thread();
            }
        ));
    }

    fn update_pipewire_devices(&self, maybe_devices: Option<Vec<String>>) {
        self.imp().pipewire_devices.set_model(
            maybe_devices.and_then(|devices: Vec<String>| {
                let mut device_list = vec!["(auto)"];
                device_list.append(&mut devices.iter().map(String::as_ref).collect::<Vec<&str>>());
                Some(gtk::StringList::new(&device_list))
            }).as_ref()
        );
    }

    fn update_pipewire_current_device(&self, curr_device: Option<i32>) {
        // Position -1 means auto.
        if let Some(curr_device) = curr_device {
            self.imp().pipewire_devices.set_selected((curr_device + 1) as u32);
        }
    }
}
