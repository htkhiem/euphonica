use adw::prelude::*;
use adw::subclass::prelude::*;
use ashpd::desktop::file_chooser::SelectedFiles;
use glib::{WeakRef, clone, closure_local};
use gtk::{CompositeTemplate, ListItem, SignalListItemFactory, gio, glib};
use std::{
    cell::{OnceCell, RefCell},
    rc::Rc,
};
use uuid::Uuid;

use derivative::Derivative;
use strum::{EnumCount, IntoEnumIterator, VariantArray};

use crate::{
    cache::{Cache, ImageAction, placeholders::ALBUMART_PLACEHOLDER, sqlite},
    client::ClientState,
    common::{
        DynamicPlaylist, Song, SongRow,
        dynamic_playlist::{AutoRefresh, Ordering, Rule},
    },
    utils::{format_secs_as_duration, tokio_runtime},
    window::EuphonicaWindow,
};

use super::{Library, ordering_button::OrderingButton, rule_button::RuleButton};

mod imp {
    use std::{cell::Cell, sync::OnceLock};

    use glib::subclass::Signal;

    use super::*;

    #[derive(Debug, CompositeTemplate, Derivative)]
    #[derivative(Default)]
    #[template(
        resource = "/io/github/htkhiem/Euphonica/gtk/library/dynamic-playlist-editor-view.ui"
    )]
    pub struct DynamicPlaylistEditorView {
        #[template_child]
        pub overwrite_dialog: TemplateChild<adw::AlertDialog>,
        #[template_child]
        pub unsaved_dialog: TemplateChild<adw::AlertDialog>,
        #[template_child]
        pub edit_cover_dialog: TemplateChild<adw::AlertDialog>,
        #[template_child]
        pub infobox_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub collapse_infobox: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub cover_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub cover: TemplateChild<gtk::Image>,

        #[template_child]
        pub exit_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub save_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub save_btn_content: TemplateChild<gtk::Stack>,

        #[template_child]
        pub title: TemplateChild<gtk::Entry>,
        #[template_child]
        pub add_rule_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub rules_box: TemplateChild<adw::WrapBox>,
        pub rules_model: OnceCell<gio::ListModel>,
        #[template_child]
        pub add_ordering_btn: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub ordering_box: TemplateChild<adw::WrapBox>,
        pub orderings_model: OnceCell<gio::ListModel>,

        #[template_child]
        pub refresh_schedule: TemplateChild<gtk::DropDown>,

        #[template_child]
        pub limit_mode: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub limit: TemplateChild<gtk::SpinButton>,
        #[template_child]
        pub limit_unit: TemplateChild<gtk::Label>,

        #[template_child]
        pub track_count: TemplateChild<gtk::Label>,
        #[template_child]
        pub runtime: TemplateChild<gtk::Label>,
        #[template_child]
        pub refresh_btn: TemplateChild<gtk::Button>,

        #[template_child]
        pub content_pages: TemplateChild<gtk::Stack>,
        #[template_child]
        pub content: TemplateChild<gtk::ListView>,
        #[derivative(Default(value = "gio::ListStore::new::<Song>()"))]
        pub song_list: gio::ListStore,
        pub orderings_menu: gio::Menu,
        pub cover_action: RefCell<ImageAction>,
        pub rules_valid: Cell<bool>,
        pub title_valid: Cell<bool>,
        pub unsaved: Cell<bool>,
        pub editing_name: RefCell<Option<String>>, // If not None, will be in edit mode.

        pub library: WeakRef<Library>,
        pub cache: OnceCell<Rc<Cache>>,
        #[derivative(Default(value = "RefCell::new(String::from(\"\"))"))]
        pub tmp_name: RefCell<String>,
        pub window: WeakRef<EuphonicaWindow>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DynamicPlaylistEditorView {
        const NAME: &'static str = "EuphonicaDynamicPlaylistEditorView";
        type Type = super::DynamicPlaylistEditorView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);

            klass.set_layout_manager_type::<gtk::BinLayout>();
            // klass.set_css_name("albumview");
            klass.set_accessible_role(gtk::AccessibleRole::Group);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for DynamicPlaylistEditorView {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
            println!("Disposing DynamicPlaylistEditorView");
        }

        fn constructed(&self) {
            self.parent_constructed();

            // self.cover.set_paintable(Some(&*ALBUMART_PLACEHOLDER));

            self.title.connect_changed(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.obj().validate_title();
                    this.obj().on_change();
                }
            ));

            self.refresh_schedule.connect_selected_notify(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.obj().on_change();
                }
            ));

            self.limit_mode.connect_selected_notify(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.obj().on_change();
                }
            ));

            self.limit.connect_changed(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.obj().on_change();
                }
            ));

            // TODO: find another way as observe_children() is very inefficient
            let obj = self.obj();
            let rules_model = self.rules_box.observe_children();
            rules_model.connect_items_changed(clone!(
                #[weak]
                obj,
                move |_, _, _, _| {
                    obj.validate_rules();
                    obj.on_change();
                }
            ));
            let _ = self.rules_model.set(rules_model);
            obj.validate_rules();
            self.add_rule_btn.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |add_btn| {
                    let rules_box = this.rules_box.get();
                    let btn = RuleButton::new(&rules_box);
                    rules_box.append(&btn);
                    rules_box.reorder_child_after(add_btn, Some(&btn));
                    // Validate once at creation
                    btn.validate();
                    btn.connect_notify_local(
                        Some("is-valid"),
                        clone!(
                            #[weak]
                            this,
                            move |_, _| {
                                this.obj().validate_rules();
                                this.obj().on_change();
                            }
                        ),
                    );
                }
            ));

            // TODO: find another way as observe_children() is very inefficient
            let orderings_model = self.ordering_box.observe_children();
            orderings_model.connect_items_changed(clone!(
                #[weak]
                obj,
                move |_, _, _, _| {
                    obj.on_ordering_changed();
                    obj.on_change();
                }
            ));
            let _ = self.orderings_model.set(orderings_model);

            self.song_list
                .bind_property("n-items", &self.track_count.get(), "label")
                .transform_to(|_, n_items: u32| Some(n_items.to_string()))
                .sync_create()
                .build();

            let action_add_ordering = gio::ActionEntry::builder("add-ordering")
                .parameter_type(Some(glib::VariantTy::UINT32))
                .activate(clone!(
                    #[weak]
                    obj,
                    move |_, _, idx: Option<&glib::Variant>| {
                        let ordering_box = obj.imp().ordering_box.get();
                        let idx = idx.unwrap().get::<u32>().unwrap();
                        let ordering = Ordering::VARIANTS[idx as usize];
                        let btn = OrderingButton::new(ordering, &ordering_box);
                        ordering_box.append(&btn);
                        let add_ordering_btn = obj.imp().add_ordering_btn.get();
                        ordering_box.reorder_child_after(&add_ordering_btn, Some(&btn));
                    }
                ))
                .build();

            // Create a new action group and add actions to it
            let actions = gio::SimpleActionGroup::new();
            actions.add_action_entries([action_add_ordering]);
            obj.insert_action_group("dynamic-playlist-editor-view", Some(&actions));
            // Once the actions are in place we can construct the menu items
            // Call once to init ordering options menu
            obj.on_ordering_changed();
            self.add_ordering_btn
                .set_menu_model(Some(&self.orderings_menu));

            self.limit_mode
                .bind_property("selected", &self.limit.get(), "visible")
                .transform_to(|_, idx: u32| Some(idx > 0))
                .sync_create()
                .build();

            self.limit_mode
                .bind_property("selected", &self.limit_unit.get(), "visible")
                .transform_to(|_, idx: u32| Some(idx > 0))
                .sync_create()
                .build();

            self.refresh_btn.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.obj().preview_result();
                }
            ));

            self.exit_btn.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.obj().exit(false);
                }
            ));

            self.save_btn.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.obj().on_save_btn_clicked();
                }
            ));

            let infobox_revealer = self.infobox_revealer.get();
            let collapse_infobox = self.collapse_infobox.get();
            collapse_infobox
                .bind_property("active", &infobox_revealer, "reveal-child")
                .transform_to(|_, active: bool| Some(!active))
                .transform_from(|_, active: bool| Some(!active))
                .bidirectional()
                .sync_create()
                .build();

            infobox_revealer
                .bind_property("child-revealed", &collapse_infobox, "icon-name")
                .transform_to(|_, revealed| {
                    if revealed {
                        return Some("up-symbolic");
                    }
                    Some("down-symbolic")
                })
                .sync_create()
                .build();

            // Finally, update sensitivity of buttons
            self.obj().update_sensitivity();
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("exit-clicked")
                        .param_types([
                            bool::static_type(), // if true, indicates saved changes & should reset list & content views
                        ])
                        .build(),
                ]
            })
        }
    }

    impl WidgetImpl for DynamicPlaylistEditorView {}
}

glib::wrapper! {
    pub struct DynamicPlaylistEditorView(ObjectSubclass<imp::DynamicPlaylistEditorView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gio::ActionGroup, gio::ActionMap;
}

impl Default for DynamicPlaylistEditorView {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl DynamicPlaylistEditorView {
    fn get_library(&self) -> Option<Library> {
        self.imp().library.upgrade()
    }

    fn get_cache(&self) -> Option<&Rc<Cache>> {
        self.imp().cache.get()
    }

    fn open_cover_file_dialog(&self) {
        let (sender, receiver) = oneshot::channel();
        tokio_runtime().spawn(async move {
            sender.send(match SelectedFiles::open_file()
                .title("Select a new cover image")
                .modal(true)
                .multiple(false)
                .send()
                .await
                .expect("ashpd file open await failure")
                .response()
            {
                Ok(files) => {
                    let uris = files.uris();
                    if !uris.is_empty() {
                        Some(uris[0].to_string())
                    } else {
                        None
                    }
                }
                Err(e) => {
                    dbg!(e);
                    None
                }
            });
        });
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            (),
            async move {
                if let Some(path) = receiver.await.expect("Broken oneshot receiver") {
                    this.set_cover_path(&path);
                }
            }
        ));
    }

    pub fn setup(
        &self,
        library: &Library,
        cache: Rc<Cache>,
        window: &EuphonicaWindow,
    ) {
        self.imp()
            .cache
            .set(cache.clone())
            .expect("DynamicPlaylistEditorView cannot bind to cache");
        self.imp().window.set(Some(window));
        self.imp().library.set(Some(library));

        self.imp().cover_btn.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                let cover_name = this.imp().cover_action.borrow_mut();
                match *cover_name {
                    ImageAction::Unknown | ImageAction::Clear | ImageAction::Existing(false) => {
                        // Open file chooser directly
                        this.open_cover_file_dialog();
                    }
                    _ => {
                        let dialog = this.imp().edit_cover_dialog.get();
                        dialog.choose(
                            &this,
                            Option::<gio::Cancellable>::None.as_ref(),
                            clone!(
                                #[weak]
                                this,
                                move |resp| {
                                    match resp.as_str() {
                                        "replace" => {
                                            this.open_cover_file_dialog();
                                        }
                                        "clear" => {
                                            this.clear_cover();
                                        }
                                        _ => {}
                                    }
                                }
                            ),
                        );
                    }
                }
            }
        ));

        // Set up factory
        let factory = SignalListItemFactory::new();

        factory.connect_setup(clone!(
            #[weak]
            cache,
            move |_, list_item| {
                let item = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem");
                let row = SongRow::new(Some(cache), None);
                item.property_expression("item")
                    .chain_property::<Song>("name")
                    .bind(&row, "name", gtk::Widget::NONE);

                row.set_first_attrib_icon_name(Some("library-music-symbolic"));
                item.property_expression("item")
                    .chain_property::<Song>("album")
                    .bind(&row, "first-attrib-text", gtk::Widget::NONE);

                row.set_second_attrib_icon_name(Some("music-artist-symbolic"));
                item.property_expression("item")
                    .chain_property::<Song>("artist")
                    .bind(&row, "second-attrib-text", gtk::Widget::NONE);

                row.set_third_attrib_icon_name(Some("hourglass-symbolic"));
                item.property_expression("item")
                    .chain_property::<Song>("duration")
                    .chain_closure::<String>(closure_local!(|_: Option<glib::Object>, dur: u64| {
                        format_secs_as_duration(dur as f64)
                    }))
                    .bind(&row, "third-attrib-text", gtk::Widget::NONE);

                item.property_expression("item")
                    .chain_property::<Song>("quality-grade")
                    .bind(&row, "quality-grade", gtk::Widget::NONE);
                // No end-widget for this view
                item.set_child(Some(&row));
            }
        ));
        // Tell factory how to bind `SongRow` to one of our Album GObjects
        factory.connect_bind(move |_, list_item| {
            // Get `Song` from `ListItem` (that is, the data side)
            let item = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .item()
                .and_downcast::<Song>()
                .expect("The item has to be a common::Song.");

            // Get `SongRow` from `ListItem` (the UI widget)
            let child: SongRow = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .child()
                .and_downcast::<SongRow>()
                .expect("The child has to be a `SongRow`.");

            // Within this binding fn is where the cached album art texture gets used.
            child.on_bind(&item);
        });

        // When row goes out of sight, unbind from item to allow reuse with another.
        factory.connect_unbind(move |_, list_item| {
            // Get `SongRow` from `ListItem` (the UI widget)
            let child: SongRow = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .child()
                .and_downcast::<SongRow>()
                .expect("The child has to be a `SongRow`.");
            child.on_unbind();
        });

        // Set the factory of the list view
        self.imp().content.set_factory(Some(&factory));
        self.imp()
            .content
            .set_model(Some(&gtk::NoSelection::new(Some(
                self.imp().song_list.clone(),
            ))));
    }

    fn on_change(&self) {
        self.imp().unsaved.set(true);
        self.update_sensitivity();
    }

    fn validate_title(&self) {
        let entry = self.imp().title.get();
        let is_valid = entry.text_length() > 0;
        let old_valid = self.imp().title_valid.replace(is_valid);

        if !is_valid && !entry.has_css_class("error") {
            entry.add_css_class("error");
        } else if is_valid && entry.has_css_class("error") {
            entry.remove_css_class("error");
        }

        if old_valid != is_valid {
            self.update_sensitivity();
        }
    }

    fn validate_rules(&self) {
        let model = self.imp().rules_model.get().unwrap();
        let n_items = model.n_items();
        // There's always the "add rule" button
        if n_items == 1 {
            // Allow fetching without any filters. Basically just sort the whole library.
            let old_rules_valid = self.imp().rules_valid.replace(true);
            if old_rules_valid {
                self.update_sensitivity();
            }
            return;
        }
        let mut per_rule_is_valid: Vec<bool> = Vec::with_capacity(n_items as usize);
        for i in 0..n_items {
            if let Some(rule_btn) = model.item(i).and_downcast_ref::<RuleButton>() {
                per_rule_is_valid.push(rule_btn.is_valid());
            }
        }
        let new_rules_valid = per_rule_is_valid.iter().all(|item| *item);
        let old_rules_valid = self.imp().rules_valid.replace(new_rules_valid);
        if old_rules_valid != new_rules_valid {
            self.update_sensitivity();
        }
    }

    /// Never allow the ordering section to fall into an invalid state by enforcing
    /// the following rules:
    /// 1. If Random is added, disable the Add Ordering button as any further
    ///    ordering wouldn't make sense. We disable instead of hide to make this clearer.
    /// 2. If there's any non-Random option, disable the Random option in the Add
    ///    Ordering dropdown.
    /// 3. For every non-Random option, disable them in the dropdown too to prevent
    ///    adding duplicate ones.
    fn on_ordering_changed(&self) {
        let orderings = self.imp().orderings_model.get().unwrap();
        let mut presence = [false; Ordering::COUNT];
        let mut has_random = false;
        let mut has_other = false;
        let n_btns = orderings.n_items() as usize;
        for i in 0..n_btns {
            if let Some(ordering_btn) = orderings.item(i as u32).and_downcast::<OrderingButton>() {
                let ordering = ordering_btn.ordering();
                presence[ordering as usize] = true;
                // Some orderings cannot go together, so if one is presence, the other should
                // also be considered to be.
                if let Some(reversed) = ordering.reverse() {
                    presence[reversed as usize] = true;
                }
                if ordering == Ordering::Random {
                    has_random = true;
                } else {
                    has_other = true;
                }
            }
        }

        // If there's Random, disable Add Ordering button
        self.imp().add_ordering_btn.set_sensitive(!has_random);

        // Get remaining ordering options
        if !has_random {
            let orderings_menu = &self.imp().orderings_menu;
            orderings_menu.remove_all();
            for opt in Ordering::iter() {
                let idx = opt as usize;
                // Random option only makes sense when no other Ordering has been specified
                if !presence[idx] && (opt != Ordering::Random || !has_other) {
                    let item = gio::MenuItem::new(Some(opt.readable_name()), None);
                    item.set_action_and_target_value(
                        Some("dynamic-playlist-editor-view.add-ordering"),
                        Some(&(idx as u32).to_variant()),
                    );
                    orderings_menu.append_item(&item);
                }
            }
        }
    }

    fn update_sensitivity(&self) {
        let rules_valid = self.imp().rules_valid.get();
        let title_valid = self.imp().title_valid.get();
        let unsaved = self.imp().unsaved.get();
        self.imp()
            .save_btn
            .set_sensitive(rules_valid && title_valid && unsaved);
        self.imp().refresh_btn.set_sensitive(rules_valid);
    }

    /// Set a user-selected path as the new local cover.
    pub fn set_cover_path(&self, path: &str) {
        // Assume ashpd always return filesystem spec
        let filepath = urlencoding::decode(if path.starts_with("file://") {
            &path[7..]
        } else {
            path
        })
        .expect("Path must be in UTF-8")
        .into_owned();
        self.imp()
            .cover_action
            .replace(ImageAction::New(filepath.clone()));
        self.imp().cover.set_from_file(Some(filepath)); // FIXME: use libglycin
        self.on_change();
    }

    fn clear_cover(&self) {
        self.imp().cover_action.replace(ImageAction::Clear);
        self.imp().cover.set_paintable(Some(&*ALBUMART_PLACEHOLDER));
        self.on_change();
    }

    async fn schedule_existing_cover(&self, dp: &DynamicPlaylist) {
        self.imp()
            .cover_action
            .replace(ImageAction::Existing(false)); // for now
        self.imp().cover.set_paintable(Some(&*ALBUMART_PLACEHOLDER));

        // Fetch high resolution playlist cover
        match self.imp().cache.get().unwrap().get_playlist_cover(dp.name.to_owned(), true, false).await {
            Ok(Some(tex)) => {
                self.imp().cover_action.replace(ImageAction::Existing(true));
                self.imp().cover.set_paintable(Some(&tex));
            }
            Ok(None) => {}
            Err(e) => {dbg!(e);}
        }
    }

    fn get_refresh_schedule(&self) -> AutoRefresh {
        // TODO: gettext
        match self
            .imp()
            .refresh_schedule
            .model()
            .and_downcast::<gtk::StringList>()
            .unwrap()
            .string(self.imp().refresh_schedule.selected())
            .unwrap()
            .as_str()
        {
            "No auto-refresh" => AutoRefresh::None,
            "Refresh hourly" => AutoRefresh::Hourly,
            "Refresh daily" => AutoRefresh::Daily,
            "Refresh weekly" => AutoRefresh::Weekly,
            "Refresh monthly" => AutoRefresh::Monthly,
            "Refresh yearly" => AutoRefresh::Yearly,
            _ => unimplemented!(),
        }
    }

    fn set_refresh_schedule(&self, auto_refresh: AutoRefresh) {
        self.imp()
            .refresh_schedule
            .set_selected(match auto_refresh {
                AutoRefresh::None => 0,
                AutoRefresh::Hourly => 1,
                AutoRefresh::Daily => 2,
                AutoRefresh::Weekly => 3,
                AutoRefresh::Monthly => 4,
                AutoRefresh::Yearly => 5,
            });
    }

    fn preview_result(&self) {
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)] self, async move {
                this.imp().refresh_btn.set_sensitive(false);
                this.imp().content_pages.set_visible_child_name("spinner");
                this.imp().song_list.remove_all();

                // Build a test DP instance with a random name to avoid confusion w/ late-coming
                // background fetches of actual DPs.
                let mut dp = this.build_dynamic_playlist();
                let tmp_name = Uuid::new_v4().simple().to_string();
                dp.name = tmp_name.clone();
                this.imp().tmp_name.replace(tmp_name);

                println!("{:?}", &dp);

                // Don't cache as this DP is still being edited
                match this.get_library()
                    .unwrap()
                    .get_dynamic_playlist_songs(dp, false).await
                {
                    Ok(songs) => {
                        this.imp().song_list.extend_from_slice(&songs);
                        this.imp().content_pages.set_visible_child_name(
                            if songs.len() > 0 {"content"} else {"empty"}
                        );
                    }
                    Err(e) => {dbg!(e);}
                };
                this.imp().refresh_btn.set_sensitive(true);
            }
        ));
    }

    fn build_dynamic_playlist(&self) -> DynamicPlaylist {
        let rules_model = self.imp().rules_model.get().unwrap();
        let n_rules = rules_model.n_items() as usize;
        let mut rules: Vec<Rule> = Vec::with_capacity(n_rules - 1); // Except the "Add Rule" button
        for i in 0..n_rules {
            if let Some(rule_btn) = rules_model.item(i as u32).and_downcast_ref::<RuleButton>() {
                rules.push(rule_btn.get_rule().unwrap());
            }
        }

        let orderings_model = self.imp().orderings_model.get().unwrap();
        let n_orderings = orderings_model.n_items() as usize;
        let mut ordering: Vec<Ordering> = Vec::with_capacity(n_orderings - 1); // Except the "Add Rule" button
        for i in 0..n_orderings {
            if let Some(ordering_btn) = orderings_model
                .item(i as u32)
                .and_downcast::<OrderingButton>()
            {
                ordering.push(ordering_btn.ordering());
            }
        }
        let limit: Option<u32> = if self.imp().limit_mode.selected() > 0 {
            Some(self.imp().limit.adjustment().value().round() as u32)
        } else {
            None
        };

        DynamicPlaylist {
            name: self.imp().title.text().to_string(),
            last_queued: None,
            play_count: 0,
            rules,
            ordering,
            auto_refresh: self.get_refresh_schedule(),
            last_refresh: None,
            limit,
        }
    }

    fn finalize_save(&self) {
        let cover_action;
        {
            // Clone then end borrow outside of async scope to avoid multiple borrows
            cover_action = self.imp().cover_action.borrow().to_owned();
        }
        // Always overwrite, as we'd only reach here after user confirmation
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let dp = this.build_dynamic_playlist();
                let res = this.imp().cache.get().unwrap().insert_dynamic_playlist(
                    dp,
                    cover_action,
                    Some(
                        this.imp()
                            .editing_name
                            .borrow()
                            .as_deref()
                            .unwrap_or(this.imp().title.text().as_str())
                            .to_string(),
                    ),
                ).await;
                let stack = this.imp().save_btn_content.get();
                if stack.visible_child_name().unwrap() != "label" {
                    stack.set_visible_child_name("label");
                }
                match res {
                    Ok(()) => {
                        this.imp().unsaved.set(false);
                        this.exit(true);
                    }
                    Err(e) => {
                        dbg!(e);
                        this.imp().unsaved.set(true);
                        this.update_sensitivity();
                    }
                }
            }
        ));
    }

    // Overwrite parameter is not applicable when editing an existing playlist.
    fn on_save_btn_clicked(&self) {
        let btn = self.imp().save_btn.get();
        let stack = self.imp().save_btn_content.get();
        btn.set_sensitive(false);
        if stack.visible_child_name().unwrap() != "spinner" {
            stack.set_visible_child_name("spinner");
        }
        let editing: bool;
        {
            editing = self.imp().editing_name.borrow().is_some();
        }
        if !editing
            && sqlite::exists_dynamic_playlist(self.imp().title.text().as_str()).unwrap_or(false)
        {
            // If creating a new playlist, ask for confirmation before overwriting
            let dialog = self.imp().overwrite_dialog.get();
            dialog.choose(
                self,
                Option::<gio::Cancellable>::None.as_ref(),
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |resp| {
                        if resp == "overwrite" {
                            this.finalize_save();
                        }
                    }
                ),
            );
        } else {
            // Just overwrite if editing an existing playlist
            self.finalize_save();
        }
    }

    fn exit(&self, should_refresh: bool) {
        if self.imp().unsaved.get() {
            let dialog = self.imp().unsaved_dialog.get();
            dialog.choose(
                self,
                Option::<gio::Cancellable>::None.as_ref(),
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |resp| {
                        match resp.as_str() {
                            "save" => {
                                this.on_save_btn_clicked();
                            }
                            "discard" => {
                                this.emit_by_name::<()>("exit_clicked", &[&false.to_value()]);
                            }
                            _ => {}
                        }
                    }
                ),
            );
        } else {
            self.emit_by_name::<()>("exit_clicked", &[&should_refresh.to_value()]);
        }
    }

    pub fn init(&self, dp: DynamicPlaylist) {
        let imp = self.imp();
        // Set editor into edit mode and not "create new"
        imp.editing_name.replace(Some(dp.name.clone()));
        imp.title.set_text(&dp.name);

        // Init cover
        self.schedule_existing_cover(&dp);

        // Init rules
        let rules_box = imp.rules_box.get();
        let add_btn = imp.add_rule_btn.get();
        let mut last_btn: Option<RuleButton> = None;
        for rule in dp.rules.into_iter() {
            let btn = RuleButton::from_rule(rule, &rules_box);
            rules_box.append(&btn);
            // Validate once at creation
            btn.validate();
            btn.connect_notify_local(
                Some("is-valid"),
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |_, _| {
                        this.validate_rules();
                        this.on_change();
                    }
                ),
            );
            last_btn.replace(btn);
        }
        if let Some(last_btn) = last_btn {
            rules_box.reorder_child_after(&add_btn, Some(&last_btn));
        }

        // Init orderings
        let ordering_box = imp.ordering_box.get();
        let add_btn = imp.add_ordering_btn.get();
        let mut last_btn: Option<OrderingButton> = None;
        for ordering in dp.ordering.into_iter() {
            let btn = OrderingButton::new(ordering, &ordering_box);
            ordering_box.append(&btn);
            last_btn.replace(btn);
        }
        if let Some(last_btn) = last_btn {
            ordering_box.reorder_child_after(&add_btn, Some(&last_btn));
        }

        self.validate_rules();
        self.on_ordering_changed();

        // Init the other settings
        self.set_refresh_schedule(dp.auto_refresh);
        if let Some(limit) = dp.limit {
            imp.limit_mode.set_selected(1);
            imp.limit.set_value(limit as f64);
        }

        self.preview_result();
        self.imp().unsaved.set(false);
    }

    fn add_songs(&self, songs: &[Song]) {
        // If this is called with an empty list, it indicates the queried DP is empty.
        if songs.is_empty() {
            println!("dynamic_playlist_editor_view: end of response received");
            if self
                .imp()
                .content_pages
                .visible_child_name()
                .is_some_and(|name| name.as_str() != "empty")
                && self.imp().song_list.n_items() == 0
            {
                self.imp().content_pages.set_visible_child_name("empty");
            }
            self.imp().refresh_btn.set_sensitive(true);
        } else {
            self.imp().content_pages.set_visible_child_name("content");
            self.imp().song_list.extend_from_slice(songs);
        }

        self.imp().runtime.set_label(&format_secs_as_duration(
            self.imp()
                .song_list
                .iter()
                .map(|item: Result<Song, _>| {
                    if let Ok(song) = item {
                        return song.get_duration();
                    }
                    0
                })
                .sum::<u64>() as f64,
        ));
    }

    pub fn get_current_name(&self) -> String {
        self.imp().title.text().to_string()
    }
}
