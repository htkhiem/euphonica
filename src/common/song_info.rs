use adw::prelude::*;
use adw::subclass::prelude::*;

use glib::{Object, clone};
use gtk::{CompositeTemplate, gdk, glib};
use mpd::status::AudioFormat;
use quick_xml::escape::escape;
use std::{cell::RefCell, ffi::OsStr, path::Path};

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
        // Unescaped URI so the copy button copies the real value, not the markup-escaped display one
        pub raw_path: RefCell<String>,
        #[template_child]
        pub duration: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub last_modified: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub added: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub metadata: TemplateChild<adw::PreferencesGroup>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SongInfoDialog {
        const NAME: &'static str = "EuphonicaSongInfoDialog";
        type Type = super::SongInfoDialog;
        type ParentType = adw::Dialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
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
                    let raw_path = this.raw_path.borrow();
                    gdk::Display::default()
                        .unwrap()
                        .clipboard()
                        .set_text(raw_path.as_str());
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
        cover: Option<gdk::Paintable>,
    ) -> Self {
        let res: Self = Object::builder().build();

        let imp = res.imp();
        // The fork's Song parser hoists some keys out of `tags` into struct fields:
        // - `title`/`artist`/`last_mod` only keep the LAST value of repeated keys
        // - `Time` (deprecated, low-precision `duration`) is discarded entirely
        let (file, title, last_mod, artist, duration, tags) = (
            song.file,
            song.title,
            song.last_mod,
            song.artist,
            song.duration,
            song.tags,
        );

        let path = Path::new(&file);
        imp.name
            .label()
            .set_label(path.file_name().and_then(OsStr::to_str).unwrap_or(""));
        // ActionRow subtitles are rendered as Pango markup, so we must XML-escape special chars
        // (all subtitles below are escaped)
        imp.raw_path.replace(file.clone());
        imp.path.set_subtitle(&escape(&file));

        // Duration is an MPD-computed value (in seconds), displayed with an "s" suffix.
        if let Some(duration) = duration {
            let text = if duration.subsec_nanos() == 0 {
                format!("{}s", duration.as_secs())
            } else {
                format!("{:.3}s", duration.as_secs_f64())
            };
            imp.duration.set_subtitle(&text);
        }
        if let Some(last_mod) = &last_mod {
            // Just display verbatim
            imp.last_modified.set_subtitle(last_mod);
        }

        // As always, DSD has to be handled early and specially...
        let extension: Option<&str> = path.extension().and_then(OsStr::to_str);
        if extension.is_some_and(|ext| DSD_EXTENSIONS.contains(&ext)) {
            imp.format.set_subtitle(DSD_DISPLAY_NAME);
        }

        // Unlike SongInfo's constructor we'll show repeated tags here & will also parse way more tags.
        // Groups tag values by lowercased name, preserving both first-seen order and all values, so
        // duplicated tags are all displayed to let the user spot broken metadata.
        let mut tag_values: Vec<(String, Vec<String>)> = Vec::new();
        for (tag, val) in tags {
            let lowered = tag.to_lowercase();
            // The following are guaranteed that there are at most one each. They aren't tags but rather
            // MPD's own metadata fields, displayed in the File group.
            if lowered == tags::FORMAT {
                if let Ok(format) = val.parse::<AudioFormat>() {
                    if let Some(extension) = extension {
                        if LOSSLESS_PCM_EXTENSIONS.contains(&extension) {
                            imp.format.set_subtitle(&format!(
                                "Lossless {}",
                                prettify_audio_format(&format)
                            ));
                        } else if format.bits == 1 {
                            // As of v0.24 MPD doesn't return a Format key when it's DSD, but who knows, put this here just in case they
                            // decide to in the future.
                            imp.format.set_subtitle(DSD_DISPLAY_NAME);
                        } else {
                            imp.format.set_subtitle(&prettify_audio_format(&format))
                        }
                    }
                }
            } else if lowered == tags::ADDED {
                // Just display verbatim
                imp.added.set_subtitle(&val);
            }
            match tag_values.iter_mut().find(|(name, _)| *name == lowered) {
                Some((_, vals)) => vals.push(val),
                None => tag_values.push((lowered, vec![val])),
            }
        }

        // Build the Metadata group's rows dynamically, in MPD's first-seen order.
        // Title & Artist come from the struct instead (see above).
        let metadata = &*imp.metadata;
        if let Some(title) = &title {
            let row = make_property_row("Title", false);
            row.set_subtitle(&escape(title));
            metadata.add(&row);
        }
        if let Some(artist) = &artist {
            let row = make_property_row("Artist", false);
            row.set_subtitle(&escape(artist));
            metadata.add(&row);
        }
        for (name, vals) in tag_values {
            // Already displayed in the File group
            if name == tags::FORMAT || name == tags::ADDED || name == tags::LAST_MODIFIED {
                continue;
            }
            // MusicBrainz ID rows get prettied-up titles & a copy button
            let copyable = name.starts_with("musicbrainz_");
            let row = make_property_row(&tags::prettify_tag_name(&name), copyable);
            row.set_subtitle(&escape(&vals.join("\n")));
            metadata.add(&row);
        }

        if let Some(cover) = cover {
            imp.cover.show(&cover);
            imp.cover.set_visible(true);
        }

        res
    }
}

/// Creates a `property`-styled ActionRow with a title, optionally with a copy-to-clipboard
/// suffix button that copies the row's subtitle.
fn make_property_row(title: &str, copyable: bool) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).build();
    row.add_css_class("property");
    if copyable {
        let button = gtk::Button::builder()
            .icon_name("copy-symbolic")
            .valign(gtk::Align::Center)
            .build();
        button.connect_clicked(clone!(
            #[weak]
            row,
            move |_| {
                if let Some(subtitle) = row.subtitle() {
                    gdk::Display::default()
                        .unwrap()
                        .clipboard()
                        .set_text(subtitle.as_str());
                }
            }
        ));
        row.add_suffix(&button);
    }
    row
}
