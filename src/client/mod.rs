use chrono::{DateTime, Duration, Local};

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

const BATCH_SIZE: usize = 128;
const FETCH_LIMIT: usize = 10000000; // Fetch at most ten million songs at once (same
// folder, same tag, etc)

fn get_past_unix_timestamp(backoff: i64) -> i64 {
    let current_local_dt: DateTime<Local> = Local::now();
    let backoff_dur: Duration = Duration::seconds(backoff);
    current_local_dt
        .checked_sub_signed(backoff_dur)
        .unwrap()
        .timestamp()
}
