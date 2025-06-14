use libc;
use std::{
    f32::consts::PI,
    fs::{File, OpenOptions},
    // Option 1: read from MPD FIFO output (default)
    io::{self, prelude::*, BufReader, Error},
    os::unix::fs::OpenOptionsExt,
};

// Option 2: read from local Pipewire output (works in Flatpak)

use mpd::status::AudioFormat;

pub fn open_named_pipe_readonly(path: &str) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_RDONLY) // from os::unix::fs::OpenOptionsExt, i.e. on Unix-like systems only.
        .open(path)
}

/// Blackman-Harris 4-term window, for reducing scalloping loss in FFT.
/// Implementation taken from crate spectrum-analyzer v0.5.2, refactored
/// to run in-place.
fn blackman_harris_4term_inplace(samples: &mut [f32]) {
    const ALPHAS: [f32; 4] = [0.35875, -0.48829, 0.14128, -0.01168];
    let samples_len_f32 = samples.len() as f32;

    for i in 0..samples.len() {
        let mut acc = 0.0;

        // Will result in something like that:
        /* ALPHA0
            + ALPHA1 * ((2.0 * PI * *samples[i])/samples_len_f32).cos()
            + ALPHA2 * ((4.0 * PI * *samples[i])/samples_len_f32).cos()
            + ALPHA3 * ((6.0 * PI * *samples[i])/samples_len_f32).cos()
        */

        for alpha_i in 0..ALPHAS.len() {
            // in 1. iter. 0PI, then 2PI, then 4 PI, then 6 PI
            let two_pi_iteration = 2.0 * alpha_i as f32 * PI;
            let sample = samples[i];
            acc += ALPHAS[alpha_i] * ((two_pi_iteration * sample) / samples_len_f32).cos();
        }
        samples[i] = acc;
    }
}

pub fn try_open_pipe(
    path: &str,
    format: &AudioFormat,
    n_samples: usize,
) -> Result<BufReader<File>, Error> {
    // Bits per sample * 2 (stereo) * n_samples * 4 (safety factor)
    let buf_bytes = ((format.bits as usize * 8 * n_samples) as f64 / 8.0).ceil() as usize;

    let pipe = BufReader::with_capacity(buf_bytes, open_named_pipe_readonly(path)?);
    Ok(pipe)
}

fn parse_to_float(buf: [u8; 4], format: &AudioFormat, is_le: bool) -> f32 {
    // Currently MPD only supports streaming PCM through FIFO so we need not handle DSD here.
    // DSD is overkill for decorative things like these anyway.
    if format.bits == 0 {
        // 32bit float. Should already be -1 to 1, no need for normalisation.
        if buf.len() != 4 {
            panic!("Invalid FIFO data: configured to interpret as float32, got less than 4 bytes per sample");
        }
        if is_le {
            f32::from_le_bytes(buf)
        } else {
            f32::from_be_bytes(buf)
        }
    } else {
        // Assume signed magnitudes since that's what normal LPCM is
        let max_val: f32 = match format.bits {
            32 => std::i32::MAX as f32,
            16 => std::i16::MAX as f32,
            8 => std::i8::MAX as f32,
            _ => unimplemented!(),
        };
        if is_le {
            i32::from_le_bytes(buf) as f32 / max_val
        } else {
            i32::from_be_bytes(buf) as f32 / max_val
        }
    }
}

/// Read the specified number of samples, then return them as 32-bit float.
///
/// Here we assume the left and right channels are given one after the other as follows:
/// L1 R1 L2 R2 L3 R3 ...
/// That is, each sample consists of two numbers. Each number may also span multiple bytes.
///
/// When reading, try to read in the form of a sliding window by always reading the required
/// number of samples but only advancing the number of samples per frame. If the buffer has
/// not grown big enough, pad past with zeros.
///
/// Little-endianness is assumed for simplicity. TODO: maybe support big-endian too?
pub fn get_stereo_pcm(
    samples_left: &mut [f32],
    samples_right: &mut [f32],
    reader: &mut BufReader<File>,
    format: &AudioFormat,
    fps: f32,
    is_le: bool,
) -> Result<(), std::io::Error> {
    let num_samples = samples_left.len();
    if num_samples != samples_right.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Given sample buffers' lengths do not match",
        ));
    }
    samples_left.fill(0.0);
    samples_right.fill(0.0);
    // Bytes Per Sample (one channel)
    let bps = (format.bits / 8) as usize;
    // Stereo so x2 before /8
    let bytes_per_frame = (format.rate as f32 / fps * format.bits as f32 / 4.0).ceil() as usize;
    let internal_buf = reader.fill_buf()?;
    // Read & write offsets, such that we'll always read from the latest samples
    // and write into the latest slots.
    // If we have not collected enough data to fill the samples slices, this
    // will leave the earliest samples as zeros. Assume buffer never contains
    // "partial" samples, i.e. filled size is always divisible by bps * 2.
    let available = internal_buf.len() / bps / 2;
    let read_offset: usize = if available > num_samples {
        available - num_samples
    } else {
        0
    };
    let write_offset: usize = if available < num_samples {
        num_samples - available
    } else {
        0
    };
    // Each per-sample buffer will always be 4 bytes for easier parsing. Since we're assuming
    // little-endianness, it will be filled from the left, with the most significant bits left
    // blank if each sample has less than 32 bits.
    for idx in 0..(num_samples.min(available)) {
        // Left channel
        let mut sample_buf = vec![0u8; 4];
        let first_pos = read_offset + idx * 2 * bps;
        sample_buf[..bps].copy_from_slice(&internal_buf[first_pos..(first_pos + bps)]);
        samples_left[write_offset + idx] =
            parse_to_float(sample_buf.try_into().unwrap(), format, is_le);

        // Right channel
        let mut sample_buf = vec![0u8; 4];
        let second_pos = first_pos + bps;
        sample_buf[..bps].copy_from_slice(&internal_buf[second_pos..(second_pos + bps)]);
        samples_right[write_offset + idx] =
            parse_to_float(sample_buf.try_into().unwrap(), format, is_le);
    }
    // Advance internal buffer
    reader.consume(bytes_per_frame);
    Ok(())
}

#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum BinMode {
    Linear,
    Logarithmic,
}

/// Get frequency domain of stereo PCM samples.
///
/// This function may reallocate output_buf to match input_buf's size.
/// However, input_buf and scratch_buf must be of the same size from
/// the beginning.
///
/// Only frequencies within the human hearing range (20Hz to 20kHz) will
/// be kept.
///
/// This function also bins the magnitudes together to reduce the number
/// of output frequencies.
pub fn get_magnitudes(
    format: &AudioFormat,
    input_buf: &mut [f32],
    output_buf: &mut Vec<f32>,
    n_bins: u32,
    bin_mode: BinMode,
    min_freq: f32,
    max_freq: f32,
) {
    blackman_harris_4term_inplace(input_buf);

    let fft_res = spectrum_analyzer::samples_fft_to_spectrum(
        input_buf,
        format.rate,
        spectrum_analyzer::FrequencyLimit::Range(min_freq, max_freq),
        None,
        None,
    );
    let spectrum = fft_res.data();
    output_buf.clear();
    // Compute magnitudes. This might trigger mem allocations
    // if bin count has changed.
    for _ in 0..n_bins {
        output_buf.push(0.0);
    }

    // Determine which bin this frequency falls into.
    // Each bin's value is the maximum magnitude of all the frequencies therein.
    match bin_mode {
        BinMode::Linear => {
            let spacing = (max_freq - min_freq) / (n_bins as f32);
            for i in 0..spectrum.len() {
                let (freq, x) = spectrum[i];
                // Each bin's range is an interval open to the right, except for the last.
                let bin_idx: usize = (((freq.val() - min_freq) / spacing).floor() as usize)
                    .min((n_bins - 1) as usize);
                output_buf[bin_idx] = output_buf[bin_idx].max(x.val());
            }
        }
        BinMode::Logarithmic => {
            // Evenly-spaced but in logarithmic scale
            let log_base = (max_freq.log10() - min_freq.log10()) / (n_bins as f32);
            for i in 0..spectrum.len() {
                let (freq, x) = spectrum[i];
                // Same logic as linear, but converted to logarithmic first
                let bin_idx: usize = (((freq.val().log10() - min_freq.log10()) / log_base)
                    .floor()
                    .max(0.0) as usize)
                    .min((n_bins - 1) as usize);
                output_buf[bin_idx] = output_buf[bin_idx].max(x.val());
            }
        }
    }
}
