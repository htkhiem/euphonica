use ::glib::clone;
use adw::prelude::*;
use glib::subclass::Signal;
use gtk::subclass::prelude::*;

use adw::ColorScheme;

// Reimplementation of libpanel's ThemeSelector, as the
// Rust bindings don't really work.
mod imp {
    use std::sync::OnceLock;

    use super::*;
    use gtk::CompositeTemplate;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/theme-selector.ui")]
    pub struct ThemeSelector {
        #[template_child]
        pub follow: TemplateChild<gtk::CheckButton>,
        #[template_child]
        pub light: TemplateChild<gtk::CheckButton>,
        #[template_child]
        pub dark: TemplateChild<gtk::CheckButton>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ThemeSelector {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaThemeSelector";
        type Type = super::ThemeSelector;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ThemeSelector {
        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("changed")
                        .param_types([ColorScheme::static_type()])
                        .build(),
                ]
            })
        }
    }

    impl WidgetImpl for ThemeSelector {}

    impl BoxImpl for ThemeSelector {}
}

glib::wrapper! {
    pub struct ThemeSelector(ObjectSubclass<imp::ThemeSelector>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for ThemeSelector {
    fn default() -> Self {
        Self::new()
    }
}

impl ThemeSelector {
    pub fn new() -> Self {
        let style = adw::StyleManager::default();
        let res: Self = glib::Object::new();
        res.update_selection(style.color_scheme());
        style.connect_color_scheme_notify(clone!(
            #[weak(rename_to = this)]
            res,
            move |style| {
                this.update_selection(style.color_scheme());
            }
        ));

        style.connect_dark_notify(clone!(
            #[weak(rename_to = this)]
            res,
            move |style| {
                if style.is_dark() {
                    this.add_css_class("dark");
                } else {
                    this.remove_css_class("dark");
                }
            }
        ));

        res.imp().follow.connect_toggled(clone!(
            #[weak(rename_to = this)]
            res,
            move |btn| {
                if btn.is_active() {
                    this.emit_by_name::<()>("changed", &[&ColorScheme::Default]);
                }
            }
        ));

        res.imp().light.connect_toggled(clone!(
            #[weak(rename_to = this)]
            res,
            move |btn| {
                if btn.is_active() {
                    this.emit_by_name::<()>("changed", &[&ColorScheme::ForceLight]);
                }
            }
        ));

        res.imp().dark.connect_toggled(clone!(
            #[weak(rename_to = this)]
            res,
            move |btn| {
                if btn.is_active() {
                    this.emit_by_name::<()>("changed", &[&ColorScheme::ForceDark]);
                }
            }
        ));

        res
    }

    fn update_selection(&self, scheme: ColorScheme) {
        match scheme {
            ColorScheme::ForceDark | ColorScheme::PreferDark => {
                self.imp().dark.set_active(true);
            }
            ColorScheme::ForceLight | ColorScheme::PreferLight => {
                self.imp().light.set_active(true);
            }
            _ => {
                self.imp().follow.set_active(true);
            }
        }
    }
}
