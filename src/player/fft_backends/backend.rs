use std::{
    rc::Rc, sync::{Arc, Mutex}
};
use glib::{prelude::*};

use crate::player::Player;

#[derive(Clone, Copy, Debug, glib::Enum, PartialEq, Default)]
#[enum_type(name = "EuphonicaFftStatus")]
pub enum FftStatus {
    #[default]
    Invalid,
    Stopping,
    ValidNotReading, // due to visualiser not being run
    Reading,
}

pub trait FftBackendImpl {
    /// Name as used in GSettings
    fn name(&self) -> &'static str;
    fn player(&self) -> &Player;
    fn get_param(&self, key: &str) -> Option<glib::Variant>;
    fn set_param(&self, key: &str, val: glib::Variant);

    fn start(self: Rc<Self>, output: Arc<Mutex<(Vec<f32>, Vec<f32>)>>) -> Result<(), ()>;
    fn stop(&self, block: bool);
}

pub trait FftBackendExt: FftBackendImpl {
    fn status(&self) -> FftStatus {
        self.player().fft_status()
    }

    fn set_status(&self, new: FftStatus) {
        self.player().set_fft_status(new);
    }

    fn emit_param_changed(&self, key: &str, val: &glib::Variant) {
        self.player().emit_by_name::<()>("fft-param-changed", &[&self.name(), &key, val]);
    }
}

impl<O: FftBackendImpl> FftBackendExt for O {}
