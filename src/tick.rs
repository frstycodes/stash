//! A tiny, clean click in the manner of an iOS picker ("Detent", picked by ear from
//! `design/tick-sounds.html`), played quietly as a clip grip steps a second (by dragging
//! it or turning the mouse wheel over it). Synthesized once into an in-memory
//! WAV, so there's no sound file to ship, and played without waiting.

use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
use windows::core::PCWSTR;

const RATE: u32 = 44_100;
/// Ticks closer together than this are dropped (a picker wheel ticks fast, but never
/// buzzes).
const MIN_GAP: Duration = Duration::from_millis(30);

static UP: OnceLock<Vec<u8>> = OnceLock::new();
static DOWN: OnceLock<Vec<u8>> = OnceLock::new();
static LAST: Mutex<Option<Instant>> = Mutex::new(None);

/// One tick: a hair higher going forward than going back.
pub fn play(forward: bool) {
    {
        let mut last = LAST.lock().unwrap();
        let now = Instant::now();
        if last.is_some_and(|t| now.duration_since(t) < MIN_GAP) {
            return;
        }
        *last = Some(now);
    }
    let wav = if forward { UP.get_or_init(|| wav(&tick(1.03))) } else { DOWN.get_or_init(|| wav(&tick(1.0))) };
    unsafe {
        // SND_MEMORY reads the WAV from this (static) buffer; a new tick cuts off the last
        let _ = PlaySoundW(PCWSTR(wav.as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
    }
}

/// The tick's samples (-1..1): "Detent" from `design/tick-sounds.html`, the one the user
/// picked. Two pure tones (2 kHz, and 950 Hz at half its level) dying away within a
/// couple of milliseconds, eased in over 0.15 ms: a tiny, clean click in the manner of an
/// iOS picker, more like a toggle than a knock. `pitch` scales it.
fn tick(pitch: f32) -> Vec<f32> {
    let n = (RATE as f32 * 0.03) as usize;
    let tau = std::f32::consts::TAU;
    let mut out: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / RATE as f32;
            let hi = (tau * 2000.0 * pitch * t).sin() * (-t / 0.0012).exp();
            let lo = (tau * 950.0 * pitch * t).sin() * (-t / 0.0025).exp() * 0.5;
            // eased in, and faded out over the last fifth, so it never pops
            (hi + lo) * smooth(t / 0.00015) * smooth((n - i) as f32 / (n as f32 * 0.2))
        })
        .collect();
    let peak = out.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    let gain = if peak > 0.0 { LEVEL / peak } else { 0.0 };
    for x in &mut out {
        *x *= gain;
    }
    out
}

/// The tick's peak: Detent's level on the audition page with its volume at -35 dB
/// (0.9 × 10^(-35/20), about -36 dBFS), as picked.
const LEVEL: f32 = 0.9 * 0.017_782_794;

fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// 16-bit mono PCM in a WAV container.
fn wav(samples: &[f32]) -> Vec<u8> {
    let data = (samples.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + data as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&RATE.to_le_bytes());
    w.extend_from_slice(&(RATE * 2).to_le_bytes()); // bytes a second
    w.extend_from_slice(&2u16.to_le_bytes()); // bytes a frame
    w.extend_from_slice(&16u16.to_le_bytes()); // bits a sample
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        w.extend_from_slice(&((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes());
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_quiet_tick() {
        let s = tick(1.0);
        let peak = s.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!((peak - LEVEL).abs() < 1e-6, "peaks at the picked level: {peak}");
        assert!((20.0 * peak.log10() + 35.9).abs() < 0.1, "about -36 dBFS");
        // a click: nearly all of it in the first 10 ms, and silent at the end
        let energy = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>();
        let first = energy(&s[..(RATE / 100) as usize]) / energy(&s);
        assert!(first > 0.98, "over in about 10 ms: {first}");
        assert!(s.last().unwrap().abs() < 1e-6);
        let w = wav(&s);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(w.len(), 44 + s.len() * 2);
    }

    /// `cargo test tick::tests::listen -- --ignored` plays a few, to hear it.
    #[test]
    #[ignore]
    fn listen() {
        for i in 0..8 {
            play(i % 4 != 3);
            std::thread::sleep(Duration::from_millis(140));
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}
