// use duplicate::duplicate_item;

pub mod fft;
pub mod backend;
pub mod fifo;
pub mod pipewire;

use backend::*;
pub use fifo::FifoFftBackend;
pub use pipewire::PipeWireFftBackend;

// #[duplicate_item(name; [FifoFftBackend]; [PipeWireFftBackend])]
// impl Drop for name {
//     fn drop(&mut self) {
//         self.stop();
//     }
// }
