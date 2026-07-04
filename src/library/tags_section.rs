use gtk::{
    CompositeTemplate,
    glib::{self, Object, clone},
    prelude::*,
    subclass::prelude::*,
};
use std::cell::OnceCell;

use super::tag_button::TagButton;
use crate::common::{ContentStack, FadingScrolledWindow};
use crate::meta_providers::models::Tag as TagMeta;
use crate::window::EuphonicaWindow;

mod imp {
    use super::*;

   #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/tags-section.ui")]
    pub struct TagsSection {
        #[template_child]
        pub tags_stack: TemplateChild<ContentStack>,
        #[template_child]
        pub add_first_tag_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub tag_entry: TemplateChild<gtk::Entry>,
        #[template_child]
        pub add_tag_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub tags_fader: TemplateChild<FadingScrolledWindow>,
        #[template_child]
        pub tags_scroller: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub tags_box: TemplateChild<adw::WrapBox>,

        pub window: OnceCell<EuphonicaWindow>,
        pub on_tag_added: OnceCell<Box<dyn Fn() + 'static>>,
        pub on_tag_removed: OnceCell<Box<dyn Fn() + 'static>>,
        pub on_add_btn_clicked: OnceCell<Box<dyn Fn() + 'static>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TagsSection {
        const NAME: &'static str = "EuphonicaTagsSection";
        type Type = super::TagsSection;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for TagsSection {
        fn constructed(&self) {
            self.parent_constructed();

            // Entry text change -> toggle add button sensitivity
            let btn = self.add_tag_btn.get();
            self.tag_entry.connect_text_notify(move |entry| {
                btn.set_sensitive(entry.text_length() > 0);
            });

            // Add button click -> add tag from entry
            self.add_tag_btn.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.obj().add_from_entry();
                }
            ));

            // Placeholder add button -> show content
            self.add_first_tag_btn.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    if let Some(cb) = this.on_add_btn_clicked.get() {
                        cb();
                    }
                    this.tags_stack.show_content();
                }
            ));

            // Enter key in entry -> add tag
            self.tag_entry.connect_activate(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.obj().add_from_entry();
                }
            ));
        }
    }

    impl WidgetImpl for TagsSection {}
}

glib::wrapper! {
    pub struct TagsSection(ObjectSubclass<imp::TagsSection>)
    @extends gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl TagsSection {
    pub fn new() -> Self {
        Object::builder().build()
    }

    pub fn set_window(&self, window: &EuphonicaWindow) {
        self.imp()
            .window
            .set(window.clone())
            .unwrap_or_else(|_| panic!("Window already set"));
    }

    /// Set callback called after a tag is added (from UI entry or `add_tag`).
    pub fn set_on_tag_added<T: Fn() + 'static>(&self, cb: T) {
        self.imp()
            .on_tag_added
            .set(Box::new(cb))
            .unwrap_or_else(|_| panic!("Callback already set"));
    }

    /// Set callback called after a tag is removed (by clicking the remove button on a Tag widget).
    pub fn set_on_tag_removed<T: Fn() + 'static>(&self, cb: T) {
        self.imp()
            .on_tag_removed
            .set(Box::new(cb))
            .unwrap_or_else(|_| panic!("Callback already set"));
    }

    /// Set callback called when the placeholder "add first tag" button is clicked.
    pub fn set_on_add_btn_clicked<T: Fn() + 'static>(&self, cb: T) {
        self.imp()
            .on_add_btn_clicked
            .set(Box::new(cb))
            .unwrap_or_else(|_| panic!("Callback already set"));
    }

    /// Programmatically add a tag to the list.
    /// Silently skips if a tag with the same name already exists.
    pub fn add_tag(&self, name: &str, link: Option<String>, count: Option<i32>, set_by_user: bool) {
        // Check for duplicates
        if let Some(first) = self.imp().tags_box.first_child() {
            let mut cursor: TagButton = first.downcast::<TagButton>().unwrap();
            loop {
                if cursor.get_name().as_str() == name {
                    return; // duplicate, skip
                }
                if let Some(next) = cursor.next_sibling().and_downcast::<TagButton>() {
                    cursor = next;
                } else {
                    break;
                }
            }
        }

        let window = self.imp().window.get().unwrap();
        let tags_box = self.imp().tags_box.get();

        let tag = TagButton::new(
            name,
            link.clone(),
            count,
            true,
            &tags_box,
            &window,
            set_by_user,
            clone!(
                #[weak(rename_to = this)]
                self,
                move |tag: &TagButton| {
                    if let Some(cb) = this.imp().on_tag_removed.get() {
                        cb();
                    }
                }
            ),
        );

        self.imp().tags_box.append(&tag);

        // Switch from placeholder to content if needed
        self.imp().tags_stack.show_content();

        // Scroll to bottom
        let adjustment = self.imp().tags_scroller.vadjustment();
        let upper = adjustment.upper();
        let page_size = adjustment.page_size();
        adjustment.set_value(upper - page_size);

        // Clear entry
        self.imp().tag_entry.set_text("");

        if set_by_user {
            if let Some(cb) = self.imp().on_tag_added.get() {
                cb();
            }
        }
    }

    /// Add a tag from the entry widget's current text.
    fn add_from_entry(&self) {
        let name = self.imp().tag_entry.text();
        if name.is_empty() {
            return;
        }
        self.add_tag(name.as_str(), None, None, true);
    }

    /// Remove all tags from the list.
    pub fn remove_all(&self, show_spinner: bool) {
        self.imp().tags_box.remove_all();
        if show_spinner {
            self.imp().tags_stack.show_spinner();
        } else {
            self.imp().tags_stack.show_placeholder();
        }
    }

    /// Build a list of TagMeta structs from the current tag widgets.
    pub fn get_tags(&self) -> Vec<TagMeta> {
        let mut result = Vec::new();
        if let Some(first) = self.imp().tags_box.first_child() {
            let mut cursor: TagButton = first.downcast::<TagButton>().unwrap();
            loop {
                let tag_name = cursor.get_name().as_str().to_owned();
                result.push(TagMeta {
                    url: cursor.get_link().map(|s: &str| s.to_owned()),
                    name: tag_name.clone(),
                    count: cursor.get_count(),
                    set_by_user: cursor.get_set_by_user(),
                });
                if let Some(next) = cursor.next_sibling().and_downcast::<TagButton>() {
                    cursor = next;
                } else {
                    break;
                }
            }
        }
        result
    }

    pub fn show_placeholder(&self) {
        self.imp().tags_stack.show_placeholder();
    }

    pub fn show_content(&self) {
        self.imp().tags_stack.show_content();
    }
}
