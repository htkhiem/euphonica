use adw::prelude::*;
use adw::subclass::prelude::*;

use glib::{
    Object, clone,
};
use gtk::{CompositeTemplate, gdk, glib};
use mpd::status::AudioFormat;
use std::{
    ffi::OsStr,
    path::Path,
};

use crate::{
    common::{DSD_EXTENSIONS, ImageStack, LOSSLESS_PCM_EXTENSIONS, Marquee, tags},
    utils::prettify_audio_format,
};

static DSD_DISPLAY_NAME: &'static str = "Direct Stream Digital";

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/song-info.ui")]
    pub struct SongInfoDialog {
        #[template_child]
        pub cover: TemplateChild<ImageStack>,

        #[template_child]
        pub name: TemplateChild<Marquee>,
        #[template_child]
        pub format: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub path: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub copy_path: TemplateChild<gtk::Button>,
        #[template_child]
        pub last_modified: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub added: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub artist: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub artistsort: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub albumartist: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub album: TemplateChild<adw::ActionRow>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SongInfoDialog {
        const NAME: &'static str = "EuphonicaSongInfoDialog";
        type Type = super::SongInfoDialog;
        type ParentType = adw::Dialog;
    }

    impl ObjectImpl for SongInfoDialog {
        fn constructed(&self) {
            self.parent_constructed();
            // Always on :)
            self.name.set_should_run_and_check(true);

            // Wire up buttons
            self.copy_path.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    if let Some(subtitle) = this.path.subtitle() {
                        gdk::Display::default()
                            .unwrap()
                            .clipboard()
                            .set_text(subtitle.as_str());
                    }
                }
            ));
        }
    }

    impl WidgetImpl for SongInfoDialog {}

    // The bindings has "Adw" here to avoid colliding with GTK4 DialogImpl lol
    impl AdwDialogImpl for SongInfoDialog {}
}

// Common row widget for displaying a single song, used across the UI.
glib::wrapper! {
    pub struct SongInfoDialog(ObjectSubclass<imp::SongInfoDialog>)
    @extends adw::Dialog, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::ShortcutManager;
}

impl SongInfoDialog {
    /// Constructs a dialog containing the song's information directly. No GObject binding necessary here.
    /// Upon each "Track info" click we'll construct a fresh new dialog.
    pub fn from_mpd_song(
        song: mpd::song::Song,
        // If not given, will not show cover
        cover: Option<gdk::Texture>,
    ) -> Self {
        let res: Self = Object::builder().build();

        let imp = res.imp();
        let path = Path::new(&song.file);
        imp.name
            .label()
            .set_label(path.file_name().and_then(OsStr::to_str).unwrap_or(""));
        imp.path.set_subtitle(&song.file);

        // As always, DSD has to be handled early and specially...
        let extension: Option<&str> = path.extension().and_then(OsStr::to_str);
        if let Some(extension) = extension.as_deref() {
            if DSD_EXTENSIONS.contains(&extension) {
                imp.format.set_subtitle(DSD_DISPLAY_NAME);
            }
        }

        // Unlike SongInfo's constructor we'll show repeated tags here & will also parse way more tags.
        let mut albums: Vec<String> = Vec::new();
        let mut artists: Vec<String> = Vec::new();
        let mut artistsorts: Vec<String> = Vec::new();
        let mut albumartists: Vec<String> = Vec::new();
        for (tag, val) in song.tags.into_iter() {
            match tag.to_lowercase().as_str() {
                tags::ALBUM => {
                    albums.push(val);
                }
                tags::ARTIST => {
                    artists.push(val);
                }
                tags::ARTISTSORT => {
                    artistsorts.push(val);
                }
                tags::ALBUMARTIST => {
                    albumartists.push(val);
                }
                // Lots more tags to parse


                // The following are guaranteed that there are at most one each. They aren't tags but rather
                // MPD's own metadata fields.
                tags::FORMAT => {

                    if let Ok(format) = val.parse::<AudioFormat>() {
                        if let Some(extension) = extension {
                            if LOSSLESS_PCM_EXTENSIONS.contains(&extension) {
                                imp.format.set_subtitle(&format!("Lossless {}", prettify_audio_format(&format)));
                            } else if format.bits == 1 {
                                // As of v0.24 MPD doesn't return a Format key when it's DSD, but who knows, put this here just in case they
                                // decide to in the future.
                                imp.format.set_subtitle(DSD_DISPLAY_NAME);
                            } else {
                                imp.format.set_subtitle(&prettify_audio_format(&format))
                            }
                        }
                    }
                }
                tags::ADDED => {
                    // Just display verbatim
                    imp.added.set_subtitle(&val);
                }
                tags::LAST_MODIFIED => {
                    // Just display verbatim
                    imp.last_modified.set_subtitle(&val);
                }
                _ => {}
            }
        }

        if let Some(cover) = cover {
            imp.cover.show(&cover);
        }

        res
    }
}
