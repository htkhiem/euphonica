use duplicate::duplicate_item;

pub mod backend;
use backend::*;
use futures::FutureExt;
pub mod fft;

// FIFO BACKEND
#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
pub mod fifo;
#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
pub use fifo::FifoFftBackend;

// PIPEWIRE BACKEND
#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
pub mod pipewire;
#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
pub use pipewire::PipeWireFftBackend;

#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
#[duplicate_item(name; [FifoFftBackend]; [PipeWireFftBackend])]
impl Drop for name {
    fn drop(&mut self) {
        self.stop().now_or_never();
    }
}
