//! Sound the app plays and records itself, on the devices the user picked (Settings → Voice &
//! video): voice messages and audio files, voice memos, and the ring of an incoming call. Devices
//! are cpal's (PulseAudio's on Linux, which PipeWire serves too, so their names match the ones
//! calls use), found by name, falling back to the system's default. Files decode in pure Rust:
//! Ogg, WebM and MP4 audio (Opus, Vorbis, AAC), MP3, FLAC and WAV.
//!
//! Each stream runs on a thread of its own and stops when its handle drops. Headless runs (tests,
//! `--snap`) never open a device: they use Rune's player and recorder, which fake theirs.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

/// Voice memos are written at this rate, mono: plenty for speech, a quarter of CD's size.
const MEMO_RATE: u32 = 24_000;
/// The level meter's floor: silence reads this, in dBFS.
const FLOOR_DB: f32 = -160.0;

/// The host whose device names calls share: PulseAudio where there is one (Linux), else the
/// system's.
fn host() -> cpal::Host {
    #[cfg(all(unix, not(target_os = "macos")))]
    if let Ok(host) = cpal::host_from_id(cpal::HostId::PulseAudio) {
        return host;
    }
    cpal::default_host()
}

fn output_device(name: Option<&str>) -> Option<cpal::Device> {
    let host = host();
    name.and_then(|name| host.output_devices().ok()?.find(|device| device.to_string() == name))
        .or_else(|| host.default_output_device())
}

fn input_device(name: Option<&str>) -> Option<cpal::Device> {
    let host = host();
    name.and_then(|name| host.input_devices().ok()?.find(|device| device.to_string() == name))
        .or_else(|| host.default_input_device())
}

// ----- decoding ----------------------------------------------------------------------------

/// Decoded audio: interleaved samples from -1 to 1.
#[derive(Debug, Default)]
pub struct Pcm {
    pub rate: u32,
    pub channels: usize,
    pub samples: Vec<f32>,
}

impl Pcm {
    fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1)
    }

    /// Channel `channel` at fractional frame `at`, between the frames around it.
    fn sample(&self, at: f64, channel: usize) -> f32 {
        let channels = self.channels.max(1);
        let channel = channel.min(channels - 1);
        let first = at.floor() as usize;
        let mix = (at - at.floor()) as f32;
        let get = |frame: usize| self.samples.get(frame * channels + channel).copied().unwrap_or(0.0);
        get(first) * (1.0 - mix) + get(first + 1) * mix
    }
}

/// Decodes the file's first audio track.
pub fn decode(path: &Path) -> Result<Pcm, String> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{CODEC_TYPE_NULL, CODEC_TYPE_OPUS, DecoderOptions};
    use symphonia::core::errors::Error;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let fail = |e: Error| format!("can't decode this audio: {e}");
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(extension);
    }
    let options = FormatOptions { enable_gapless: true, ..Default::default() };
    let mut format = symphonia::default::get_probe()
        .format(&hint, stream, &options, &MetadataOptions::default())
        .map_err(fail)?
        .format;
    let track = format.tracks().iter().find(|t| t.codec_params.codec != CODEC_TYPE_NULL).ok_or("no audio track")?;
    let (id, params) = (track.id, track.codec_params.clone());
    let mut pcm = Pcm { rate: params.sample_rate.unwrap_or(48_000), channels: 0, samples: Vec::new() };
    // Opus has no decoder in symphonia: opuscule decodes its packets (RFC 7845).
    let mut opus = if params.codec == CODEC_TYPE_OPUS {
        Some(Opus::new(params.extra_data.as_deref().unwrap_or_default())?)
    } else {
        None
    };
    let mut decoder = match opus {
        Some(_) => None,
        None => Some(symphonia::default::get_codecs().make(&params, &DecoderOptions::default()).map_err(fail)?),
    };
    let mut buffer: Option<SampleBuffer<f32>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(Error::ResetRequired) => break,
            Err(e) => return Err(fail(e)),
        };
        if packet.track_id() != id {
            continue;
        }
        if let Some(opus) = opus.as_mut() {
            pcm.rate = 48_000;
            pcm.channels = opus.channels;
            opus.decode(&packet.data, &mut pcm.samples)?;
            continue;
        }
        let Some(decoder) = decoder.as_mut() else { break };
        match decoder.decode(&packet) {
            Ok(audio) => {
                let spec = *audio.spec();
                pcm.rate = spec.rate;
                pcm.channels = spec.channels.count();
                let needed = audio.capacity() * spec.channels.count();
                if buffer.as_ref().is_none_or(|b| b.capacity() < needed) {
                    buffer = Some(SampleBuffer::new(audio.capacity() as u64, spec));
                }
                if let Some(buffer) = buffer.as_mut() {
                    buffer.copy_interleaved_ref(audio);
                    pcm.samples.extend_from_slice(buffer.samples());
                }
            }
            // A damaged packet is skipped, as a player would.
            Err(Error::DecodeError(_)) => {}
            Err(e) => return Err(fail(e)),
        }
    }
    if pcm.channels == 0 {
        return Err("the audio is empty".into());
    }
    Ok(pcm)
}

struct Opus {
    decoder: opuscule::Decoder,
    channels: usize,
    /// Decoded frames still to drop for the pre-skip.
    skip: usize,
    gain: f32,
    buffer: Vec<f32>,
}

impl Opus {
    /// From the `OpusHead` identification header (RFC 7845 §5.1).
    fn new(head: &[u8]) -> Result<Self, String> {
        if head.len() < 19 || &head[..8] != b"OpusHead" {
            return Err("Opus audio without its header".into());
        }
        let channels = usize::from(head[9]);
        if head[18] != 0 || !(1..=2).contains(&channels) {
            return Err(format!("Opus with {channels} channels isn't supported"));
        }
        let skip = usize::from(u16::from_le_bytes([head[10], head[11]]));
        let gain_db = f32::from(i16::from_le_bytes([head[16], head[17]])) / 256.0;
        let layout = if channels == 1 { opuscule::Channels::Mono } else { opuscule::Channels::Stereo };
        Ok(Self {
            decoder: opuscule::Decoder::new(opuscule::SampleRate::Hz48000, layout),
            channels,
            skip,
            gain: 10f32.powf(gain_db / 20.0),
            // 120 ms, the longest packet.
            buffer: vec![0.0; 5760 * channels],
        })
    }

    fn decode(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<(), String> {
        let frames = match self.decoder.decode(Some(packet), &mut self.buffer, false) {
            Ok(frames) => frames,
            Err(opuscule::Error::InvalidPacket) => return Ok(()),
            Err(e) => return Err(format!("Opus: {e}")),
        };
        let start = self.skip.min(frames);
        self.skip -= start;
        let gain = self.gain;
        out.extend(
            self.buffer[start * self.channels..frames * self.channels].iter().map(|s| (s * gain).clamp(-1.0, 1.0)),
        );
        Ok(())
    }
}

// ----- output ------------------------------------------------------------------------------

/// Plays what `fill` writes (interleaved f32 at the device's rate and channels) on `device`
/// until `stop` disconnects. Runs on the calling thread.
fn play_on(
    device: &cpal::Device,
    stop: &Receiver<()>,
    fill: impl FnMut(&mut [f32], u32, usize) + Send + 'static,
) -> Result<(), String> {
    let config = device.default_output_config().map_err(|e| e.to_string())?;
    let (format, config) = (config.sample_format(), config.config());
    let (rate, channels) = (config.sample_rate, usize::from(config.channels));
    let (lost, gone) = channel();
    let stream = match format {
        SampleFormat::F32 => output_stream::<f32>(device, config, (rate, channels), fill, lost),
        SampleFormat::I16 => output_stream::<i16>(device, config, (rate, channels), fill, lost),
        SampleFormat::U16 => output_stream::<u16>(device, config, (rate, channels), fill, lost),
        SampleFormat::I32 => output_stream::<i32>(device, config, (rate, channels), fill, lost),
        other => Err(format!("the speaker's format {other} isn't supported")),
    }?;
    stream.play().map_err(|e| e.to_string())?;
    // Plays until the handle drops its sender, or the speaker goes.
    loop {
        match stop.recv_timeout(std::time::Duration::from_millis(100)) {
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Ok(error) = gone.try_recv() {
                    return Err(error);
                }
            }
            _ => return Ok(()),
        }
    }
}

/// Whether a stream error means the device is gone for good (unplugged, the sound server gone),
/// rather than a hiccup.
fn device_lost(error: &cpal::Error) -> bool {
    use cpal::ErrorKind;
    matches!(error.kind(), ErrorKind::DeviceNotAvailable | ErrorKind::HostUnavailable | ErrorKind::StreamInvalidated)
}

/// An output stream that `fill`s each block; `lost` hears when the device goes for good
/// (unplugged, the sound server gone), other trouble is only logged.
fn output_stream<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    (rate, channels): (u32, usize),
    mut fill: impl FnMut(&mut [f32], u32, usize) + Send + 'static,
    lost: Sender<String>,
) -> Result<cpal::Stream, String> {
    let mut scratch: Vec<f32> = Vec::new();
    device
        .build_output_stream(
            config,
            move |out: &mut [T], _: &cpal::OutputCallbackInfo| {
                scratch.resize(out.len(), 0.0);
                scratch.iter_mut().for_each(|s| *s = 0.0);
                fill(&mut scratch, rate, channels);
                for (out, sample) in out.iter_mut().zip(&scratch) {
                    *out = T::from_sample(*sample);
                }
            },
            move |error: cpal::Error| {
                if device_lost(&error) {
                    let _ = lost.send(error.to_string());
                } else {
                    eprintln!("better-matrix: audio output: {error}");
                }
            },
            None,
        )
        .map_err(|e| e.to_string())
}

/// A file playing (or paused) on a speaker.
#[derive(Debug, Default)]
struct Playback {
    pcm: Option<Arc<Pcm>>,
    /// Where it is, in the file's frames.
    position: f64,
    playing: bool,
}

#[derive(Debug)]
pub struct NativePlayer {
    state: Arc<Mutex<Playback>>,
    _stop: Sender<()>,
}

impl NativePlayer {
    /// Opens `path` for playing on `device` (the default when `None`); it starts paused.
    /// `failed` runs (on the player's thread) if the file can't be read or played.
    pub fn open(path: PathBuf, device: Option<String>, failed: impl FnOnce(String) + Send + 'static) -> Self {
        let state = Arc::new(Mutex::new(Playback::default()));
        let (stop, stopped) = channel();
        let shared = Arc::clone(&state);
        let _ = std::thread::Builder::new().name("audio-player".into()).spawn(move || {
            let pcm = match decode(&path) {
                Ok(pcm) => Arc::new(pcm),
                Err(error) => return failed(error),
            };
            if let Ok(mut state) = shared.lock() {
                state.pcm = Some(Arc::clone(&pcm));
            }
            let Some(device) = output_device(device.as_deref()) else {
                return failed("there is no speaker".to_owned());
            };
            let fill = move |out: &mut [f32], rate: u32, channels: usize| {
                let Ok(mut state) = shared.lock() else { return };
                if !state.playing {
                    return;
                }
                let step = f64::from(pcm.rate) / f64::from(rate);
                for frame in out.chunks_mut(channels) {
                    if state.position >= pcm.frames() as f64 {
                        state.playing = false;
                        break;
                    }
                    for (channel, out) in frame.iter_mut().enumerate() {
                        *out = pcm.sample(state.position, channel);
                    }
                    state.position += step;
                }
            };
            if let Err(error) = play_on(&device, &stopped, fill) {
                failed(error);
            }
        });
        Self { state, _stop: stop }
    }

    pub fn play(&self) {
        if let Ok(mut state) = self.state.lock() {
            let end = state.pcm.as_ref().map_or(f64::MAX, |pcm| pcm.frames() as f64);
            if state.position >= end {
                state.position = 0.0;
            }
            state.playing = true;
        }
    }

    pub fn pause(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.playing = false;
        }
    }

    pub fn is_playing(&self) -> bool {
        self.state.lock().is_ok_and(|state| state.playing)
    }

    /// Seconds played.
    pub fn time(&self) -> f64 {
        self.state.lock().ok().and_then(|s| s.pcm.as_ref().map(|pcm| s.position / f64::from(pcm.rate))).unwrap_or(0.0)
    }

    /// The length, once decoded.
    pub fn duration(&self) -> Option<f64> {
        let state = self.state.lock().ok()?;
        state.pcm.as_ref().map(|pcm| pcm.frames() as f64 / f64::from(pcm.rate))
    }
}

/// The ring of an incoming call: a soft two-note chime every few seconds, until dropped.
#[derive(Debug)]
pub struct Ring {
    _stop: Sender<()>,
}

/// The ring's chime, at `t` seconds into its period: two notes that fade like a bell.
fn chime(t: f32) -> f32 {
    const PERIOD: f32 = 2.6;
    let t = t % PERIOD;
    let note = |start: f32, hz: f32| {
        let at = t - start;
        if !(0.0..1.2).contains(&at) {
            return 0.0;
        }
        let attack = (at / 0.01).min(1.0);
        attack * (-at * 4.5).exp() * (std::f32::consts::TAU * hz * at).sin()
    };
    0.22 * (note(0.0, 659.25) + note(0.22, 880.0))
}

impl Ring {
    /// Starts ringing on `device` (the default when `None`).
    pub fn start(device: Option<String>) -> Self {
        let (stop, stopped) = channel();
        let _ = std::thread::Builder::new().name("ring".into()).spawn(move || {
            let Some(device) = output_device(device.as_deref()) else { return };
            let mut clock = 0u64;
            let fill = move |out: &mut [f32], rate: u32, channels: usize| {
                // The clock wraps with the chime, so it stays exact however long it rings.
                let period = u64::from(rate) * 26 / 10;
                for frame in out.chunks_mut(channels) {
                    let sample = chime(clock as f32 / rate as f32);
                    frame.iter_mut().for_each(|out| *out = sample);
                    clock = (clock + 1) % period.max(1);
                }
            };
            if let Err(error) = play_on(&device, &stopped, fill) {
                eprintln!("better-matrix: {error}");
            }
        });
        Self { _stop: stop }
    }
}

// ----- input -------------------------------------------------------------------------------

/// An input level, smoothed like a meter: the peak jumps up at once and falls 20 dB a second,
/// the RMS rises over about 30 ms and falls over about 300 ms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Level {
    peak_db: f32,
    rms_db: f32,
}

impl Level {
    pub const SILENT: Level = Level { peak_db: FLOOR_DB, rms_db: FLOOR_DB };

    pub fn peak_db(&self) -> f32 {
        self.peak_db
    }

    pub fn rms_db(&self) -> f32 {
        self.rms_db
    }

    /// The RMS for a level bar: -60 dBFS and below is 0, full scale is 1.
    pub fn meter(&self) -> f32 {
        ((self.rms_db + 60.0) / 60.0).clamp(0.0, 1.0)
    }

    /// The level after a block of `samples` lasting `seconds`.
    fn after(self, samples: &[f32], seconds: f32) -> Self {
        let db = |amplitude: f32| if amplitude <= 1e-8 { FLOOR_DB } else { (20.0 * amplitude.log10()).max(FLOOR_DB) };
        let peak = db(samples.iter().fold(0.0f32, |peak, s| peak.max(s.abs())));
        let rms = db((samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt());
        let fallen = self.peak_db - 20.0 * seconds;
        let speed = if rms > self.rms_db { 0.03 } else { 0.3 };
        let smoothed = self.rms_db + (rms - self.rms_db) * (1.0 - (-seconds / speed).exp());
        Self { peak_db: peak.max(fallen).max(FLOOR_DB), rms_db: smoothed.max(FLOOR_DB) }
    }
}

/// What a recording has heard so far.
#[derive(Debug, Default)]
struct Heard {
    /// Mono samples at `rate`.
    samples: Vec<f32>,
    rate: u32,
    level: Option<Level>,
    failed: Option<String>,
}

/// A finished recording.
#[derive(Clone, Debug, PartialEq)]
pub struct Recording {
    /// The file, whose extension follows its format.
    pub path: PathBuf,
    /// Seconds of audio.
    pub duration: f64,
}

/// Handles share the recording: a clone stops the same one.
#[derive(Clone)]
pub struct NativeRecorder {
    path: PathBuf,
    device: Option<String>,
    heard: Arc<Mutex<Heard>>,
    live: rune::Live<f32>,
    stop: Arc<Mutex<Option<Sender<()>>>>,
    thread: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
}

impl std::fmt::Debug for NativeRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeRecorder").field("path", &self.path).finish_non_exhaustive()
    }
}

impl NativeRecorder {
    pub fn to_file(path: PathBuf, device: Option<String>) -> Self {
        Self {
            path,
            device,
            heard: Arc::default(),
            live: rune::Live::new(0.0),
            stop: Arc::default(),
            thread: Arc::default(),
        }
    }

    pub fn start(&self) {
        let (stop, stopped) = channel();
        if let Ok(mut slot) = self.stop.lock() {
            *slot = Some(stop);
        }
        let (heard, live, device) = (Arc::clone(&self.heard), self.live.clone(), self.device.clone());
        let thread = std::thread::Builder::new().name("audio-recorder".into()).spawn(move || {
            if let Err(error) = record_on(device.as_deref(), &stopped, &heard, live)
                && let Ok(mut heard) = heard.lock()
            {
                heard.failed = Some(error);
            }
        });
        if let (Ok(thread), Ok(mut slot)) = (thread, self.thread.lock()) {
            *slot = Some(thread);
        }
    }

    /// Stops recording, waiting for the last samples when `wait`.
    fn finish(&self, wait: bool) {
        if let Ok(mut slot) = self.stop.lock() {
            slot.take();
        }
        let thread = self.thread.lock().ok().and_then(|mut slot| slot.take());
        if let Some(thread) = thread.filter(|_| wait) {
            let _ = thread.join();
        }
    }

    /// Stops and writes the file: 16-bit mono WAV. Blocking (it waits for the microphone to
    /// close and writes the file), so it runs off the UI thread.
    pub fn stop(&self) -> Result<Recording, String> {
        self.finish(true);
        let heard = self.heard.lock().map_err(|_| "the recording was lost".to_owned())?;
        if let Some(error) = &heard.failed {
            return Err(error.clone());
        }
        let samples = resample(&heard.samples, heard.rate, MEMO_RATE);
        let path = self.path.with_extension("wav");
        std::fs::write(&path, wav(&samples, MEMO_RATE)).map_err(|e| e.to_string())?;
        Ok(Recording { path, duration: samples.len() as f64 / f64::from(MEMO_RATE) })
    }

    pub fn cancel(&self) {
        self.finish(false);
    }

    /// Seconds recorded.
    pub fn elapsed(&self) -> f64 {
        self.heard.lock().map_or(0.0, |heard| heard.samples.len() as f64 / f64::from(heard.rate.max(1)))
    }

    /// Why recording stopped working (the microphone couldn't open, or went), if it did.
    pub fn failed(&self) -> Option<String> {
        self.heard.lock().ok().and_then(|heard| heard.failed.clone())
    }

    /// A recording whose microphone already failed, for tests.
    #[cfg(test)]
    pub fn failing(error: &str) -> Self {
        let recorder = Self::to_file(PathBuf::from("unused"), None);
        if let Ok(mut heard) = recorder.heard.lock() {
            heard.failed = Some(error.to_owned());
        }
        recorder
    }

    pub fn level(&self) -> Level {
        self.heard.lock().ok().and_then(|heard| heard.level).unwrap_or(Level::SILENT)
    }

    pub fn live_level(&self) -> rune::Live<f32> {
        self.live.clone()
    }
}

fn record_on(
    device: Option<&str>,
    stop: &Receiver<()>,
    heard: &Arc<Mutex<Heard>>,
    live: rune::Live<f32>,
) -> Result<(), String> {
    let device = input_device(device).ok_or("there is no microphone")?;
    let config = device.default_input_config().map_err(|e| e.to_string())?;
    let (format, config) = (config.sample_format(), config.config());
    if let Ok(mut heard) = heard.lock() {
        heard.rate = config.sample_rate;
        // A minute's room up front, so a short memo never moves its samples.
        heard.samples.reserve(config.sample_rate as usize * 60);
    }
    let stream = match format {
        SampleFormat::F32 => input_stream::<f32>(&device, config, Arc::clone(heard), live),
        SampleFormat::I16 => input_stream::<i16>(&device, config, Arc::clone(heard), live),
        SampleFormat::U16 => input_stream::<u16>(&device, config, Arc::clone(heard), live),
        SampleFormat::I32 => input_stream::<i32>(&device, config, Arc::clone(heard), live),
        other => Err(format!("the microphone's format {other} isn't supported")),
    }?;
    stream.play().map_err(|e| e.to_string())?;
    let _ = stop.recv();
    Ok(())
}

fn input_stream<T: SizedSample>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    heard: Arc<Mutex<Heard>>,
    live: rune::Live<f32>,
) -> Result<cpal::Stream, String>
where
    f32: FromSample<T>,
{
    let (channels, rate) = (usize::from(config.channels).max(1), config.sample_rate);
    let mut block: Vec<f32> = Vec::new();
    let failed = Arc::clone(&heard);
    device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                // Mono: the channels averaged.
                block.clear();
                block.extend(
                    data.chunks(channels)
                        .map(|frame| frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / channels as f32),
                );
                let Ok(mut heard) = heard.lock() else { return };
                let level = heard.level.unwrap_or(Level::SILENT).after(&block, block.len() as f32 / rate as f32);
                heard.level = Some(level);
                heard.samples.extend_from_slice(&block);
                live.set(level.meter());
            },
            // A microphone that goes fails the memo, rather than sending what came before.
            move |error: cpal::Error| {
                if device_lost(&error) {
                    if let Ok(mut heard) = failed.lock() {
                        heard.failed.get_or_insert_with(|| error.to_string());
                    }
                } else {
                    eprintln!("better-matrix: audio input: {error}");
                }
            },
            None,
        )
        .map_err(|e| e.to_string())
}

/// `samples` from `from` to `to` Hz, between neighbouring samples (averaged when shrinking).
fn resample(samples: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || from == 0 || samples.is_empty() {
        return samples.to_vec();
    }
    let ratio = f64::from(from) / f64::from(to);
    let count = (samples.len() as f64 / ratio) as usize;
    (0..count)
        .map(|i| {
            let start = i as f64 * ratio;
            let end = ((i + 1) as f64 * ratio).min(samples.len() as f64);
            let (a, b) = (start as usize, (end.ceil() as usize).max(start as usize + 1).min(samples.len()));
            samples[a..b].iter().sum::<f32>() / (b - a) as f32
        })
        .collect()
}

/// A 16-bit mono PCM WAV of `samples` at `rate`.
fn wav(samples: &[f32], rate: u32) -> Vec<u8> {
    let data = u32::try_from(samples.len() * 2).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&((sample.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    out
}

// ----- what the app holds ------------------------------------------------------------------

/// A file to play: through the chosen speaker, or Rune's player when headless.
#[derive(Debug)]
pub enum Player {
    Rune(rune::Player),
    Native(NativePlayer),
}

impl Player {
    pub fn play(&self) {
        match self {
            Self::Rune(player) => player.play(),
            Self::Native(player) => player.play(),
        }
    }

    pub fn pause(&self) {
        match self {
            Self::Rune(player) => player.pause(),
            Self::Native(player) => player.pause(),
        }
    }

    pub fn is_playing(&self) -> bool {
        match self {
            Self::Rune(player) => player.is_playing(),
            Self::Native(player) => player.is_playing(),
        }
    }

    pub fn time(&self) -> f64 {
        match self {
            Self::Rune(player) => player.time(),
            Self::Native(player) => player.time(),
        }
    }

    pub fn duration(&self) -> Option<f64> {
        match self {
            Self::Rune(player) => player.duration(),
            Self::Native(player) => player.duration(),
        }
    }
}

/// A voice memo being recorded: from the chosen microphone, or Rune's recorder (with its fake
/// microphone) when headless.
#[derive(Clone, Debug)]
pub enum Recorder {
    Rune(rune::Recorder),
    Native(NativeRecorder),
}

impl Recorder {
    pub fn start(&self) {
        match self {
            Self::Rune(recorder) => recorder.start(),
            Self::Native(recorder) => recorder.start(),
        }
    }

    pub fn stop(&self) -> Result<Recording, String> {
        match self {
            Self::Rune(recorder) => recorder.stop().map(|done| Recording { path: done.path, duration: done.duration }),
            Self::Native(recorder) => recorder.stop(),
        }
    }

    pub fn cancel(&self) {
        match self {
            Self::Rune(recorder) => recorder.cancel(),
            Self::Native(recorder) => recorder.cancel(),
        }
    }

    pub fn elapsed(&self) -> f64 {
        match self {
            Self::Rune(recorder) => recorder.elapsed(),
            Self::Native(recorder) => recorder.elapsed(),
        }
    }

    /// Why recording stopped working, if it did; Rune's fake microphone never fails.
    pub fn failed(&self) -> Option<String> {
        match self {
            Self::Rune(_) => None,
            Self::Native(recorder) => recorder.failed(),
        }
    }

    pub fn level(&self) -> Level {
        match self {
            Self::Rune(recorder) => {
                let level = recorder.level();
                Level { peak_db: level.peak_db(), rms_db: level.rms_db() }
            }
            Self::Native(recorder) => recorder.level(),
        }
    }

    pub fn live_level(&self) -> rune::Live<f32> {
        match self {
            Self::Rune(recorder) => recorder.live_level(),
            Self::Native(recorder) => recorder.live_level(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_memo_should_shrink_to_its_rate_keeping_its_length() {
        let samples = vec![0.5; 48_000];
        let out = resample(&samples, 48_000, MEMO_RATE);
        assert_eq!(out.len(), 24_000);
        assert!(out.iter().all(|s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn a_wav_should_carry_its_rate_and_samples() {
        let out = wav(&[0.0, 1.0], 24_000);
        assert_eq!((&out[..4], out.len()), (&b"RIFF"[..], 48));
        assert_eq!(u32::from_le_bytes([out[24], out[25], out[26], out[27]]), 24_000);
    }

    #[test]
    fn a_written_memo_should_decode_back() {
        let dir = std::env::temp_dir().join(format!("bm-sound-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("memo.wav");
        let tone: Vec<f32> = (0..2400).map(|i| (i as f32 * 0.1).sin() * 0.5).collect();
        std::fs::write(&path, wav(&tone, MEMO_RATE)).unwrap_or_else(|_| panic!("unwritable"));
        let pcm = decode(&path).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!((pcm.rate, pcm.channels, pcm.frames()), (MEMO_RATE, 1, 2400));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_loud_block_should_raise_the_meter_at_once_and_silence_lower_it_slowly() {
        let loud = Level::SILENT.after(&[0.5; 480], 0.01);
        assert!(loud.peak_db() > -7.0);
        let after = loud.after(&[0.0; 480], 0.01);
        assert!(after.peak_db() > loud.peak_db() - 1.0);
    }

    #[test]
    fn the_ring_should_be_quiet_between_chimes() {
        assert!(chime(0.05).abs() > 0.0);
        assert_eq!(chime(2.0), 0.0);
    }
}
