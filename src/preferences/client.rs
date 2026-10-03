use std::{
    cell::{Cell, RefCell},
    fs::File,
    io::{Read, Write},
    str::FromStr,
};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{
    CompositeTemplate,
    glib::{self, WeakRef, closure_local},
};

use glib::clone;

use mpd::status::AudioFormat;

use crate::{
    application::EuphonicaApplication,
    client::{
        ClientState,
        password::{get_mpd_password_async, set_mpd_password},
        state::StickersSupportLevel,
    },
    common::ConnectionState,
    player::{FftStatus, Player},
    preferences::Preferences,
    server::{Resampler, SoxrPreset, config::MpdConfig},
    utils::{self, get_standalone_config_path, settings_manager},
};
use crate::{preferences::outputs::AudioOutputs, server::config::OutputConfig};

// Allows us to implicitly grant read access to files outside of the sandbox.
// The default FileDialog will simply copy the file to /run/..., which is
// not applicable for opening namedpipes.
use ashpd::desktop::file_chooser::SelectedFiles;

const FFT_SIZES: &[u32; 4] = &[512, 1024, 2048, 4096];

pub enum StatusIconState {
    Disabled,
    Loading,
    Partial,
    Full,
}

impl StickersSupportLevel {
    // TODO: translatable
    pub fn get_ui_elements(&self) -> (StatusIconState, String, String) {
        match self {
            StickersSupportLevel::Disabled => (
                StatusIconState::Disabled,
                String::from("Stickers support: disabled"),
                String::from(
                    "Features such as song and album rating are unavailable. Enable stickers DB in your mpd.conf first.",
                ),
            ),
            StickersSupportLevel::SongsOnly => (
                StatusIconState::Partial,
                String::from("Stickers support: partial"),
                String::from("Album-level stickers are unavailable on MPD older than 0.24."),
            ),
            StickersSupportLevel::All => (
                StatusIconState::Full,
                String::from("Stickers support: full"),
                String::from("All stickers-based features are enabled."),
            ),
        }
    }
}

pub fn set_status_icon(img: &gtk::Image, state: StatusIconState) {
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

/// Toggle the "error" CSS class on a row. Both add/remove are idempotent,
/// so no prior-state check is needed.
fn set_row_error(row: &impl IsA<gtk::Widget>, has_error: bool) {
    if has_error && !row.has_css_class("error") {
        row.add_css_class("error");
    } else if row.has_css_class("error") {
        row.remove_css_class("error");
    }
}

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/preferences/client.ui")]
    pub struct ClientPreferences {
        // Standalone mode
        #[template_child]
        pub mpd_use_own_server: TemplateChild<adw::ExpanderRow>,
        #[template_child]
        pub mpd_override_exec_path: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub mpd_exec_path: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub mpd_library_path: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub mpd_library_browse: TemplateChild<gtk::Button>,
        #[template_child]
        pub config_outputs_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub config_resampler_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub standalone_status: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub standalone_status_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub apply_standalone_config: TemplateChild<adw::ButtonRow>,

        // External MPD
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
        #[template_child]
        pub mpd_backup_meta_as_stickers: TemplateChild<adw::SwitchRow>,

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

        #[template_child]
        pub outputs_subpage: TemplateChild<adw::NavigationPage>,
        #[template_child]
        pub outputs_box: TemplateChild<AudioOutputs>,
        #[template_child]
        pub add_output: TemplateChild<gtk::Button>,

        #[template_child]
        pub resampler_subpage: TemplateChild<adw::NavigationPage>,
        #[template_child]
        pub force_resampler: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub resampler_plugin: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub libsamplerate_config: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub libsamplerate_type: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub soxr_config: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub soxr_quality: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub soxr_threads: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub soxr_custom_precision: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub soxr_custom_phase_response: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub soxr_custom_passband_end: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub soxr_custom_stopband_begin: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub soxr_custom_attenuation: TemplateChild<adw::SpinRow>,

        // Validation flags
        pub has_library_path: Cell<bool>,
        pub valid_exec_path: Cell<bool>,
        pub valid_host: Cell<bool>,
        pub valid_port: Cell<bool>,
        pub valid_unix_socket: Cell<bool>,
        pub valid_fifo_format: Cell<bool>,

        pub standalone_cfg: RefCell<MpdConfig>,
        pub dialog: WeakRef<Preferences>,
        pub client_state: WeakRef<ClientState>,
        pub standalone_server: WeakRef<crate::server::controller::ManagedMpdServer>,
        pub player: WeakRef<Player>,

        pub viz_mode_setting_id: RefCell<Option<glib::SignalHandlerId>>,
        pub viz_source_setting_id: RefCell<Option<glib::SignalHandlerId>>,
        pub server_status_id: RefCell<Option<glib::SignalHandlerId>>,
        pub player_fft_param_id: RefCell<Option<glib::SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ClientPreferences {
        const NAME: &'static str = "EuphonicaClientPreferences";
        type Type = super::ClientPreferences;
        type ParentType = adw::PreferencesPage;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ClientPreferences {
        fn constructed(&self) {
            self.parent_constructed();
            let client_settings = settings_manager().child("client");
            client_settings
                .bind(
                    "mpd-use-own-server",
                    &self.mpd_use_own_server.get(),
                    "enable-expansion",
                )
                .build();
            client_settings
                .bind(
                    "mpd-override-exec-path",
                    &self.mpd_override_exec_path.get(),
                    "active",
                )
                .build();
            client_settings
                .bind("mpd-exec-path", &self.mpd_exec_path.get(), "text")
                .build();
            client_settings
                .bind(
                    "mpd-download-album-art",
                    &self.mpd_download_album_art.get(),
                    "active",
                )
                .build();
            client_settings
                .bind(
                    "mpd-backup-metadata",
                    &self.mpd_backup_meta_as_stickers.get(),
                    "active",
                )
                .build();
            self.mpd_host.connect_changed(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.on_hostname_changed();
                }
            ));
            self.mpd_port.connect_changed(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.on_port_changed();
                }
            ));
            self.mpd_unix_socket.connect_changed(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.on_unix_socket_changed();
                }
            ));

            // Standalone mode locks the visualiser data source to the hidden
            // internal FIFO; in client mode the rows follow the selection.
            self.update_visualizer_config_visibility();
            let _ = self
                .viz_mode_setting_id
                .replace(Some(client_settings.connect_changed(
                    Some("mpd-use-own-server"),
                    {
                        clone!(
                            #[weak(rename_to = this)]
                            self,
                            move |_, _| this.update_visualizer_config_visibility()
                        )
                    },
                )));
            let _ = self
                .viz_source_setting_id
                .replace(Some(client_settings.connect_changed(
                    Some("mpd-visualizer-pcm-source"),
                    {
                        clone!(
                            #[weak(rename_to = this)]
                            self,
                            move |_, _| this.update_visualizer_config_visibility()
                        )
                    },
                )));

            let viz_settings = settings_manager().child("client");
            let fifo_path_row = self.fifo_path.get();
            viz_settings
                .bind("mpd-fifo-path", &fifo_path_row, "subtitle")
                .get_only()
                .build();
            viz_settings
                .bind(
                    "pipewire-restart-between-songs",
                    &self.pipewire_restart_between_songs.get(),
                    "active",
                )
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
                        let fifo_settings = settings_manager().child("client");
                        let uris = files.uris();
                        if !uris.is_empty() {
                            fifo_settings
                                .set_string("mpd-fifo-path", uris[0].as_str())
                                .expect("Unable to save FIFO path");
                        }
                    }
                });
            });
            self.fifo_format.connect_changed(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.on_fifo_format_changed();
                }
            ));
            let viz_source = self.viz_source.get();
            viz_settings
                .bind("mpd-visualizer-pcm-source", &viz_source, "selected")
                .mapping(|var, _| {
                    if let Some(typ) = var.get::<String>() {
                        match typ.as_str() {
                            "fifo" => Some(0u32.to_value()),
                            "pipewire" => Some(1u32.to_value()),
                            _ => unimplemented!(),
                        }
                    } else {
                        Option::<glib::Value>::None
                    }
                })
                .set_mapping(|val, _| {
                    if let Ok(idx) = val.get::<u32>() {
                        match idx {
                            0 => Some("fifo".to_variant()),
                            1 => Some("pipewire".to_variant()),
                            _ => unimplemented!(),
                        }
                    } else {
                        Option::<glib::Variant>::None
                    }
                })
                .build();

            // Library path browse
            self.mpd_library_browse.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    let (sender, receiver) = oneshot::channel();
                    utils::tokio_runtime().spawn(async move {
                        sender
                            .send(
                                SelectedFiles::open_file()
                                    .title("Select folder containing your music")
                                    .directory(true)
                                    .modal(true)
                                    .multiple(false)
                                    .send()
                                    .await
                                    .expect("ashpd folder open await failure")
                                    .response(),
                            )
                            .expect("Broken oneshot sender");
                    });

                    glib::spawn_future_local(clone!(
                        #[weak]
                        this,
                        async move {
                            if let Ok(folders) = receiver.await.expect("Broken oneshot receiver") {
                                let uris = folders.uris();
                                if !uris.is_empty() {
                                    let uri = uris[0].as_str();
                                    if let Ok(uri) =
                                        urlencoding::decode(if uri.starts_with("file://") {
                                            &uri[7..]
                                        } else {
                                            uri
                                        })
                                        .map(String::from)
                                    {
                                        this.set_music_library_path(Some(&uri));
                                        let mut cfg = this.standalone_cfg.borrow_mut();
                                        cfg.music_directory = uri;
                                    }
                                }
                            }
                        }
                    ));
                }
            ));

            // Outputs
            let outputs_box = self.outputs_box.get();
            outputs_box
                .bind_property("n-outputs", &self.config_outputs_row.get(), "subtitle")
                .transform_to(|_, val: u32| {
                    // TODO: translatable
                    Some(format!("{} output(s)", val).to_value())
                })
                .sync_create()
                .build();
            self.on_outputs_box_is_valid_changed(outputs_box.is_valid());
            outputs_box.connect_notify_local(
                Some("is-valid"),
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |obox, _| {
                        this.on_outputs_box_is_valid_changed(obox.is_valid());
                    }
                ),
            );
            self.add_output.connect_clicked(move |_| {
                outputs_box.add(&OutputConfig::default(), true);
            });

            // Validation (exec path is bound to settings above, so its value
            // is already populated here; the entry rows are populated in
            // setup() and checked there).
            let mpd_exec_path = self.mpd_exec_path.get();
            self.on_exec_path_changed();
            mpd_exec_path.connect_text_length_notify(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.on_exec_path_changed();
                }
            ));

            // Resampler
            self.on_resampler_plugin_changed();
            let resampler_plugin = self.resampler_plugin.get();
            self.force_resampler.connect_active_notify(clone!(
                #[weak(rename_to = this)]
                self,
                move |toggle| {
                    let this = this;
                    if !toggle.is_active() {
                        resampler_plugin.set_selected(0); // Auto
                    }
                    this.on_resampler_plugin_changed();
                }
            ));
            self.resampler_plugin.connect_selected_notify(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.on_resampler_plugin_changed();
                }
            ));
            self.soxr_quality.connect_selected_notify(clone!(
                #[weak(rename_to = this)]
                self,
                move |quality| {
                    let mut selected = quality.selected();
                    if selected == u32::MAX {
                        selected = 1; // default to High
                    }
                    // Custom => show more config rows
                    // Just need to set the precision row's visibility (others are daisy chained)
                    this.soxr_custom_precision.set_visible(selected == 5);
                }
            ));
        }

        fn dispose(&self) {
            let client_settings = settings_manager().child("client");
            if let Some(id) = self.viz_mode_setting_id.take() {
                client_settings.disconnect(id);
            }
            if let Some(id) = self.viz_source_setting_id.take() {
                client_settings.disconnect(id);
            }
            if let Some(server) = self.standalone_server.upgrade() {
                if let Some(id) = self.server_status_id.take() {
                    server.disconnect(id);
                }
            }
            if let Some(player) = self.player.upgrade() {
                if let Some(id) = self.player_fft_param_id.take() {
                    player.disconnect(id);
                }
            }
        }
    }
    impl WidgetImpl for ClientPreferences {}
    impl PreferencesPageImpl for ClientPreferences {}

    impl ClientPreferences {
        /// Populate widget state from settings and the parsed standalone
        /// config. Called from `setup()` after loading `standalone_cfg`.
        pub fn populate_from_settings(&self) {
            // Standalone mode
            let library_path = {
                let cfg = self.standalone_cfg.borrow();
                cfg.music_directory.clone()
            };
            self.set_music_library_path(if library_path.is_empty() {
                None
            } else {
                Some(&library_path)
            });
            let resampler = self.standalone_cfg.borrow().resampler;
            self.populate_resampler_widgets(resampler);
            self.force_resampler
                .set_active(self.standalone_cfg.borrow().resampler.is_some());

            // Client mode connection params. These are intentionally NOT
            // bound to the settings; they are only saved when the user
            // clicks "Reconnect".
            let conn_settings = settings_manager().child("client");
            self.mpd_host.set_text(&conn_settings.string("mpd-host"));
            self.mpd_unix_socket
                .set_text(&conn_settings.string("mpd-unix-socket"));
            self.mpd_port
                .set_text(&conn_settings.uint("mpd-port").to_string());

            // Visualiser
            self.fifo_format
                .set_text(&conn_settings.string("mpd-fifo-format"));
            let player_settings = settings_manager().child("player");
            self.fft_fps
                .set_value(player_settings.uint("visualizer-fps") as f64);
            self.fft_n_samples
                .set_selected(match player_settings.uint("visualizer-fft-samples") {
                    512 => 0,
                    1024 => 1,
                    2048 => 2,
                    4096 => 3,
                    // Settings may hold an arbitrary u32; fall back to the
                    // 1024 default instead of panicking.
                    _ => 1,
                });
            self.fft_n_bins
                .set_value(player_settings.uint("visualizer-spectrum-bins") as f64);

            if let Some(server) = self.standalone_server.upgrade() {
                self.on_standalone_status_changed(matches!(
                    server.status(),
                    ConnectionState::Connected
                ));
            }
        }

        pub fn on_standalone_status_changed(&self, running: bool) {
            if running {
                self.standalone_status.set_subtitle("Running");
                set_status_icon(&self.standalone_status_icon.get(), StatusIconState::Full);
            } else {
                self.standalone_status.set_subtitle("Failing");
                set_status_icon(
                    &self.standalone_status_icon.get(),
                    StatusIconState::Disabled,
                );
            }
            self.update_apply_sensitivity();
        }

        fn update_apply_sensitivity(&self) {
            self.apply_standalone_config.set_sensitive(
                self.has_library_path.get()
                    && self.outputs_box.is_valid()
                    && (!self.mpd_override_exec_path.is_active() || self.valid_exec_path.get()),
            );
        }

        fn update_fft_reconnect_sensitivity(&self) {
            self.fft_reconnect
                .set_sensitive(self.valid_fifo_format.get());
        }

        fn on_exec_path_changed(&self) {
            let entry_row = self.mpd_exec_path.get();
            let valid = entry_row.text_length() > 0;
            set_row_error(&entry_row, !valid);
            self.valid_exec_path.set(valid);
            self.update_apply_sensitivity();
        }

        fn set_music_library_path(&self, path: Option<&str>) {
            let library_path_row = self.mpd_library_path.get();
            if let Some(path) = path {
                library_path_row.set_subtitle(path);
                self.has_library_path.set(true);
            } else {
                library_path_row.set_subtitle("(unset)");
                self.has_library_path.set(false);
            }
            set_row_error(&library_path_row, !self.has_library_path.get());
            self.update_apply_sensitivity();
        }

        fn on_outputs_box_is_valid_changed(&self, is_valid: bool) {
            set_row_error(&self.config_outputs_row.get(), !is_valid);
            self.update_apply_sensitivity();
        }

        fn on_hostname_changed(&self) {
            let entry_row = self.mpd_host.get();
            let valid = entry_row.text_length() > 0;
            set_row_error(&entry_row, !valid);
            self.valid_host.set(valid);
            self.update_reconnect_sensitivity();
        }

        fn on_port_changed(&self) {
            let entry_row = self.mpd_port.get();
            let valid = entry_row.text().parse::<u32>().is_ok();
            set_row_error(&entry_row, !valid);
            self.valid_port.set(valid);
            self.update_reconnect_sensitivity();
        }

        fn on_unix_socket_changed(&self) {
            let entry_row = self.mpd_unix_socket.get();
            let valid = entry_row.text_length() > 0;
            set_row_error(&entry_row, !valid);
            self.valid_unix_socket.set(valid);
            self.update_reconnect_sensitivity();
        }

        fn update_reconnect_sensitivity(&self) {
            self.reconnect
                .set_sensitive(if self.mpd_use_unix_socket.is_active() {
                    self.valid_unix_socket.get()
                } else {
                    self.valid_host.get() && self.valid_port.get()
                });
        }

        fn on_fifo_format_changed(&self) {
            let entry_row = self.fifo_format.get();
            let valid = AudioFormat::from_str(entry_row.text().as_str()).is_ok();
            set_row_error(&entry_row, !valid);
            self.valid_fifo_format.set(valid);
            self.update_fft_reconnect_sensitivity();
        }

        pub fn on_connection_state_changed(&self, cs: &ClientState) {
            let conn_state = cs.connection_state();
            self.mpd_status
                .set_enable_expansion(conn_state == ConnectionState::Connected);
            match conn_state {
                ConnectionState::NotConnected => {
                    self.mpd_status.set_subtitle("Failed to connect");
                    set_status_icon(&self.mpd_status_icon.get(), StatusIconState::Disabled);
                }
                ConnectionState::Connecting => {
                    self.mpd_status.set_subtitle("Connecting...");
                    set_status_icon(&self.mpd_status_icon.get(), StatusIconState::Loading);
                }
                ConnectionState::Unauthenticated => {
                    self.mpd_status.set_subtitle("Authentication failed");
                    set_status_icon(&self.mpd_status_icon.get(), StatusIconState::Disabled);
                }
                ConnectionState::CredentialStoreError => {
                    self.mpd_status.set_subtitle("Credential store error");
                    set_status_icon(&self.mpd_status_icon.get(), StatusIconState::Disabled);
                }
                ConnectionState::WrongPassword => {
                    self.mpd_status.set_subtitle("Incorrect password");
                    set_status_icon(&self.mpd_status_icon.get(), StatusIconState::Disabled);
                }
                ConnectionState::ConnectionRefused => {
                    self.mpd_status.set_subtitle("Connection refused");
                    set_status_icon(&self.mpd_status_icon.get(), StatusIconState::Disabled);
                }
                ConnectionState::SocketNotFound => {
                    self.mpd_status.set_subtitle("Socket not found");
                    set_status_icon(&self.mpd_status_icon.get(), StatusIconState::Disabled);
                }
                ConnectionState::Connected => {
                    self.mpd_status.set_subtitle("Connected");
                    set_status_icon(&self.mpd_status_icon.get(), StatusIconState::Full);
                }
            }
            self.update_reconnect_sensitivity();
        }

        pub fn on_playlists_status_changed(&self, cs: &ClientState) {
            // TODO: translatable
            let row = self.playlists_status.get();
            let icon = self.playlists_status_icon.get();
            if cs.supports_playlists() {
                set_status_icon(&icon, StatusIconState::Full);
                row.set_title("Playlists support: enabled");
                row.set_subtitle("Playlist-related features are enabled.");
            } else {
                set_status_icon(&icon, StatusIconState::Disabled);
                row.set_title("Playlists support: disabled");
                row.set_subtitle("Enable playlists DB in your mpd.conf first.");
            }
        }

        pub fn on_stickers_status_changed(&self, cs: &ClientState) {
            let row = self.stickers_status.get();
            let icon = self.stickers_status_icon.get();

            let (icon_state, title, subtitle) = cs.stickers_support_level().get_ui_elements();
            set_status_icon(&icon, icon_state);
            row.set_title(&title);
            row.set_subtitle(&subtitle);
        }

        /// Visibility of the Viz data source group rows.
        /// Standalone Mode locks the source to the hidden internal FIFO, so the data source combo
        /// and both the PipeWire- and FIFO-specific rows are hidden. In client mode the rows follow the
        /// selected data source.
        fn update_visualizer_config_visibility(&self) {
            let standalone = self.mpd_use_own_server.get().enables_expansion();
            let idx = self.viz_source.get().selected(); // 0 = fifo, 1 = pipewire
            self.viz_source.set_visible(!standalone);
            let pw_visible = !standalone && idx == 1;
            self.pipewire_devices.set_visible(pw_visible);
            self.pipewire_restart_between_songs.set_visible(pw_visible);
            let fifo_visible = !standalone && idx == 0;
            self.fifo_path.set_visible(fifo_visible);
            self.fifo_format.set_visible(fifo_visible);
        }

        pub fn update_pipewire_device_list(&self, maybe_devices: Option<Vec<String>>) {
            self.pipewire_devices.set_model(
                maybe_devices
                    .map(|devices: Vec<String>| {
                        let mut device_list = vec!["(auto)"];
                        device_list
                            .append(&mut devices.iter().map(String::as_ref).collect::<Vec<&str>>());
                        gtk::StringList::new(&device_list)
                    })
                    .as_ref(),
            );
        }

        pub fn update_pipewire_current_device(&self, curr_device: Option<i32>) {
            // Position -1 means auto.
            if let Some(curr_device) = curr_device {
                self.pipewire_devices.set_selected((curr_device + 1) as u32);
            }
        }

        fn on_resampler_plugin_changed(&self) {
            // Single handler for the plugin combo: row subtitle + which config
            // list box (libsamplerate vs soxr) is visible.
            let selected = self
                .resampler_plugin
                .selected_item()
                .and_downcast::<gtk::StringObject>()
                .map_or(String::from(""), |s| s.string().to_string());
            self.libsamplerate_config
                .set_visible(selected == "LibSampleRate");
            self.soxr_config.set_visible(selected == "SoX");
            if self.force_resampler.is_active()
                && let Some(s) = self
                    .resampler_plugin
                    .selected_item()
                    .and_downcast::<gtk::StringObject>()
            {
                self.config_resampler_row.set_subtitle(s.string().as_str());
            } else {
                self.config_resampler_row.set_subtitle("Let MPD decide");
            }
        }

        /// Populate the resampler subpage widgets from a `Resampler` value.
        fn populate_resampler_widgets(&self, resampler: Option<Resampler>) {
            let plugin = self.resampler_plugin.get();
            match resampler {
                Some(resampler) => {
                    plugin.set_selected(resampler.ui_plugin_index());
                    match resampler {
                        Resampler::LibSampleRate(typ) => {
                            self.libsamplerate_type.set_selected(typ.min(4) as u32);
                        }
                        Resampler::Soxr(threads, qual) => {
                            self.soxr_threads.set_value(threads as f64);
                            self.soxr_quality.set_selected(qual.ui_quality_index());
                            if let SoxrPreset::Custom(
                                precision,
                                phase_response,
                                passband_end,
                                stopband_begin,
                                attenuation,
                            ) = qual
                            {
                                self.soxr_custom_precision.set_selected(
                                    self.soxr_custom_precision
                                        .model()
                                        .and_downcast::<gtk::StringList>()
                                        .unwrap()
                                        .find(precision.to_string().as_str())
                                        .min(4),
                                );
                                self.soxr_custom_phase_response
                                    .set_value(phase_response as f64);
                                self.soxr_custom_passband_end.set_value(passband_end);
                                self.soxr_custom_stopband_begin.set_value(stopband_begin);
                                self.soxr_custom_attenuation.set_value(attenuation);
                            }
                        }
                        _ => {}
                    }
                }
                None => {
                    plugin.set_selected(0);
                }
            }
        }

        /// Read the resampler subpage widgets into a `Resampler`, or `None`
        /// when "force resampler" is off or Auto is selected.
        pub fn generate_resampler_config(&self) -> Option<Resampler> {
            if !self.force_resampler.is_active() {
                return None;
            }
            match self.resampler_plugin.selected() {
                1 => Some(Resampler::Internal),
                2 => Some(Resampler::LibSampleRate(
                    self.libsamplerate_type.selected().min(4) as u8,
                )),
                3 => Some(Resampler::Soxr(
                    self.soxr_threads.value().max(0.0).min(16.0).round() as u8,
                    match self.soxr_quality.selected() {
                        5 => SoxrPreset::Custom(
                            self.soxr_custom_precision
                                .selected_item()
                                .and_downcast::<gtk::StringObject>()
                                .unwrap()
                                .string()
                                .to_string()
                                .parse::<u8>()
                                .unwrap(),
                            self.soxr_custom_phase_response
                                .value()
                                .max(0.0)
                                .min(100.0)
                                .round() as u8,
                            self.soxr_custom_passband_end.value() as f64,
                            self.soxr_custom_stopband_begin.value() as f64,
                            self.soxr_custom_attenuation.value() as f64,
                        ),
                        idx => SoxrPreset::from_ui_quality_index(idx),
                    },
                )),
                _ => None,
            }
        }
    }
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
    /// Wire the external dependencies (application, player, dialog), load
    /// the standalone config file and perform the initial state population
    /// and sensitivity updates.
    pub fn setup(&self, app: &EuphonicaApplication, player: &Player, dialog: &Preferences) {
        let imp = self.imp();
        let client_state = app.get_client().get_client_state();
        imp.dialog.set(Some(dialog));
        imp.client_state.set(Some(&client_state));
        imp.standalone_server.set(Some(app.get_server()));
        imp.player.set(Some(player));
        // Subpage navigation
        let outputs_subpage = self.imp().outputs_subpage.get();
        self.imp().config_outputs_row.connect_activated(clone!(
            #[weak]
            dialog,
            move |_| {
                dialog.push_subpage(&outputs_subpage);
            }
        ));
        let resampler_subpage = self.imp().resampler_subpage.get();
        self.imp().config_resampler_row.connect_activated(clone!(
            #[weak]
            dialog,
            move |_| {
                dialog.push_subpage(&resampler_subpage);
            }
        ));

        // Upon init, read the managed MPD config file or create a fresh one
        // in-memory in case there's none or the existing one has issues.
        let mut has_existing = false;
        if let Ok(mut file) = File::open(&get_standalone_config_path()) {
            let mut txt = String::new();
            if file.read_to_string(&mut txt).is_ok() {
                if let Ok(cfg) = MpdConfig::try_from(txt.as_str()) {
                    let _ = imp.standalone_cfg.replace(cfg);
                    has_existing = true;
                }
            }
        }
        if !has_existing {
            // Initialise with sensible defaults (the Default trait only
            // creates an empty one for filling in by try_from, not usable
            // as a base here).
            let _ = imp.standalone_cfg.replace(MpdConfig::new_minimal());
        }

        // Populate the audio outputs subpage eagerly (like the resampler
        // settings) so that Apply always reads widget state that matches the
        // parsed config.
        imp.outputs_box
            .init_from_config(&imp.standalone_cfg.borrow());

        // This runs AFTER having settled the standalone config situation.
        imp.populate_from_settings();

        // Standalone mode.
        let conn_settings = settings_manager().child("client");
        conn_settings
            .bind(
                "mpd-use-unix-socket",
                &imp.mpd_use_unix_socket.get(),
                "active",
            )
            .build();

        let _ = imp
            .server_status_id
            .replace(Some(app.get_server().connect_notify_local(
                Some("status"),
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |ss, _| {
                        this.imp().on_standalone_status_changed(matches!(
                            ss.status(),
                            ConnectionState::Connected
                        ));
                    }
                ),
            )));

        let password_field = imp.mpd_password.get();
        glib::spawn_future_local(async move {
            match get_mpd_password_async().await {
                Ok(maybe_password) => {
                    // At startup the password entry is disabled with a tooltip stating that
                    // the credential store is not available.
                    password_field.set_sensitive(true);
                    password_field.set_tooltip_text(None);
                    if let Some(password) = maybe_password {
                        password_field.set_text(&password);
                    }
                }
                Err(_e) => {
                    // Credential store unavailable; leave the entry disabled.
                }
            }
        });

        // Display connection status
        imp.on_connection_state_changed(&client_state);
        client_state.connect_notify_local(Some("connection-state"), {
            clone!(
                #[weak(rename_to = this)]
                self,
                move |cs, _| {
                    this.imp().on_connection_state_changed(cs);
                }
            )
        });

        imp.on_playlists_status_changed(&client_state);
        client_state.connect_notify_local(Some("supports-playlists"), {
            clone!(
                #[weak(rename_to = this)]
                self,
                move |cs, _| {
                    this.imp().on_playlists_status_changed(cs);
                }
            )
        });

        imp.on_stickers_status_changed(&client_state);
        client_state.connect_notify_local(Some("stickers-support-level"), {
            clone!(
                #[weak(rename_to = this)]
                self,
                move |cs, _| {
                    this.imp().on_stickers_status_changed(cs);
                }
            )
        });

        imp.apply_standalone_config.connect_activated(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            app,
            move |_| {
                // Overwrite path with config then trigger reconnect
                {
                    let imp = this.imp();
                    let config_path = get_standalone_config_path();
                    let mut cfg = imp.standalone_cfg.borrow_mut();
                    // Apply all settings.
                    // Library path has already been applied the moment the browse window closed so skip it here.
                    // Outputs
                    cfg.audio_outputs = imp.outputs_box.get_config();
                    // Resampler
                    cfg.resampler = imp.generate_resampler_config();
                    let mut output =
                        File::create(&config_path).expect("Unable to write to config file");
                    write!(output, "{}", cfg).unwrap();
                }
                // Just to be sure
                if let Err(e) = settings_manager()
                    .child("client")
                    .set_boolean("mpd-use-own-server", true)
                {
                    eprintln!("Failed to set mpd-use-own-server: {e}");
                } else {
                    glib::spawn_future_local(async move {
                        let _ = app.refresh().await;
                    });
                }
            }
        ));

        imp.reconnect.connect_activated(clone!(
            #[weak(rename_to = this)]
            self,
            #[strong]
            conn_settings,
            #[weak]
            app,
            move |_| {
                if this.imp().mpd_use_unix_socket.is_active() {
                    let socket_path = this.imp().mpd_unix_socket.text();
                    if socket_path.is_empty() {
                        eprintln!("Refusing to reconnect: no unix socket path set.");
                        return;
                    }
                    let _ = conn_settings.set_string("mpd-unix-socket", &socket_path);
                } else {
                    let port = match this.imp().mpd_port.text().parse::<u32>() {
                        Ok(port) => port,
                        Err(_) => {
                            eprintln!("Refusing to reconnect: invalid MPD port number.");
                            return;
                        }
                    };
                    let _ = conn_settings.set_string("mpd-host", &this.imp().mpd_host.text());
                    let _ = conn_settings.set_uint("mpd-port", port);
                }

                let password_val = this.imp().mpd_password.text();
                let password_available = this.imp().mpd_password.is_sensitive();
                glib::spawn_future_local(clone!(
                    #[weak]
                    app,
                    async move {
                        if !password_available {
                            if let Err(e) = app.refresh().await {
                                eprintln!("MPD reconnect failed: {e:?}");
                            }
                            return;
                        }

                        let password: Option<&str> = if password_val.is_empty() {
                            None
                        } else {
                            Some(password_val.as_str())
                        };
                        match set_mpd_password(password).await {
                            Ok(()) => {
                                if let Err(e) = app.refresh().await {
                                    eprintln!("MPD reconnect failed: {e:?}");
                                }
                            }
                            Err(msg) => {
                                eprintln!("Failed to save MPD password: {msg}");
                            }
                        }
                    }
                ));
            }
        ));

        // Visualiser
        player
            .bind_property("fft-status", &imp.fifo_status.get(), "subtitle")
            .transform_to(|_, status: FftStatus| Some(status.get_description()))
            .sync_create()
            .build();

        // Get PipeWire devices, if the PipeWire backend is running
        self.imp().update_pipewire_device_list(
            player
                .get_fft_param(Some("pipewire"), "devices")
                .and_then(|variant| variant.get::<Vec<String>>()),
        );
        self.imp().update_pipewire_current_device(
            player
                .get_fft_param(Some("pipewire"), "current-device")
                .and_then(|variant| variant.get::<i32>()),
        );
        let _ = imp.player_fft_param_id.replace(Some(player.connect_closure(
            "fft-param-changed",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: Player, name: String, key: String, new_val: glib::Variant| {
                    // Currently only need to handle PipeWire
                    if name == "pipewire" {
                        match key.as_str() {
                            "devices" => {
                                this.imp()
                                    .update_pipewire_device_list(new_val.get::<Vec<String>>());
                            }
                            "current-device" => {
                                this.imp()
                                    .update_pipewire_current_device(new_val.get::<i32>());
                            }
                            _ => {}
                        }
                    }
                }
            ),
        )));

        let player_settings = settings_manager().child("player");
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
                let imp = this.imp();
                let pw_dev_idx = imp.pipewire_devices.selected();
                if pw_dev_idx != gtk::INVALID_LIST_POSITION {
                    player.set_fft_param(
                        Some("pipewire"),
                        "current-device",
                        (pw_dev_idx as i32 - 1).to_variant(),
                    );
                }
                // Skip this when in Standalone Mode (hardcoded config takes precedence).
                if !conn_settings.boolean("mpd-use-own-server") {
                    conn_settings
                        .set_string("mpd-fifo-format", &imp.fifo_format.text())
                        .expect("Cannot save FIFO settings");
                }
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
                glib::spawn_future_local(clone!(
                    #[weak]
                    player,
                    async move {
                        player.restart_fft_thread().await;
                    }
                ));
            }
        ));
    }
}
