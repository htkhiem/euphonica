use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, glib::Enum, PartialEq, Default)]
#[enum_type(name = "EuphonicaFftStatus")]
pub enum FftStatus {
    #[default]
    Invalid,
    ValidNotReading, // due to visualiser not being run
    Reading,
}

pub trait FftBackend {
    /// Start a new FFT thread reading from a particular data source.
    /// This function must not block the main thread.
    fn start(&self, output: Arc<Mutex<(Vec<f32>, Vec<f32>)>>) -> Result<(), ()>;

    /// Stop the FFT thread of this backend.
    fn stop(&self);

    fn status(&self) -> FftStatus;
}
