//! `Audio`, `Source` and `SoundData` — Love2D's `love.audio` and
//! `love.sound`, on `rodio`.
//!
//! A `Source` is one voice, as in Love2D: `play` on a source that is already
//! playing does nothing, so overlapping copies of a sound effect are
//! `source.clone()`s. A source is decoded into memory once when it is made
//! and shared between its clones, so playing one allocates nothing.
//!
//! `SoundData` is a buffer of samples a program fills itself — the way to
//! make sound effects without shipping a file, and what `Audio.newSourceFrom`
//! turns into a playable source.
//!
//! With no audio device (a headless machine, a container), every call still
//! works and nothing is heard: a game should not die for want of speakers.
//! `Audio.isAvailable()` says which case this is.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::time::Duration;

use rodio::{OutputStream, OutputStreamHandle, Sink};
use saule_sdk::prelude::*;
use saule_sdk::saule_export;

/// Whether a source is decoded up front or streamed. Both are decoded into
/// memory here; the distinction is kept so programs written for Love2D read
/// the same.
#[saule_enum]
#[derive(Clone, Copy, PartialEq)]
pub enum SourceType {
    Static,
    Stream,
}

struct Output {
    _stream: OutputStream,
    handle: OutputStreamHandle,
}

thread_local! {
    /// The device, opened on first use; `None` inside means there is none.
    static OUTPUT: RefCell<Option<Option<Output>>> = const { RefCell::new(None) };
    /// Every sink that may still be playing, for `Audio.stop()` and the
    /// active-source count.
    static SINKS: RefCell<Vec<std::sync::Weak<Sink>>> = const { RefCell::new(Vec::new()) };
}

/// The master volume, read by every playing voice as it mixes.
static MASTER: AtomicU32 = AtomicU32::new(0x3F80_0000); // 1.0f32

fn master() -> f32 {
    f32::from_bits(MASTER.load(Ordering::Relaxed))
}

fn with_output<R>(f: impl FnOnce(Option<&OutputStreamHandle>) -> R) -> R {
    OUTPUT.with(|o| {
        let mut o = o.borrow_mut();
        let out = o.get_or_insert_with(|| {
            OutputStream::try_default()
                .ok()
                .map(|(stream, handle)| Output {
                    _stream: stream,
                    handle,
                })
        });
        f(out.as_ref().map(|o| &o.handle))
    })
}

// ---------------------------------------------------------------------------
// The playing voice
// ---------------------------------------------------------------------------

/// Decoded samples, interleaved.
struct Pcm {
    data: Arc<[f32]>,
    channels: u16,
    rate: u32,
}

/// A `rodio` source over shared samples, which reports where it is and
/// reads the looping flag live, so `setLooping` applies mid-play.
struct Voice {
    data: Arc<[f32]>,
    channels: u16,
    rate: u32,
    pos: Arc<AtomicUsize>,
    looping: Arc<AtomicBool>,
}

impl Iterator for Voice {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let mut i = self.pos.load(Ordering::Relaxed);
        if i >= self.data.len() {
            if !self.looping.load(Ordering::Relaxed) || self.data.is_empty() {
                return None;
            }
            i = 0;
        }
        self.pos.store(i + 1, Ordering::Relaxed);
        Some(self.data[i] * master())
    }
}

impl rodio::Source for Voice {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> u16 {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

// ---------------------------------------------------------------------------
// Source
// ---------------------------------------------------------------------------

/// A sound that can be played: one voice, with its own volume, pitch and
/// looping. `clone()` it to play the same sound over itself.
#[saule_class]
pub struct Source {
    pcm: Arc<Pcm>,
    sink: Option<Arc<Sink>>,
    pos: Arc<AtomicUsize>,
    looping: Arc<AtomicBool>,
    volume: f32,
    pitch: f32,
}

impl Source {
    fn from_pcm(pcm: Pcm) -> Self {
        Source {
            pcm: Arc::new(pcm),
            sink: None,
            pos: Arc::new(AtomicUsize::new(0)),
            looping: Arc::new(AtomicBool::new(false)),
            volume: 1.0,
            pitch: 1.0,
        }
    }

    fn start(&mut self) {
        let voice = Voice {
            data: Arc::clone(&self.pcm.data),
            channels: self.pcm.channels,
            rate: self.pcm.rate,
            pos: Arc::clone(&self.pos),
            looping: Arc::clone(&self.looping),
        };
        self.sink = with_output(|out| {
            let sink = Arc::new(Sink::try_new(out?).ok()?);
            sink.set_volume(self.volume);
            sink.set_speed(self.pitch);
            sink.append(voice);
            SINKS.with(|s| {
                let mut s = s.borrow_mut();
                s.retain(|w| w.strong_count() > 0);
                s.push(Arc::downgrade(&sink));
            });
            Some(sink)
        });
    }

    fn frames(&self) -> usize {
        self.pcm.data.len() / self.pcm.channels.max(1) as usize
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        if let Some(s) = self.sink.take() {
            s.stop();
        }
    }
}

#[saule_methods]
impl Source {
    /// Start playing — from where it was paused, or from the beginning if it
    /// had stopped or finished. Does nothing if it is already playing.
    pub fn play(&mut self) {
        if let Some(s) = &self.sink
            && !s.empty()
        {
            s.play();
            return;
        }
        if self.pos.load(Ordering::Relaxed) >= self.pcm.data.len() {
            self.pos.store(0, Ordering::Relaxed);
        }
        self.start();
    }

    /// Stop, and rewind to the beginning.
    pub fn stop(&mut self) {
        if let Some(s) = self.sink.take() {
            s.stop();
        }
        self.pos.store(0, Ordering::Relaxed);
    }

    /// Pause where it is; `play` resumes.
    pub fn pause(&mut self) {
        if let Some(s) = &self.sink {
            s.pause();
        }
    }

    pub fn is_playing(&self) -> bool {
        self.sink
            .as_ref()
            .is_some_and(|s| !s.is_paused() && !s.empty())
    }

    /// `0.0` is silent, `1.0` as loaded.
    pub fn set_volume(&mut self, volume: f64) {
        self.volume = volume.max(0.0) as f32;
        if let Some(s) = &self.sink {
            s.set_volume(self.volume);
        }
    }

    pub fn get_volume(&self) -> f64 {
        self.volume as f64
    }

    /// Playback speed and pitch together: `2.0` is an octave up and twice
    /// as fast.
    pub fn set_pitch(&mut self, pitch: f64) -> Result<(), String> {
        if pitch.is_nan() || pitch <= 0.0 {
            return Err("Source.setPitch: the pitch must be positive".into());
        }
        self.pitch = pitch as f32;
        if let Some(s) = &self.sink {
            s.set_speed(self.pitch);
        }
        Ok(())
    }

    pub fn get_pitch(&self) -> f64 {
        self.pitch as f64
    }

    /// Start again from the beginning when it reaches the end.
    pub fn set_looping(&mut self, looping: bool) {
        self.looping.store(looping, Ordering::Relaxed);
    }

    pub fn is_looping(&self) -> bool {
        self.looping.load(Ordering::Relaxed)
    }

    /// Jump to `seconds` from the start.
    pub fn seek(&mut self, seconds: f64) {
        let frame = (seconds.max(0.0) * self.pcm.rate as f64) as usize;
        let at = frame.min(self.frames()) * self.pcm.channels.max(1) as usize;
        self.pos.store(at, Ordering::Relaxed);
    }

    /// Seconds from the start to where it is now.
    pub fn tell(&self) -> f64 {
        let frame = self.pos.load(Ordering::Relaxed) / self.pcm.channels.max(1) as usize;
        frame as f64 / self.pcm.rate as f64
    }

    /// Length in seconds.
    pub fn get_duration(&self) -> f64 {
        self.frames() as f64 / self.pcm.rate as f64
    }

    pub fn get_channel_count(&self) -> i64 {
        self.pcm.channels as i64
    }

    /// A new voice of the same sound, stopped, with this one's volume, pitch
    /// and looping. Decoded samples are shared, not copied.
    #[saule(name = "clone")]
    pub fn duplicate(&self) -> Source {
        Source {
            pcm: Arc::clone(&self.pcm),
            sink: None,
            pos: Arc::new(AtomicUsize::new(0)),
            looping: Arc::new(AtomicBool::new(self.is_looping())),
            volume: self.volume,
            pitch: self.pitch,
        }
    }
}

// ---------------------------------------------------------------------------
// SoundData
// ---------------------------------------------------------------------------

/// A buffer of mono samples in `-1.0..=1.0`, to fill in yourself and play
/// with `Audio.newSourceFrom`. Love2D's `SoundData`.
#[saule_class]
pub struct SoundData {
    samples: Vec<f32>,
    rate: u32,
}

#[saule_methods]
impl SoundData {
    /// `SoundData(count [, rate])` — `count` samples of silence at `rate`
    /// samples per second (44100 by default).
    pub fn new(count: i64, rate: Option<i64>) -> Result<Self, String> {
        let rate = rate.unwrap_or(44_100);
        if count <= 0 || count > 44_100 * 600 {
            return Err(
                "SoundData: the sample count must be between 1 and ten minutes' worth".into(),
            );
        }
        if !(8_000..=192_000).contains(&rate) {
            return Err("SoundData: the rate must be between 8000 and 192000".into());
        }
        Ok(SoundData {
            samples: vec![0.0; count as usize],
            rate: rate as u32,
        })
    }

    /// Set sample `i` (from 0), clamped to `-1.0..=1.0`.
    pub fn set_sample(&mut self, i: i64, value: f64) -> Result<(), String> {
        let slot = usize::try_from(i)
            .ok()
            .and_then(|i| self.samples.get_mut(i))
            .ok_or_else(|| format!("SoundData.setSample: no sample {i}"))?;
        *slot = value.clamp(-1.0, 1.0) as f32;
        Ok(())
    }

    /// Sample `i` (from 0).
    pub fn get_sample(&self, i: i64) -> Result<f64, String> {
        usize::try_from(i)
            .ok()
            .and_then(|i| self.samples.get(i))
            .map(|&v| v as f64)
            .ok_or_else(|| format!("SoundData.getSample: no sample {i}"))
    }

    /// Set every sample from `first` on from a table of values — far faster
    /// than one `setSample` per sample for a long sound.
    pub fn set_samples(&mut self, first: i64, values: Vec<f64>) -> Result<(), String> {
        let start = usize::try_from(first)
            .map_err(|_| format!("SoundData.setSamples: no sample {first}"))?;
        let end = start + values.len();
        if end > self.samples.len() {
            return Err(format!(
                "SoundData.setSamples: {} values from {first} run past the {} samples",
                values.len(),
                self.samples.len()
            ));
        }
        for (slot, v) in self.samples[start..end].iter_mut().zip(values) {
            *slot = v.clamp(-1.0, 1.0) as f32;
        }
        Ok(())
    }

    pub fn get_sample_count(&self) -> i64 {
        self.samples.len() as i64
    }

    pub fn get_sample_rate(&self) -> i64 {
        self.rate as i64
    }

    /// Length in seconds.
    pub fn get_duration(&self) -> f64 {
        self.samples.len() as f64 / self.rate as f64
    }
}

// ---------------------------------------------------------------------------
// Audio
// ---------------------------------------------------------------------------

/// `Audio.newSource(path [, type])` — load a WAV or Ogg Vorbis file.
#[saule_export(class = "Audio", name = "newSource")]
fn audio_new_source(path: String, kind: Option<SourceType>) -> Result<Source, String> {
    let _ = kind;
    let file = std::fs::File::open(&path).map_err(|e| format!("Audio.newSource: {path}: {e}"))?;
    let decoder = rodio::Decoder::new(std::io::BufReader::new(file))
        .map_err(|e| format!("Audio.newSource: {path}: {e}"))?;
    let channels = rodio::Source::channels(&decoder);
    let rate = rodio::Source::sample_rate(&decoder);
    let data: Vec<f32> = rodio::Source::convert_samples(decoder).collect();
    Ok(Source::from_pcm(Pcm {
        data: data.into(),
        channels,
        rate,
    }))
}

/// `Audio.newSourceFrom(data)` — a source playing a `SoundData`, copied as it
/// is now. Love2D's `newSource(soundData)`.
#[saule_export(class = "Audio", name = "newSourceFrom")]
fn audio_new_source_from(data: &SoundData) -> Source {
    Source::from_pcm(Pcm {
        data: data.samples.clone().into(),
        channels: 1,
        rate: data.rate,
    })
}

/// `Audio.newSoundData(count [, rate])` — the same as `SoundData(…)`.
#[saule_export(class = "Audio", name = "newSoundData")]
fn audio_new_sound_data(count: i64, rate: Option<i64>) -> Result<SoundData, String> {
    SoundData::new(count, rate)
}

/// `Audio.play(source)` — `source.play()`.
#[saule_export(class = "Audio", name = "play")]
fn audio_play(source: &mut Source) {
    source.play();
}

/// `Audio.stop()` — stop every source.
#[saule_export(class = "Audio", name = "stop")]
fn audio_stop() {
    SINKS.with(|s| {
        for sink in s.borrow_mut().drain(..).filter_map(|w| w.upgrade()) {
            sink.stop();
        }
    });
}

/// `Audio.setVolume(volume)` — the master volume, over every source.
#[saule_export(class = "Audio", name = "setVolume")]
fn audio_set_volume(volume: f64) {
    MASTER.store((volume.max(0.0) as f32).to_bits(), Ordering::Relaxed);
}

/// `Audio.getVolume()` — the master volume.
#[saule_export(class = "Audio", name = "getVolume")]
fn audio_get_volume() -> f64 {
    master() as f64
}

/// `Audio.getActiveSourceCount()` — sources playing right now.
#[saule_export(class = "Audio", name = "getActiveSourceCount")]
fn audio_get_active_source_count() -> i64 {
    SINKS.with(|s| {
        let mut s = s.borrow_mut();
        s.retain(|w| w.upgrade().is_some_and(|k| !k.empty()));
        s.iter()
            .filter_map(|w| w.upgrade())
            .filter(|k| !k.is_paused())
            .count() as i64
    })
}

/// `Audio.isAvailable()` — whether there is a device to play on. Without
/// one, sources still work and are silent.
#[saule_export(class = "Audio", name = "isAvailable")]
fn audio_is_available() -> bool {
    with_output(|out| out.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_voice_loops_only_while_asked_to() {
        let looping = Arc::new(AtomicBool::new(true));
        let mut v = Voice {
            data: Arc::from(vec![0.5f32, -0.5]),
            channels: 1,
            rate: 8000,
            pos: Arc::new(AtomicUsize::new(0)),
            looping: Arc::clone(&looping),
        };
        let first: Vec<f32> = (&mut v).take(5).collect();
        assert_eq!(first, vec![0.5, -0.5, 0.5, -0.5, 0.5]);
        looping.store(false, Ordering::Relaxed);
        assert_eq!(v.next(), Some(-0.5));
        assert_eq!(v.next(), None);
    }

    #[test]
    fn sound_data_clamps_and_bounds_checks() {
        let mut d = SoundData::new(4, Some(8000)).unwrap();
        d.set_sample(0, 3.0).unwrap();
        assert_eq!(d.get_sample(0).unwrap(), 1.0);
        assert!(d.set_sample(4, 0.0).is_err());
        assert!(d.set_samples(2, vec![0.1, 0.2, 0.3]).is_err());
        d.set_samples(2, vec![0.1, 0.2]).unwrap();
        assert!((d.get_sample(3).unwrap() - 0.2).abs() < 1e-6);
        assert!((d.get_duration() - 0.0005).abs() < 1e-9);
    }
}
