use gtk::{
    CompositeTemplate, gio,
    glib::{self, Object, clone},
    prelude::*,
    subclass::prelude::*,
};
use std::cell::OnceCell;

use crate::window::EuphonicaWindow;
use super::Tag;

mod imp {
    use super::*;
    
    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/tag-button.ui")]
    pub struct TagButton {
        #[template_child]
        pub name: TemplateChild<gtk::Label>,
        #[template_child]
        pub count: TemplateChild<gtk::Label>,
        #[template_child]
        pub tag_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub remove_btn: TemplateChild<gtk::Button>,
        pub data: OnceCell<Tag>
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for TagButton {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaTagButton";
        type Type = super::TagButton;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for TagButton {}

    impl WidgetImpl for TagButton {}

    impl BoxImpl for TagButton {}
}

glib::wrapper! {
    pub struct TagButton(ObjectSubclass<imp::TagButton>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl TagButton {
    pub fn new<T: Fn(&Self) + 'static>(
        data: &Tag,
        wrap_box: &adw::WrapBox,
        window: &EuphonicaWindow,
        on_remove: T,
    ) -> Self {
        let res: Self = Object::builder().build();
        let _ = res.imp().data.set(data.clone());
        res.imp().name.set_label(data.name());
        // res.imp().set_by_user.set(&data.set_by_user());
        if data.removable() {
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

        if let Some(link) = data.link() {
            res.imp().tag_btn.set_tooltip_text(Some(link));
            let owned_link = link.to_owned();
            res.imp().tag_btn.connect_clicked(clone!(
                #[weak]
                window,
                move |_| {
                    let launcher = gtk::FileLauncher::new(Some(&gio::File::for_uri(&owned_link)));
                    launcher.launch(Some(&window), gio::Cancellable::NONE, |_| {});
                }
            ));
        }
        let count = data.count();
        if count > 1 {
            res.imp().count.set_label(&count.to_string());
            res.imp().count.set_visible(true);
        }
        res
    }

    pub fn data(&self) -> Option<&Tag> {
        self.imp().data.get()
    }
}
