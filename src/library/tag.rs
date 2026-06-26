use gtk::{
    CompositeTemplate, gio,
    glib::{self, Object, ParamSpec, ParamSpecString, clone},
    prelude::*,
    subclass::prelude::*,
};
use std::cell::{OnceCell, Cell};

use crate::window::EuphonicaWindow;
use once_cell::sync::Lazy;

mod imp {
    use super::*;
    
    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/tag.ui")]
    pub struct Tag {
        #[template_child]
        pub name: TemplateChild<gtk::Label>,
        #[template_child]
        pub count: TemplateChild<gtk::Label>,
        #[template_child]
        pub tag_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub remove_btn: TemplateChild<gtk::Button>,
        pub link: OnceCell<String>,
        pub set_by_user: Cell<bool>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for Tag {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaTag";
        type Type = super::Tag;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for Tag {
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> =
                Lazy::new(|| vec![ParamSpecString::builder("name").build()]);
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            let obj = self.obj();
            match pspec.name() {
                "name" => obj.get_name().to_value(),
                _ => unimplemented!(),
            }
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &ParamSpec) {
            let obj = self.obj();
            match pspec.name() {
                "name" => {
                    if let Ok(name) = value.get::<&str>() {
                        obj.set_name(name);
                        obj.notify("name");
                    }
                }
                _ => unimplemented!(),
            }
        }
    }

    impl WidgetImpl for Tag {}

    impl BoxImpl for Tag {}
}

glib::wrapper! {
    pub struct Tag(ObjectSubclass<imp::Tag>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Tag {
    pub fn new<T: Fn(&Self) + 'static>(
        name: &str,
        link: Option<String>,
        count: Option<i32>,
        is_removable: bool,
        wrap_box: &adw::WrapBox,
        window: &EuphonicaWindow,
        set_by_user: bool,
        on_remove: T,
    ) -> Self {
        let res: Self = Object::builder().build();
        res.imp().name.set_label(name);
        res.imp().set_by_user.set(set_by_user);
        if is_removable {
            res.imp().remove_btn.connect_clicked(clone!(
                #[weak]
                wrap_box,
                #[weak]
                res,
                move |_| {
                    wrap_box.remove(&res);
                    on_remove(&res);
                }
            ));

            res.imp().remove_btn.set_visible(true);
        }
        if let Some(link) = link {
            res.imp().tag_btn.set_tooltip_text(Some(&link));
            res.imp().tag_btn.connect_clicked(clone!(
                #[weak]
                window,
                move |_| {
                    let launcher = gtk::FileLauncher::new(Some(&gio::File::for_uri(&link)));
                    launcher.launch(Some(&window), gio::Cancellable::NONE, |_| {});
                }
            ));
        }
        if let Some(count) = count {
            res.imp().count.set_label(&count.to_string());
            res.imp().count.set_visible(true);
        }
        res
    }

    pub fn get_name(&self) -> glib::GString {
        self.imp().name.label()
    }

    pub fn set_name(&self, name: &str) {
        self.imp().name.set_label(name);
    }

    pub fn get_count(&self) -> Option<i32> {
        self.imp().count.label().as_str().parse::<i32>().ok()
    }

    pub fn get_link(&self) -> Option<&str> {
        self.imp().link.get().map(String::as_str)
    }

    pub fn get_set_by_user(&self) -> bool {
        self.imp().set_by_user.get()
    }
}
