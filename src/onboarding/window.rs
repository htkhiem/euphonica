/* window.rs
 *
 * Copyright 2026 htkhiem2000
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

use crate::{application::EuphonicaApplication, common::ConnectionState, utils::settings_manager};
use adw::{prelude::*, subclass::prelude::*};
use glib::WeakRef;
use gtk::{
    gio::{self},
    glib::{self, clone},
};
use std::cell::Cell;

mod imp {
    use std::cell::RefCell;

    use ashpd::desktop::file_chooser::SelectedFiles;

    use crate::{
        client::{
            ClientState,
            password::{get_mpd_password_async, set_mpd_password},
        },
        common::ConnectionState,
        preferences::{AudioOutputs, StatusIconState, set_status_icon},
        server::config::{MpdConfig, OutputConfig},
        utils::tokio_runtime,
    };

    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/onboarding-window.ui")]
    pub struct EuphonicaOnboardingWindow {
        // Top level widgets
        #[template_child]
        pub page_stack: TemplateChild<adw::ViewStack>,

        // Page 0: welcome
        #[template_child]
        pub p0_next: TemplateChild<gtk::Button>,

        //  Page 1: app mode
        #[template_child]
        pub standalone_mode_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub client_mode_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub standalone_mode: TemplateChild<gtk::CheckButton>,
        #[template_child]
        pub client_mode: TemplateChild<gtk::CheckButton>,
        #[template_child]
        pub p1_prev: TemplateChild<gtk::Button>,
        #[template_child]
        pub p1_next: TemplateChild<gtk::Button>,

        // Page 2: server/connection settings depending on selection
        // 2.1: standalone mode settings
        #[template_child]
        pub standalone_mode_settings: TemplateChild<gtk::Box>,
        #[template_child]
        pub mpd_library_path: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub mpd_library_browse: TemplateChild<gtk::Button>,
        #[template_child]
        pub config_outputs_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub standalone_status: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub standalone_status_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub outputs_box: TemplateChild<AudioOutputs>,
        #[template_child]
        pub add_output: TemplateChild<gtk::Button>,
        #[template_child]
        pub outputs_back: TemplateChild<gtk::Button>,
        #[template_child]
        pub mpd_override_exec_path: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub mpd_exec_path: TemplateChild<adw::EntryRow>,
        // 2.2: client mode settings. TODO: can we avoid replicating the settings here?
        #[template_child]
        pub client_mode_settings: TemplateChild<gtk::Box>,
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
        pub mpd_status: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub mpd_status_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub mpd_download_album_art: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub mpd_backup_meta_as_stickers: TemplateChild<adw::SwitchRow>,
        // Common next/prev
        #[template_child]
        pub p2_prev: TemplateChild<gtk::Button>,
        #[template_child]
        pub p2_next: TemplateChild<gtk::Button>,

        // Page 3: library organisation
        #[template_child]
        pub release_folder_library_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub mixed_library_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub release_folder_library_mode: TemplateChild<gtk::CheckButton>,
        #[template_child]
        pub mixed_library_mode: TemplateChild<gtk::CheckButton>,
        #[template_child]
        pub p3_prev: TemplateChild<gtk::Button>,
        #[template_child]
        pub finish_btn: TemplateChild<gtk::Button>,

        // Error flags
        pub valid_exec_path: Cell<bool>, // for now simply check if not empty
        pub has_library_path: Cell<bool>,
        pub valid_host: Cell<bool>,
        pub valid_port: Cell<bool>,

        pub standalone_cfg: RefCell<MpdConfig>,
        pub app: WeakRef<EuphonicaApplication>,
        pub onboard_success: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for EuphonicaOnboardingWindow {
        const NAME: &'static str = "EuphonicaOnboardingWindow";
        type Type = super::EuphonicaOnboardingWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for EuphonicaOnboardingWindow {
        // fn dispose(&self) {}

        fn constructed(&self) {
            self.parent_constructed();

            // Prev/next buttons. This is ugly as heck but I'm lazy + there aren't that many pages.
            self.p0_next.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.goto_page("app_mode");
                }
            ));
            self.p1_prev.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.goto_page("welcome");
                }
            ));
            self.p1_next.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.goto_page("server_conn");
                }
            ));
            self.p2_next.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |btn| {
                    // Page 2's config has to go green before we allow the wizard to proceed.
                    btn.set_sensitive(false);
                    // Attempt to start server. If successful, will proceed to next page after a delay
                    // (to let the user see the status row turning green)
                    glib::spawn_future_local(clone!(
                        #[weak]
                        this,
                        #[weak]
                        btn,
                        async move {
                            if let Some(app) = this.app.upgrade() {
                                if app.refresh().await.is_ok() {
                                    glib::timeout_add_local_once(
                                        std::time::Duration::from_millis(400),
                                        clone!(
                                            #[weak]
                                            this,
                                            #[weak]
                                            btn,
                                            move || {
                                                this.goto_page("library");
                                                // Keep button disabled until out of sight
                                                btn.set_sensitive(true);
                                            }
                                        ),
                                    );
                                    return;
                                }
                            }
                            // Else allow retry immediately, no timeout
                            btn.set_sensitive(true);
                        }
                    ));
                }
            ));
            self.p2_prev.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.goto_page("app_mode");
                }
            ));
            // p3_next is the finish button, to be wired in new().
            self.p3_prev.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.goto_page("server_conn");
                }
            ));

            let client_settings = settings_manager().child("client");

            // Page 1
            for (btn, option) in [
                (self.standalone_mode_btn.get(), self.standalone_mode.get()),
                (self.client_mode_btn.get(), self.client_mode.get()),
            ] {
                let p1_next = self.p1_next.get();
                btn.connect_clicked(move |_| {
                    p1_next.set_sensitive(true);
                    option.set_active(true);
                });
            }
            client_settings
                .bind("mpd-use-own-server", &self.standalone_mode.get(), "active")
                .build();

            // Page 2
            // Changes to settings under this page are immediatelly written to the settings backend
            // as there is no risk of unsaved settings breaking the next startup. Thing is, we won't
            // progress out of this onboarding wizard until a connection has been established, so there
            // is no notion of "next startup is broken due to half-edited configs that the user didn't
            // explicitly save last time". Sounds messy, but I dunno how to put this better, sorry.
            // Standalone mode
            // Library browse: initial state is invalid (unspecified)
            self.mpd_library_browse.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    let (sender, receiver) = oneshot::channel();
                    tokio_runtime().spawn(async move {
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
            // Override exec path
            client_settings
                .bind(
                    "mpd-override-exec-path",
                    &self.mpd_override_exec_path.get(),
                    "active",
                )
                .build();
            let mpd_exec_path = self.mpd_exec_path.get();
            client_settings
                .bind("mpd-exec-path", &mpd_exec_path, "text")
                .build();
            self.on_exec_path_changed();
            mpd_exec_path.connect_text_length_notify(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.on_exec_path_changed();
                }
            ));
            // Configure outputs
            self.config_outputs_row.connect_activated(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.goto_page("outputs");
                }
            ));
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
            self.outputs_back.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.goto_page("server_conn");
                }
            ));
            // Client mode
            self.on_hostname_changed();
            self.mpd_host.connect_changed(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.on_hostname_changed();
                }
            ));
            client_settings
                .bind("mpd-host", &self.mpd_host.get(), "text")
                .build();
            self.on_port_changed();
            self.mpd_port.connect_changed(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.on_port_changed();
                }
            ));
            client_settings
                .bind("mpd-port", &self.mpd_port.get(), "text")
                .mapping(|var, _| Some(var.get::<u32>().unwrap().to_string().to_value()))
                .set_mapping(|val, _| {
                    Some(
                        val.get::<&str>()
                            .map(|s| s.parse::<u32>().ok())
                            .ok()
                            .flatten()
                            .unwrap_or(6600)
                            .to_variant(),
                    )
                })
                .build();
            let password_field = self.mpd_password.get();
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
                        // println!("{e:?}");
                    }
                }
            });
            self.mpd_password.connect_changed(move |entry| {
                if entry.is_sensitive() {
                    glib::spawn_future_local(clone!(
                        #[weak]
                        entry,
                        async move {
                            if entry.is_sensitive() {
                                let password = entry.text();
                                let password: Option<&str> = if password.is_empty() {
                                    None
                                } else {
                                    Some(password.as_str())
                                };
                                let _ = set_mpd_password(password).await;
                            }
                        }
                    ));
                }
            });

            // Page 3
            let library_settings = settings_manager().child("library");
            library_settings
                .bind(
                    "optimize-embedded-cover-loading",
                    &self.release_folder_library_mode.get(),
                    "active",
                )
                .flags(gio::SettingsBindFlags::SET)
                .build();

            for (btn, option) in [
                (
                    self.release_folder_library_btn.get(),
                    self.release_folder_library_mode.get(),
                ),
                (self.mixed_library_btn.get(), self.mixed_library_mode.get()),
            ] {
                let finish_btn = self.finish_btn.get();
                btn.connect_clicked(move |_| {
                    finish_btn.set_sensitive(true);
                    option.set_active(true);
                });
            }
        }
    }
    impl WidgetImpl for EuphonicaOnboardingWindow {}
    impl WindowImpl for EuphonicaOnboardingWindow {}
    impl ApplicationWindowImpl for EuphonicaOnboardingWindow {}
    impl AdwApplicationWindowImpl for EuphonicaOnboardingWindow {}

    impl EuphonicaOnboardingWindow {
        fn goto_page(&self, name: &'static str) {
            if self
                .page_stack
                .visible_child_name()
                .is_none_or(|curr_name| curr_name.as_str() != name)
            {
                self.page_stack.set_visible_child_name(name);
            }
        }
        fn update_test_config_btn_sensitivity(&self) {
            // Which flag to read depends on mode
            self.p2_next
                .set_sensitive(if self.standalone_mode.is_active() {
                    self.has_library_path.get()
                        && self.outputs_box.is_valid()
                        && (!self.mpd_override_exec_path.is_active() || self.valid_exec_path.get())
                } else {
                    self.valid_host.get() && self.valid_port.get()
                });
        }

        fn set_music_library_path(&self, path: Option<&str>) {
            let library_path_row = self.mpd_library_path.get();
            if let Some(path) = path {
                library_path_row.set_subtitle(path);
                if library_path_row.has_css_class("error") {
                    library_path_row.remove_css_class("error");
                }
                self.has_library_path.set(true);
            } else {
                library_path_row.set_subtitle("(unset)");
                if !library_path_row.has_css_class("error") {
                    library_path_row.add_css_class("error");
                }
                self.has_library_path.set(false);
            }
            self.update_test_config_btn_sensitivity();
        }

        fn on_exec_path_changed(&self) {
            let entry_row = self.mpd_exec_path.get();
            if entry_row.text_length() > 0 {
                if entry_row.has_css_class("error") {
                    entry_row.remove_css_class("error");
                }
                self.valid_exec_path.set(true);
            } else {
                if !entry_row.has_css_class("error") {
                    entry_row.add_css_class("error");
                }
                self.valid_exec_path.set(false);
            }
            self.update_test_config_btn_sensitivity();
        }

        fn on_outputs_box_is_valid_changed(&self, is_valid: bool) {
            let row = self.config_outputs_row.get();
            if is_valid {
                if row.has_css_class("error") {
                    row.remove_css_class("error");
                }
            } else {
                if !row.has_css_class("error") {
                    row.add_css_class("error");
                }
            }
            self.update_test_config_btn_sensitivity();
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
            self.update_test_config_btn_sensitivity();
        }

        fn on_hostname_changed(&self) {
            let entry_row = self.mpd_host.get();
            if entry_row.text_length() > 0 {
                if entry_row.has_css_class("error") {
                    entry_row.remove_css_class("error");
                }
                self.valid_host.set(true);
            } else {
                if !entry_row.has_css_class("error") {
                    entry_row.add_css_class("error");
                }
                self.valid_host.set(false);
            }
            self.update_test_config_btn_sensitivity();
        }

        fn on_port_changed(&self) {
            let entry_row = self.mpd_port.get();
            if entry_row.text().parse::<u32>().is_err() {
                if !entry_row.has_css_class("error") {
                    entry_row.add_css_class("error");
                }
                self.valid_port.set(false);
            } else {
                if entry_row.has_css_class("error") {
                    entry_row.remove_css_class("error");
                }
                self.valid_port.set(true);
            }
            self.update_test_config_btn_sensitivity();
        }

        pub fn on_connection_state_changed(&self, cs: &ClientState) {
            match cs.connection_state() {
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
            self.update_test_config_btn_sensitivity();
        }
    }
}

glib::wrapper! {
    pub struct EuphonicaOnboardingWindow(ObjectSubclass<imp::EuphonicaOnboardingWindow>)
        @extends gtk::Widget, gtk::Window, gtk::ApplicationWindow,
    adw::ApplicationWindow,
    @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible,
    gtk::Buildable, gtk::ConstraintTarget, gtk::Native, gtk::Root,
    gtk::ShortcutManager;
}

impl EuphonicaOnboardingWindow {
    pub fn new(application: &EuphonicaApplication) -> Self {
        let win: Self = glib::Object::builder()
            .property("application", application)
            .build();
        win.imp().app.set(Some(application));
        win.imp().onboard_success.set(false);

        // Display connection status
        let standalone_server = application.get_server();
        win.imp().on_standalone_status_changed(matches!(
            standalone_server.status(),
            ConnectionState::Connected
        ));
        // standalone mode
        standalone_server.connect_notify_local(
            Some("status"),
            clone!(
                #[weak(rename_to = this)]
                win,
                move |ss, _| {
                    this.imp().on_standalone_status_changed(matches!(
                        ss.status(),
                        ConnectionState::Connected
                    ));
                }
            ),
        );
        // client mode
        let client_state = application.get_client().get_client_state();
        win.imp().on_connection_state_changed(&client_state);
        client_state.connect_notify_local(
            Some("connection-state"),
            clone!(
                #[weak(rename_to = this)]
                win,
                move |cs, _| {
                    this.imp().on_connection_state_changed(cs);
                }
            ),
        );

        win.imp().finish_btn.connect_clicked(clone!(
            #[weak]
            win,
            move |_| {
                win.imp().onboard_success.set(true);
                win.close();
            }
        ));

        // Only emitted when the close button itself is clicked
        win.connect_close_request(|win| {
            win.imp()
                .app
                .upgrade()
                .unwrap()
                .conclude_onboarding(win.imp().onboard_success.get());
            glib::Propagation::Proceed
        });

        win
    }
}
