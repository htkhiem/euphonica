use std::rc::Rc;

use adw::subclass::prelude::*;
use adw::prelude::*;
use gtk::{
    glib,
    CompositeTemplate
};

use glib::clone;

use crate::{
    client::{ClientState, ConnectionState, MpdWrapper},
    utils
};

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/org/euphonica/Euphonica/gtk/preferences/client.ui")]
    pub struct ClientPreferences {
        #[template_child]
        pub mpd_host: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub mpd_port: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub mpd_status: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub reconnect: TemplateChild<gtk::Button>,
        #[template_child]
        pub mpd_download_album_art: TemplateChild<adw::SwitchRow>
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

    impl ObjectImpl for ClientPreferences {}
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
            },
            ConnectionState::Connecting => {
                self.imp().mpd_status.set_subtitle("Connecting...");
                self.imp().reconnect.set_sensitive(false);
            },
            ConnectionState::Unauthenticated => {
                self.imp().mpd_status.set_subtitle("Authentication failed");
                if !self.imp().mpd_port.has_css_class("error") {
                    self.imp().reconnect.set_sensitive(true);
                }
            },
            ConnectionState::Connected => {
                self.imp().mpd_status.set_subtitle("Connected");
                if !self.imp().mpd_port.has_css_class("error") {
                    self.imp().reconnect.set_sensitive(true);
                }
            }
        }
    }

    pub fn setup(&self, client: Rc<MpdWrapper>) {
        let imp = self.imp();
        let client_state = client.clone().get_client_state();
        // Populate with current gsettings values
        let settings = utils::settings_manager();
        // These should only be saved when the Apply button is clicked.
        // As such we won't bind the widgets directly to the settings.
        let conn_settings = settings.child("client");
        imp.mpd_host.set_text(&conn_settings.string("mpd-host"));
        imp.mpd_port.set_text(&conn_settings.uint("mpd-port").to_string());

        // TODO: more input validation
        // Prevent entering anything other than digits into the port entry row
        // This is needed since using a spinbutton row for port entry feels a bit weird
        imp.mpd_port.connect_changed(clone!(
            #[strong(rename_to = this)]
            self,
            move |entry| {
                if entry.text().parse::<u32>().is_err() {
                    if !entry.has_css_class("error") {
                        entry.add_css_class("error");
                        this.imp().reconnect.set_sensitive(false);
                    }
                }
                else if entry.has_css_class("error") {
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
            )
        );

        imp.reconnect.connect_clicked(clone!(
            #[strong(rename_to = this)]
            self,
            #[strong]
            conn_settings,
            #[weak]
            client,
            move |_| {
                let _ = conn_settings.set_string("mpd-host", &this.imp().mpd_host.text());
                let _ = conn_settings.set_uint("mpd-port", this.imp().mpd_port.text().parse::<u32>().unwrap());
                let _ = client.queue_connect();
            }
        ));
        let mpd_download_album_art = imp.mpd_download_album_art.get();
        conn_settings
            .bind(
                "mpd-download-album-art",
                &mpd_download_album_art,
                "active"
            )
            .build();
    }
}
