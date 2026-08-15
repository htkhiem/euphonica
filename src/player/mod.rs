mod bar;
mod controller;
mod fft_backends;
mod knob;
mod output;
mod output_controls;
mod pane;
mod playback_controls;
mod queue_view;
mod ratio_center_box;
mod seekbar2;

use adw::prelude::AnimationExt;
use gtk::glib::WeakRef;

use knob::VolumeKnob;
use output::MpdOutput;

fn player_is_playing(player: &WeakRef<Player>) -> bool {
    player
        .upgrade()
        .is_some_and(|player| player.state() == PlaybackState::Playing)
}

fn sync_animation(animation: &adw::TimedAnimation, should_run: bool) {
    if should_run {
        match animation.state() {
            adw::AnimationState::Idle | adw::AnimationState::Finished => animation.play(),
            adw::AnimationState::Paused => animation.resume(),
            _ => {}
        }
    } else if animation.state() == adw::AnimationState::Playing {
        animation.pause();
    }
}

pub use bar::PlayerBar;
pub use controller::{PlaybackState, PlaybackFlow, Player, get_next_replaygain};
pub use fft_backends::backend::FftStatus;
pub use output_controls::OutputControls;
pub use pane::PlayerPane;
pub use playback_controls::PlaybackControls;
pub use queue_view::QueueView;
