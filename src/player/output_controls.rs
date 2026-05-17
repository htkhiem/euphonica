use glib::{Object, SignalHandlerId, WeakRef};
use gtk::{
    CompositeTemplate,
    glib::{self, clone},
    prelude::*,
    subclass::prelude::*,
};
use std::cell::RefCell;

use super::{MpdOutput, Player};

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/player/output-controls.ui")]
    pub struct OutputControls {
        #[template_child]
        pub prev_output: TemplateChild<gtk::Button>,
        #[template_child]
        pub next_output: TemplateChild<gtk::Button>,
        #[template_child]
        pub output_stack: TemplateChild<gtk::Stack>,

        // Internal state
        pub output_widgets: RefCell<Vec<MpdOutput>>,
        pub player: WeakRef<Player>,
        pub current_output_id: RefCell<Option<SignalHandlerId>>,
        pub outputs_changed_id: RefCell<Option<SignalHandlerId>>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for OutputControls {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaOutputControls";
        type Type = super::OutputControls;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for OutputControls {
        fn dispose(&self) {
            if let Some(player) = self.player.upgrade() {
                if let Some(id) = self.current_output_id.take() {
                    player.disconnect(id);
                }
                if let Some(id) = self.outputs_changed_id.take() {
                    player.disconnect(id);
                }
            }
        }
    }

    // Trait shared by all widgets
    impl WidgetImpl for OutputControls {}

    // Trait shared by all boxes
    impl BoxImpl for OutputControls {}
}

glib::wrapper! {
    pub struct OutputControls(ObjectSubclass<imp::OutputControls>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for OutputControls {
    fn default() -> Self {
        Self::new()
    }
}

impl OutputControls {
    pub fn new() -> Self {
        Object::builder().build()
    }

    pub fn setup(&self, player: &Player) {
        let imp = self.imp();
        self.imp().player.set(Some(player));
        // Set up button click handlers
        imp.prev_output.connect_clicked(clone!(
            #[weak]
            player,
            move |_| {
                player.switch_output(true);
            }
        ));
        imp.next_output.connect_clicked(clone!(
            #[weak]
            player,
            move |_| {
                player.switch_output(false);
            }
        ));

        // Connect to player signals for output changes
        imp.current_output_id
            .replace(Some(player.connect_notify_local(
                Some("current-output"),
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |player, _| {
                        this.set_visible_output(player.current_output());
                    }
                ),
            )));

        imp.outputs_changed_id.replace(Some(player.connect_closure(
            "outputs-changed",
            false,
            glib::closure_local!(
                #[weak(rename_to = this)]
                self,
                move |player: Player| {
                    this.update_outputs(&player);
                }
            ),
        )));

        // Initial update
        self.update_outputs(player);
    }

    fn update_outputs(&self, player: &Player) {
        println!("Updating outputs...");
        let outputs = player.outputs();
        let outputs: Vec<glib::BoxedAnyObject> = (0..outputs.n_items())
            .map(|i| {
                outputs
                    .item(i)
                    .unwrap()
                    .downcast::<glib::BoxedAnyObject>()
                    .unwrap()
            })
            .collect();
        let imp = self.imp();
        let section = imp.output_stack.get();
        let new_len = outputs.len();
        if new_len == 0 {
            section.set_visible(false);
        } else {
            section.set_visible(true);
            if new_len > 1 {
                imp.prev_output.set_visible(true);
                imp.next_output.set_visible(true);
            } else {
                imp.prev_output.set_visible(false);
                imp.next_output.set_visible(false);
            }
        }
        // Handle new/removed outputs
        // Pretty rare though...
        {
            let mut output_widgets = imp.output_widgets.borrow_mut();
            let curr_len = output_widgets.len();
            if curr_len >= new_len {
                // Trim down
                for w in &output_widgets[new_len..] {
                    section.remove(w);
                }
                output_widgets.truncate(new_len);
                // Overwrite state of the remaining widgets
                // Note that this does not re-populate the stack, so the visible
                // child won't be changed.
                for (w, o) in output_widgets.iter().zip(outputs) {
                    w.update_state(&o.borrow());
                }
            } else {
                // Need to add more widgets
                // Override state of all current widgets. Personal reminder:
                // zip() is auto-truncated to the shorter of the two iters.
                for (w, o) in output_widgets.iter().zip(&outputs) {
                    w.update_state(&o.borrow());
                }
                output_widgets.reserve_exact(new_len - curr_len);
                for o in &outputs[curr_len..] {
                    let w = MpdOutput::from_output(&o.borrow(), player);
                    section.add_child(&w);
                    output_widgets.push(w);
                }
            }
        }
        // Use this to update sensitivity of back/forward
        self.set_visible_output(player.current_output());
    }

    fn set_visible_output(&self, new_idx: i32) {
        let imp = self.imp();
        let outputs = imp.output_widgets.borrow();
        let output_count = outputs.len();
        if output_count > 0 && new_idx >= 0 {
            let max = output_count - 1;
            if new_idx as usize >= max {
                imp.next_output.set_sensitive(false);
                imp.prev_output.set_sensitive(true);
            } else if new_idx <= 0 {
                imp.next_output.set_sensitive(true);
                imp.prev_output.set_sensitive(false);
            } else {
                imp.next_output.set_sensitive(true);
                imp.prev_output.set_sensitive(true);
            }

            // Update stack
            imp.output_stack
                .set_visible_child(&outputs[new_idx as usize]);
        }
    }
}
