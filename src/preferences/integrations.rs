use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{CompositeTemplate, glib};
use std::cell::OnceCell;
use std::rc::Rc;

use crate::{
    application::update_xdg_background_request, cache::Cache,
    meta_providers::effective_provider_order, utils,
};

use super::ProviderRow;

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/preferences/integrations.ui")]
    pub struct IntegrationsPreferences {
        #[template_child]
        pub enable_mpris: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub run_in_background: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub autostart: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub start_minimized: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub xdg_warn_row: TemplateChild<adw::ActionRow>,

        #[template_child]
        pub lastfm_key: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub lastfm_download_album_art: TemplateChild<adw::SwitchRow>,

        #[template_child]
        pub musicbrainz_download_album_art: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub musicbrainz_download_artist_avatar: TemplateChild<adw::SwitchRow>,

        #[template_child]
        pub the_audio_db_key: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub the_audio_db_api_version: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub the_audio_db_download_album_art: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub the_audio_db_download_artist_avatar: TemplateChild<adw::SwitchRow>,

        #[template_child]
        pub order_box: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub on_exit_action: TemplateChild<adw::ComboRow>,
        pub cache: OnceCell<Rc<Cache>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for IntegrationsPreferences {
        const NAME: &'static str = "EuphonicaIntegrationsPreferences";
        type Type = super::IntegrationsPreferences;
        type ParentType = adw::PreferencesPage;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for IntegrationsPreferences {}
    impl WidgetImpl for IntegrationsPreferences {}
    impl PreferencesPageImpl for IntegrationsPreferences {}
}

glib::wrapper! {
    pub struct IntegrationsPreferences(ObjectSubclass<imp::IntegrationsPreferences>)
        @extends adw::PreferencesPage,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Widget;
}

impl Default for IntegrationsPreferences {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl IntegrationsPreferences {
    pub fn setup(&self, cache: Rc<Cache>) {
        let _ = self.imp().cache.set(cache);
        let imp = self.imp();
        // Populate with current gsettings values
        let settings = utils::settings_manager();

        let player_settings = settings.child("player");
        let state_settings = settings.child("state");
        let enable_mpris = imp.enable_mpris.get();
        player_settings
            .bind("enable-mpris", &enable_mpris, "active")
            .build();

        // On exit action
        let on_exit_action = imp.on_exit_action.get();
        let enum_val = state_settings.enum_("on-exit-action");
        let selected = match enum_val {
            0 => 0,
            1 => 1,
            2 => 2,
            3 => 3,
            _ => 0,
        };
        on_exit_action.set_selected(selected);
        let on_exit_settings = state_settings.clone();
        on_exit_action.connect_notify_local(Some("selected"), move |row, _| {
            let _ = on_exit_settings.set_enum("on-exit-action", row.selected() as i32);
        });

        let run_in_background = imp.run_in_background.get();
        let autostart = imp.autostart.get();
        let start_minimized = imp.start_minimized.get();
        let xdg_warn = imp.xdg_warn_row.get();

        // Init background & autostart toggle states
        // Do NOT bind the widgets directly to the settings to avoid an infinite loo
        // when we update the settings from code (system might refuse our background
        // and autostart requests).
        run_in_background.set_active(state_settings.boolean("run-in-background"));
        autostart.set_active(state_settings.boolean("autostart"));
        start_minimized.set_active(state_settings.boolean("start-minimized"));
        autostart
            .bind_property("active", &start_minimized, "sensitive")
            .sync_create()
            .build();

        run_in_background.connect_notify_local(Some("active"), |sw, _| {
            let settings = utils::settings_manager().child("state");
            let _ = settings.set_boolean("run-in-background", sw.is_active());
            update_xdg_background_request();
        });
        autostart.connect_notify_local(Some("active"), |sw, _| {
            let settings = utils::settings_manager().child("state");
            let _ = settings.set_boolean("autostart", sw.is_active());
            update_xdg_background_request();
        });
        start_minimized.connect_notify_local(Some("active"), |sw, _| {
            let settings = utils::settings_manager().child("state");
            let _ = settings.set_boolean("start-minimized", sw.is_active());
            update_xdg_background_request();
        });

        if !settings.boolean("background-portal-available") {
            xdg_warn.set_visible(true);
        }

        // Set up Last.fm settings
        let lastfm_settings = utils::meta_provider_settings("lastfm");
        let lastfm_key = imp.lastfm_key.get();
        let lastfm_download_album_art = imp.lastfm_download_album_art.get();
        // let lastfm_download_artist_avatar = imp.lastfm_download_artist_avatar.get();

        lastfm_settings.bind("api-key", &lastfm_key, "text").build();

        lastfm_settings
            .bind("download-album-art", &lastfm_download_album_art, "active")
            .build();

        // Set up MusicBrainz settings
        let mb_settings = utils::meta_provider_settings("musicbrainz");
        let mb_download_album_art = imp.musicbrainz_download_album_art.get();
        let mb_download_artist_avatar = imp.musicbrainz_download_artist_avatar.get();

        mb_settings
            .bind("download-album-art", &mb_download_album_art, "active")
            .build();

        mb_settings
            .bind(
                "download-artist-avatar",
                &mb_download_artist_avatar,
                "active",
            )
            .build();

        // Set up TheAudioDB settings
        let tada_settings = utils::meta_provider_settings("theaudiodb");
        let tada_key = imp.the_audio_db_key.get();
        let tada_api_version = imp.the_audio_db_api_version.get();
        let tada_download_album_art = imp.the_audio_db_download_album_art.get();
        let tada_download_artist_avatar = imp.the_audio_db_download_artist_avatar.get();

        tada_settings.bind("api-key", &tada_key, "text").build();

        let enum_val = tada_settings.enum_("api-version");
        tada_api_version.set_selected((enum_val - 1).max(0) as u32);
        let tada_api_version_settings = tada_settings.clone();
        tada_api_version.connect_notify_local(Some("selected"), move |row, _| {
            let _ = tada_api_version_settings.set_enum("api-version", (row.selected() + 1) as i32);
        });

        tada_settings
            .bind("download-album-art", &tada_download_album_art, "active")
            .build();

        tada_settings
            .bind(
                "download-artist-avatar",
                &tada_download_artist_avatar,
                "active",
            )
            .build();

        // LRCLIB currently needs no settings.

        // Set up priority settings. Providers missing from the user's stored
        // order are automatically appended by `effective_provider_order`.
        let order_box = self.imp().order_box.get();

        for row in effective_provider_order()
            .into_iter()
            .enumerate()
            .map(|(prio, key)| ProviderRow::new(self, &key, prio as i32))
        {
            order_box.append(&row);
        }
        order_box.set_sort_func(|r1, r2| {
            let pr1 = r1
                .downcast_ref::<adw::PreferencesRow>()
                .unwrap()
                .downcast_ref::<adw::ActionRow>()
                .unwrap()
                .downcast_ref::<ProviderRow>()
                .unwrap();
            let pr2 = r2
                .downcast_ref::<adw::PreferencesRow>()
                .unwrap()
                .downcast_ref::<adw::ActionRow>()
                .unwrap()
                .downcast_ref::<ProviderRow>()
                .unwrap();
            if pr1.priority() > pr2.priority() {
                gtk::Ordering::Larger
            } else if pr1.priority() < pr2.priority() {
                gtk::Ordering::Smaller
            } else {
                gtk::Ordering::Equal
            }
        });
    }

    fn regen_provider_list(&self) {
        // Priority & key
        let mut new_order: Vec<(i32, String)> = Vec::new();
        let mut idx = 0;
        while let Some(row) = self.imp().order_box.row_at_index(idx) {
            let provider_row = row
                .downcast_ref::<adw::PreferencesRow>()
                .unwrap()
                .downcast_ref::<adw::ActionRow>()
                .unwrap()
                .downcast_ref::<ProviderRow>()
                .unwrap();
            new_order.push((provider_row.priority(), provider_row.key()));
            idx += 1;
        }
        new_order.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let key_array: Vec<String> = new_order.into_iter().map(|elem| elem.1).collect();
        let _ = utils::settings_manager()
            .child("metaprovider")
            .set_value("order", &key_array.to_variant());
    }

    pub fn on_raise_provider(&self, curr_prio: i32) {
        if curr_prio > 0 {
            let order_box = self.imp().order_box.get();
            let this_row = order_box.row_at_index(curr_prio).unwrap();
            let this_row = this_row
                .downcast_ref::<adw::PreferencesRow>()
                .unwrap()
                .downcast_ref::<adw::ActionRow>()
                .unwrap()
                .downcast_ref::<ProviderRow>()
                .unwrap();
            let upper_row = order_box.row_at_index(curr_prio - 1).unwrap();
            let upper_row = upper_row
                .downcast_ref::<adw::PreferencesRow>()
                .unwrap()
                .downcast_ref::<adw::ActionRow>()
                .unwrap()
                .downcast_ref::<ProviderRow>()
                .unwrap();
            this_row.set_priority(curr_prio - 1);
            upper_row.set_priority(curr_prio);
            order_box.invalidate_sort();
            self.regen_provider_list();
        }
    }

    pub fn on_lower_provider(&self, curr_prio: i32) {
        let order_box = self.imp().order_box.get();
        if let Some(lower_list_row) = order_box.row_at_index(curr_prio + 1) {
            let this_row = order_box.row_at_index(curr_prio).unwrap();
            let this_row = this_row
                .downcast_ref::<adw::PreferencesRow>()
                .unwrap()
                .downcast_ref::<adw::ActionRow>()
                .unwrap()
                .downcast_ref::<ProviderRow>()
                .unwrap();
            let lower_row = lower_list_row
                .downcast_ref::<adw::PreferencesRow>()
                .unwrap()
                .downcast_ref::<adw::ActionRow>()
                .unwrap()
                .downcast_ref::<ProviderRow>()
                .unwrap();
            this_row.set_priority(curr_prio + 1);
            lower_row.set_priority(curr_prio);
            order_box.invalidate_sort();
            self.regen_provider_list();
        }
    }
}
