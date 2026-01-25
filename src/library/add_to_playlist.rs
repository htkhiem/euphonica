use glib::{Properties, WeakRef, clone};
use gtk::{CompositeTemplate, glib, prelude::*, subclass::prelude::*};
use std::cell::{Cell, OnceCell};

use crate::{
    common::{INode, Song},
    utils,
};
use mpd::SaveMode;

use super::Library;

// Common implementation of an "Add to playlist" menu button.
// It allows adding to an existing playlist, or creating a new
// one if the given name does not match any existing playlist.
mod imp {
    use super::*;

    #[derive(Properties, Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/add-to-playlist-button.ui")]
    #[properties(wrapper_type = super::AddToPlaylistButton)]
    pub struct AddToPlaylistButton {
        #[template_child]
        pub menu_btn: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub options: TemplateChild<gtk::ListView>,
        // We will filter the options ListView by the contents of the name_box. If
        // - The filtered ListView has more than one item and none is selected, or
        // - The filtered ListView has exactly one item, which is NOT selected
        //   and whose name does not perfectly match the name_box's text, or
        // - The ListView is empty after filtering by the term in the name box
        // then we will create a new playlist with the selected songs.
        // Else we will append the selected songs to an existing playlist.
        #[template_child]
        pub name_box: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub add_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub add_revealer: TemplateChild<gtk::Revealer>,
        #[property(get, set)]
        pub n_to_add: Cell<u32>, // Needed for setting the menu button's label
        #[property(get)]
        pub will_create: Cell<bool>,
        pub search_model: OnceCell<gtk::FilterListModel>,
        pub sel_model: OnceCell<gtk::SingleSelection>, // For playlists
        pub library: WeakRef<Library>,
        pub song_sel_model: WeakRef<gtk::MultiSelection>,
        #[property(get, set)]
        pub collapsed: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AddToPlaylistButton {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaAddToPlaylistButton";
        type Type = super::AddToPlaylistButton;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for AddToPlaylistButton {
        fn constructed(&self) {
            self.parent_constructed();

            let options = self.options.get();
            let settings = utils::settings_manager().child("library");

            let filter = gtk::StringFilter::builder()
                .expression(gtk::PropertyExpression::new(
                    INode::static_type(),
                    Option::<gtk::PropertyExpression>::None,
                    "uri",
                ))
                .match_mode(gtk::StringFilterMatchMode::Substring)
                .build();

            settings
                .bind("search-case-sensitive", &filter, "ignore-case")
                .flags(gio::SettingsBindFlags::GET | gio::SettingsBindFlags::INVERT_BOOLEAN)
                .build();

            let search_model =
                gtk::FilterListModel::new(Option::<gio::ListStore>::None, Some(filter.clone()));

            let sel_model = gtk::SingleSelection::new(Some(
                gtk::SortListModel::builder()
                    .incremental(true)
                    .model(&search_model)
                    .sorter(
                        &gtk::StringSorter::builder()
                            .expression(gtk::PropertyExpression::new(
                                INode::static_type(),
                                Option::<gtk::PropertyExpression>::None,
                                "last-modified",
                            ))
                            .build(),
                    )
                    .build(),
            ));
            sel_model.set_autoselect(false);
            sel_model.set_can_unselect(true);

            options.set_model(Some(&sel_model));

            let factory = gtk::SignalListItemFactory::new();
            factory.connect_setup(move |_, list_item| {
                // Use a simple label
                let label = gtk::Label::new(None);

                // TODO: should probably move this into UI definitions somehow
                label.set_margin_start(6);
                label.set_margin_end(6);
                label.set_margin_top(3);
                label.set_margin_bottom(3);
                label.set_halign(gtk::Align::Start);

                let list_item = list_item
                    .downcast_ref::<gtk::ListItem>()
                    .expect("Needs to be ListItem");
                list_item.set_child(Some(&label));
                list_item
                    .property_expression("item")
                    .chain_property::<INode>("uri")
                    .bind(&label, "label", gtk::Widget::NONE);
            });

            options.set_factory(Some(&factory));

            // We need to update the popover when any of these happens:
            // - name_box is changed (debounced please)
            // - playlists model is updated (remotely) (done in setup())
            // - options list selection is changed
            self.name_box.connect_search_changed(clone!(
                #[weak(rename_to = this)]
                self,
                #[weak]
                filter,
                move |name_box| {
                    // Clear selection when changing search term.
                    // This is to ensure we never land in a state in which
                    // the selected item is hidden by the current search
                    // term.
                    let term = name_box.text();
                    if !term.is_empty() {
                        filter.set_search(Some(&name_box.text()));
                    } else {
                        filter.set_search(None);
                    }
                    this.on_changed(true);
                }
            ));

            sel_model.connect_selection_changed(clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _, _| {
                    this.on_changed(false);
                }
            ));

            let _ = self.search_model.set(search_model);
            let _ = self.sel_model.set(sel_model);

            self.add_btn.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    // Close the popover
                    this.menu_btn.set_active(false);
                    // If the playlist does not exist yet, MPD automatically
                    // creates one.
                    // As such, as long as we don't pass SaveMode::Replace,
                    // the logic should still be correct.
                    if let (Some(name), Some(song_sel_model)) =
                        (this.get_name(), this.song_sel_model.upgrade())
                    {
                        // Get songs
                        let sel = &song_sel_model.selection();
                        let store = &song_sel_model.model().unwrap();
                        let mut songs: Vec<Song>;
                        if let Some((iter, first_idx)) = gtk::BitsetIter::init_first(sel) {
                            songs = Vec::with_capacity(sel.size() as usize);
                            songs.push(store.item(first_idx).and_downcast::<Song>().unwrap());
                            iter.for_each(|idx| {
                                songs.push(store.item(idx).and_downcast::<Song>().unwrap())
                            });
                        } else {
                            let model = song_sel_model.model().unwrap();
                            let n_items = model.n_items();
                            songs = Vec::with_capacity(n_items as usize);
                            // Default to pushing all songs, skipping selection model bitset
                            for idx in 0..model.n_items() {
                                songs.push(model.item(idx).and_downcast::<Song>().unwrap());
                            }
                        }
                        let _ = this.library.upgrade().unwrap().add_songs_to_playlist(
                            name,
                            &songs,
                            SaveMode::Append,
                        );
                    }
                }
            ));
        }
    }

    impl WidgetImpl for AddToPlaylistButton {}

    impl BoxImpl for AddToPlaylistButton {}

    impl AddToPlaylistButton {
        fn get_name(&self) -> Option<String> {
            let search_model = self.search_model.get().unwrap();
            let sel_model = self.sel_model.get().unwrap();

            let name_box_text = self.name_box.text();
            if self.will_create.get() {
                if name_box_text.is_empty() {
                    None
                } else {
                    Some(name_box_text.to_string())
                }
            } else {
                // Never use name box in this case
                if let Some(item) = sel_model.selected_item() {
                    Some(item.downcast_ref::<INode>()?.get_uri().to_owned())
                } else if let Some(item) = search_model.item(0) {
                    // Nothing is selected either, we'll have to take the first item
                    // in the list. At this point it should also be the only item left.
                    Some(item.downcast_ref::<INode>()?.get_uri().to_owned())
                } else {
                    None
                }
            }
        }

        pub fn on_changed(&self, clear_sel: bool) {
            let search_model = self.search_model.get().unwrap();
            let sel_model = self.sel_model.get().unwrap();
            let name_box = self.name_box.get();
            if clear_sel {
                sel_model.unselect_all();
            }
            // Update mode
            let will_create: bool;
            let n_items = search_model.n_items();
            if n_items > 1 && sel_model.selected_item().is_none() {
                will_create = true;
            } else if n_items == 1 && sel_model.selected_item().is_none() {
                will_create = search_model
                    .item(0)
                    .unwrap()
                    .downcast_ref::<INode>()
                    .unwrap()
                    .get_uri()
                    != name_box.text().as_str();
            } else {
                will_create = n_items == 0;
            }
            self.will_create.replace(will_create);

            // Update button text
            // TODO: translatable
            let maybe_name = self.get_name();
            if let Some(name) = maybe_name {
                self.add_btn.set_sensitive(true);
                self.add_revealer.set_reveal_child(true);

                if will_create {
                    // Name might be empty in this case but it's okay since we would
                    // have hidden the button
                    self.add_btn
                        .set_label(format!("Create \"{name}\"").as_str());
                } else {
                    self.add_btn
                        .set_label(format!("Append to \"{name}\"").as_str());
                }
            } else {
                self.add_btn.set_sensitive(false);
                self.add_revealer.set_reveal_child(false);
            }
        }
    }
}

glib::wrapper! {
    pub struct AddToPlaylistButton(ObjectSubclass<imp::AddToPlaylistButton>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for AddToPlaylistButton {
    fn default() -> Self {
        Self::new()
    }
}

impl AddToPlaylistButton {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn setup(&self, library: &Library, song_sel_model: &gtk::MultiSelection) {
        let playlists = library.playlists();
        self.imp().library.set(Some(library));
        self.imp()
            .search_model
            .get()
            .unwrap()
            .set_model(Some(&playlists));
        self.imp().song_sel_model.set(Some(song_sel_model));

        playlists.connect_items_changed(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _, _, _| {
                // Clear selection when playlist model changes.
                // This is to ensure we never land in a state in which
                // the selected item no longer exists after updating
                // the model.
                this.imp().on_changed(true);
            }
        ));
    }
}
