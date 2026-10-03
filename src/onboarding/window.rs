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

use crate::{application::EuphonicaApplication, utils::settings_manager};
use adw::{prelude::*, subclass::prelude::*};
use glib::WeakRef;
use gtk::{
    gio::{self},
    glib::{self, clone},
};
use std::cell::Cell;

use glib::Properties;

mod imp {
    use super::*;

    #[derive(Debug, Default, Properties, gtk::CompositeTemplate)]
    #[properties(wrapper_type = super::EuphonicaOnboardingWindow)]
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

    #[glib::derived_properties]
    impl ObjectImpl for EuphonicaOnboardingWindow {
        fn dispose(&self) {
            // // Disconnect all signal handlers registered on global/long-lived objects
            // if let Some(id) = self.settings_bg_blur_id.take() {
            //     let settings = settings_manager().child("ui");
            //     settings.disconnect(id);
            // }
            // if let Some(id) = self.settings_visualizer_id.take() {
            //     let settings = settings_manager().child("ui");
            //     settings.disconnect(id);
            // }
            // if let Some(client_state) = self.client_state.get() {
            //     if let Some(id) = self.client_state_idle_id.take() {
            //         client_state.disconnect(id);
            //     }
            //     if let Some(id) = self.client_state_conn_state_id.take() {
            //         client_state.disconnect(id);
            //     }
            //     if let Some(id) = self.client_state_pct_fg_id.take() {
            //         client_state.disconnect(id);
            //     }
            //     if let Some(id) = self.client_state_pct_bg_id.take() {
            //         client_state.disconnect(id);
            //     }
            //     if let Some(id) = self.client_state_n_fg_id.take() {
            //         client_state.disconnect(id);
            //     }
            //     if let Some(id) = self.client_state_n_bg_id.take() {
            //         client_state.disconnect(id);
            //     }
            // }
            // if let Some(id) = self.player_cover_changed_id.take()
            //     && let Some(player) = self.player.upgrade()
            // {
            //     player.disconnect(id);
            // }
            // if let Some(id) = self.player_title_changed_id.take()
            //     && let Some(player) = self.player.upgrade()
            // {
            //     player.disconnect(id);
            // }
        }

        fn constructed(&self) {
            self.parent_constructed();

            let this = self.obj().clone();
            // Prev/next buttons. This is ugly as heck but I'm lazy + there aren't that many pages.
            self.p0_next.connect_clicked(clone!(
                #[weak]
                this,
                move |_| {
                    this.goto_page("app_mode");
                }
            ));
            self.p1_prev.connect_clicked(clone!(
                #[weak]
                this,
                move |_| {
                    this.goto_page("welcome");
                }
            ));
            self.p1_next.connect_clicked(clone!(
                #[weak]
                this,
                move |_| {
                    this.goto_page("server_conn");
                }
            ));
            self.p2_next.connect_clicked(clone!(
                #[weak]
                this,
                move |btn| {
                    // Page 2's config has to go green before we allow the wizard to proceed.
                    btn.set_sensitive(false);
                    if true {
                        this.goto_page("server_conn");
                    }
                    btn.set_sensitive(true);
                }
            ));
            self.p2_prev.connect_clicked(clone!(
                #[weak]
                this,
                move |_| {
                    this.goto_page("app_mode");
                }
            ));
            // p3_next is the finish button, to be wired in new().
            self.p3_prev.connect_clicked(clone!(
                #[weak]
                this,
                move |_| {
                    this.goto_page("server_conn");
                }
            ));

            // Page 2
            for btn in [self.standalone_mode.get(), self.client_mode.get()] {
                let p1_next = self.p1_next.get();
                btn.connect_toggled(move |_| p1_next.set_sensitive(true));
            }
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
    pub fn goto_page(&self, name: &'static str) {
        if self
            .imp()
            .page_stack
            .visible_child_name()
            .is_none_or(|curr_name| curr_name.as_str() != name)
        {
            self.imp().page_stack.set_visible_child_name(name);
        }
    }

    pub fn new(application: &EuphonicaApplication) -> Self {
        let win: Self = glib::Object::builder()
            .property("application", application)
            .build();
        win.imp().app.set(Some(application));
        win.imp().onboard_success.set(false);

        // let client_state = app.get_client().get_client_state();
        // let _ = win.imp().client_state.set(client_state.clone());
        // let player = app.get_player();

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
