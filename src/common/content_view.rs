use adw::subclass::prelude::*;
use derivative::Derivative;
use glib::{ParamSpec, ParamSpecObject};
use gtk::{CompositeTemplate, glib, prelude::*};
use once_cell::sync::Lazy;

mod imp {
    use super::*;

    #[derive(Debug, CompositeTemplate, Derivative)]
    #[derivative(Default)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/content-view.ui")]
    pub struct ContentView {
        #[template_child]
        pub header_bar: TemplateChild<adw::HeaderBar>,
        #[template_child]
        pub infobox_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub collapse_infobox: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub action_row: TemplateChild<gtk::CenterBox>,
        #[template_child]
        pub content_area: TemplateChild<gtk::Box>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ContentView {
        const NAME: &'static str = "EuphonicaContentView";
        type Type = super::ContentView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);

            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_accessible_role(gtk::AccessibleRole::Group);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ContentView {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }

        fn constructed(&self) {
            self.parent_constructed();

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
        }

        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecObject::builder::<gtk::Widget>("header-end-widget")
                        .construct_only()
                        .build(),
                    ParamSpecObject::builder::<gtk::Widget>("infobox-widget")
                        .construct_only()
                        .build(),
                    ParamSpecObject::builder::<gtk::Widget>("action-row-start-widget")
                        .construct_only()
                        .build(),
                    ParamSpecObject::builder::<gtk::Widget>("action-row-center-widget")
                        .construct_only()
                        .build(),
                    ParamSpecObject::builder::<gtk::Widget>("action-row-end-widget")
                        .construct_only()
                        .build(),
                    ParamSpecObject::builder::<gtk::Widget>("content")
                        .construct_only()
                        .build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &ParamSpec) {
            match pspec.name() {
                "header-end-widget" => {
                    if let Ok(widget) = value.get::<gtk::Widget>() {
                        self.header_bar.pack_end(&widget);
                    }
                }
                "infobox-widget" => {
                    if let Ok(widget) = value.get::<gtk::Widget>() {
                        self.infobox_revealer.set_child(Some(&widget));
                    }
                }
                "action-row-start-widget" => {
                    if let Ok(widget) = value.get::<gtk::Widget>() {
                        self.action_row.set_start_widget(Some(&widget));
                    }
                }
                "action-row-center-widget" => {
                    if let Ok(widget) = value.get::<gtk::Widget>() {
                        self.action_row.set_center_widget(Some(&widget));
                    }
                }
                "action-row-end-widget" => {
                    if let Ok(widget) = value.get::<gtk::Widget>() {
                        self.action_row.set_end_widget(Some(&widget));
                    }
                }
                "content" => {
                    if let Ok(widget) = value.get::<gtk::Widget>() {
                        self.content_area.append(&widget);
                    }
                }
                _ => unimplemented!(),
            }
        }
    }

    impl WidgetImpl for ContentView {}
}

glib::wrapper! {
    pub struct ContentView(ObjectSubclass<imp::ContentView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for ContentView {
    fn default() -> Self {
        glib::Object::new()
    }
}
