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

use crate::{
    application::EuphonicaApplication,
    client::{ClientState, ConnectionState, Result as ClientResult},
    common::{Album, Artist, INode, ThemeSelector, View, paintables::FadePaintable},
    library::{
        AlbumView, ArtistContentView, ArtistView, DynamicPlaylistEditorView, DynamicPlaylistView,
        FolderView, PlaylistView, RecentView,
    },
    player::{Player, PlayerBar, QueueView},
    sidebar::Sidebar,
    utils::{self, LazyInit, SearchableView, settings_manager},
};
use adw::{ColorScheme, StyleManager, prelude::*, subclass::prelude::*};
use auto_palette::{ImageData, Palette, color::RGB};
use glib::WeakRef;
use gtk::{
    CssProvider, cairo, gdk,
    gio::{self, SimpleActionGroup},
    glib::{self, BoxedAnyObject, SignalHandlerId, clone, closure_local},
    graphene, gsk,
};
use image::{DynamicImage, imageops::FilterType};
use libblur::{FastBlurChannels, ThreadingPolicy, stack_blur};
use mpd::Subsystem;
use std::{cell::RefCell, ops::Deref, path::PathBuf, thread, time::Duration};
use std::{
    cell::{Cell, OnceCell},
    sync::{Arc, Mutex},
};

use async_channel::Sender;
use glib::Properties;
use image::ImageReader as Reader;

mod imp {
    use super::*;

    #[derive(Debug, Default, Properties, gtk::CompositeTemplate)]
    #[properties(wrapper_type = super::EuphonicaOnboardingWindow)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/onboarding-window.ui")]
    pub struct EuphonicaOnboardingWindow {
        // Top level widgets
        // TODO: actual wizard using carousels (for now there's just a single page)
        // #[template_child]
        // pub carousel

        // Page 1: library organisation
        #[template_child]
        pub library_mode_illustration: TemplateChild<gtk::Image>,
        #[template_child]
        pub release_folder_library_mode: TemplateChild<gtk::CheckButton>,
        #[template_child]
        pub mixed_library_mode: TemplateChild<gtk::CheckButton>,
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

            // Page 1
            let library_settings = settings_manager().child("library");
            library_settings
                .bind(
                    "optimize-embedded-cover-loading",
                    &self.release_folder_library_mode.get(),
                    "active",
                )
                .build();

            self.update_library_mode_illustration(self.release_folder_library_mode.is_active());
            self.release_folder_library_mode
                .connect_active_notify(clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |btn| {
                        this.update_library_mode_illustration(btn.is_active());
                    }
                ));
        }
    }
    impl WidgetImpl for EuphonicaOnboardingWindow {}
    impl WindowImpl for EuphonicaOnboardingWindow {}
    impl ApplicationWindowImpl for EuphonicaOnboardingWindow {}
    impl AdwApplicationWindowImpl for EuphonicaOnboardingWindow {}

    impl EuphonicaOnboardingWindow {
        fn update_library_mode_illustration(&self, is_release_folder_mode: bool) {
            self.library_mode_illustration
                .set_icon_name(Some(if is_release_folder_mode {
                    "albums-in-folders-symbolic"
                } else {
                    "freeform-folders-symbolic"
                }));
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
