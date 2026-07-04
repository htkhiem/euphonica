use adw::prelude::*;
use gtk::{
    CompositeTemplate, gio,
    glib::{self, Object, Properties, WeakRef, clone},
    subclass::prelude::*,
};
use quick_xml::escape::escape;
use rustc_hash::FxHashSet;
use std::cell::{OnceCell, RefCell};

use crate::window::EuphonicaWindow;

use super::Tag;

mod imp {
    use super::*;
    use adw::prelude::AdwDialogExt;

    #[derive(Default, CompositeTemplate, Properties)]
    #[properties(wrapper_type = super::TagsFilter)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/tags-filter.ui")]
    pub struct TagsFilter {
        #[template_child]
        pub dialog: TemplateChild<adw::Dialog>,
        #[template_child]
        pub reset_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub apply_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub search: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub list: TemplateChild<gtk::ListBox>, // adw::WrapBox doesn't support model binding :(
        #[template_child]
        pub text_widget: TemplateChild<gtk::Label>,
        #[template_child]
        pub count: TemplateChild<gtk::Label>,
        #[template_child]
        pub toggle_btn: TemplateChild<gtk::ToggleButton>,
        pub on_apply: OnceCell<Box<dyn Fn(Vec<String>) + 'static>>,
        pub search_model: OnceCell<gtk::FilterListModel>,
        pub selected_filter: OnceCell<gtk::FilterListModel>,
        pub selected: RefCell<FxHashSet<String>>,
        #[property(get, set)]
        pub label_text: RefCell<String>, // pub prev_selected: RefCell<Option<FxHashSet<String>>>,  // might want to restore prev selection upon cancel
        pub window: WeakRef<EuphonicaWindow>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TagsFilter {
        const NAME: &'static str = "EuphonicaTagsFilter";
        type Type = super::TagsFilter;
        type ParentType = gtk::Button; // opens dialog

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for TagsFilter {
        fn constructed(&self) {
            self.parent_constructed();

            let dialog = self.dialog.get();
            self.obj().connect_clicked(move |this| {
                dialog.present(this.imp().window.upgrade().as_ref());
            });

            let filter = gtk::StringFilter::builder()
                .expression(gtk::PropertyExpression::new(
                    gtk::StringObject::static_type(),
                    Option::<gtk::PropertyExpression>::None,
                    "string",
                ))
                .match_mode(gtk::StringFilterMatchMode::Substring)
                .build();

            self.search.connect_search_changed(clone!(
                #[weak]
                filter,
                move |search_box| {
                    let term = search_box.text();
                    if !term.is_empty() {
                        filter.set_search(Some(&term));
                    } else {
                        filter.set_search(None);
                    }
                }
            ));

            // Model will be given on setup()
            let search_model = gtk::FilterListModel::builder()
                .incremental(true)
                .filter(&filter)
                .build();

            self.search_model
                .set(search_model)
                .expect("Unable to set search model for genre filter dialog");

            
            let toggle_btn = self.toggle_btn.get();
            let selected_filter = gtk::CustomFilter::new(|_| true); // off by default
            toggle_btn.connect_toggled(clone!(
                #[weak(rename_to = this)]
                self,
                #[weak]
                selected_filter,
                move |btn| {
                    if btn.is_active() {
                        selected_filter.set_filter_func(clone!(
                            #[weak]
                            this,
                            #[upgrade_or]
                            true,
                            move |obj| {
                                obj.downcast_ref::<Tag>()
                                    .is_some_and(|s| this.selected.borrow().contains(s.name()))
                            }
                        ));
                        selected_filter.changed(gtk::FilterChange::MoreStrict);
                    } else {
                        selected_filter.set_filter_func(|_| true);
                        selected_filter.changed(gtk::FilterChange::LessStrict);
                    }
                }
            ));

            self.selected_filter
                .set(
                    gtk::FilterListModel::builder()
                        .incremental(true)
                        .filter(&selected_filter)
                        .build(),
                )
                .expect("Unable to set selected filter");

            self.obj()
                .bind_property("label-text", &self.text_widget.get(), "label")
                .sync_create()
                .build();
        }
    }

    impl WidgetImpl for TagsFilter {}

    impl ButtonImpl for TagsFilter {}
}

glib::wrapper! {
    pub struct TagsFilter(ObjectSubclass<imp::TagsFilter>)
    @extends gtk::Button, gtk::Widget,
    @implements gtk::Accessible, gtk::Actionable, gtk::Buildable, gtk::ConstraintTarget;
}

impl TagsFilter {
    pub fn new() -> Self {
        Object::builder().build()
    }

    pub fn setup<F: Fn(Vec<String>) + 'static + Clone>(
        &self,
        model: &gio::ListStore,  // of tag::Tag objects
        on_selection_changed: F,
        window: &EuphonicaWindow,
    ) {
        let _ = self.imp().window.set(Some(window));
        let search_model = self.imp().search_model.get().unwrap();
        search_model.set_model(Some(model));
        let selected_filter = self.imp().selected_filter.get().unwrap();
        selected_filter.set_model(Some(search_model));
        let list = self.imp().list.get();
        list.bind_model(
            Some(selected_filter),
            clone!(
                #[weak(rename_to = this)]
                self,
                #[upgrade_or]
                adw::ActionRow::new().into(),
                move |obj| {
                    let tag = obj
                        .downcast_ref::<Tag>()
                        .unwrap();
                    let name = tag.name().to_owned();
                    let check = gtk::CheckButton::new();
                    {
                        check.set_active(this.imp().selected.borrow().get(&name).is_some());
                    }
                    check.connect_toggled(clone!(
                        #[weak]
                        this,
                        #[strong]
                        name,
                        move |btn| {
                            if btn.is_active() {
                                this.imp().selected.borrow_mut().insert(name.clone());
                            } else {
                                this.imp().selected.borrow_mut().remove(&name);
                            };
                        }
                    ));
                    let row_builder = adw::ActionRow::builder()
                        .activatable_widget(&check)
                        .use_markup(false)
                        .title_lines(0)
                        .title(escape(&name));
                        
                    let count = tag.count();
                    let row = if count > 1 {
                        row_builder
                            // TODO: translatable
                            .subtitle(&format!("{} occurrences", count))
                            .build()
                    } else {
                        row_builder.build()
                    };

                    row.add_suffix(&check);
                    row.into()
                }
            ),
        );

        self.imp().apply_btn.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            #[strong]
            on_selection_changed,
            move |_| {
                let selected = this
                    .imp()
                    .selected
                    .borrow()
                    .iter()
                    .map(|s| s.to_owned())
                    .collect();
                let len = this.imp().selected.borrow().len();
                let count_label = this.imp().count.get();
                count_label.set_label(len.to_string().as_str());
                count_label.set_visible(len > 0);
                this.imp().dialog.close();
                on_selection_changed(selected);
            }
        ));

        self.imp().reset_btn.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                this.imp().selected.borrow_mut().clear();
                // Make everything visible first
                this.imp().search.set_text("");
                // Then iterate over them
                let mut idx: i32 = 0;
                let list = this.imp().list.get();
                loop {
                    if let Some(tag) = list.row_at_index(idx) {
                        tag
                            .downcast_ref::<adw::ActionRow>()
                            .unwrap()
                            .activatable_widget()
                            .and_downcast::<gtk::CheckButton>()
                            .unwrap()
                            .set_active(false);
                        idx += 1;
                    } else {
                        break;
                    }
                }
            }
        ));
    }
}
