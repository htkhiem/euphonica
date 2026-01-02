mod connection;
mod stream;

pub mod password;
pub mod state;
pub mod wrapper;

pub use state::{ClientState, ConnectionState};
pub use wrapper::MpdWrapper;


pub use connection::Error;
pub use connection::Result;

#[derive(Debug, Clone, Copy)]
pub enum StickerSetMode {
    Inc,
    Set,
    Dec,
}
