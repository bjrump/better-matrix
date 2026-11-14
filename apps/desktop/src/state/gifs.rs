//! GIFs linked from messages: fetched once per link, decoded and played on a worker that streams
//! frames into a [`Pixels`], so animation redraws only the picture and never rebuilds the view.
//! Bounded: a GIF whose frames would exceed [`MAX_FRAME_BYTES`] plays only its first frame, and
//! at most [`MAX_PLAYERS`] play at once.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use image::AnimationDecoder;
use rune::Pixels;

use super::retry::Retries;

/// Decoded frames one GIF may hold.
const MAX_FRAME_BYTES: usize = 48 << 20;
/// GIFs kept playing; the oldest stop beyond this.
const MAX_PLAYERS: usize = 16;

#[derive(Debug)]
enum Entry {
    Loading,
    Playing(Player),
    /// Stopped to make room for newer ones while its timeline is still shown: not fetched
    /// again until the timeline is opened anew.
    Stopped,
    /// Downloaded, but not a GIF: the link shows as text, and it isn't fetched again.
    Broken,
}

/// One playing GIF. Dropping it stops its worker; pausing holds its frame, the worker parked
/// until resumed.
#[derive(Debug)]
pub struct Player {
    pixels: Pixels,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    worker: Option<std::thread::Thread>,
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.wake();
    }
}

impl Player {
    fn start(bytes: Arc<[u8]>) -> Self {
        let pixels = Pixels::new();
        let (stop, paused) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let (out, halt, hold) = (pixels.clone(), Arc::clone(&stop), Arc::clone(&paused));
        let worker = std::thread::Builder::new()
            .name("gif".into())
            .spawn(move || play(bytes, &out, &halt, &hold))
            .ok()
            .map(|handle| handle.thread().clone());
        Self { pixels, stop, paused, worker }
    }

    /// Holds or resumes the animation.
    fn set_paused(&self, paused: bool) {
        if self.paused.swap(paused, Ordering::Relaxed) && !paused {
            self.wake();
        }
    }

    fn wake(&self) {
        if let Some(worker) = &self.worker {
            worker.unpark();
        }
    }

    pub fn pixels(&self) -> &Pixels {
        &self.pixels
    }
}

/// Decodes the frames, lets go of the file, then loops them at their own delays until stopped,
/// holding the frame shown while paused.
fn play(bytes: Arc<[u8]>, out: &Pixels, stop: &AtomicBool, paused: &AtomicBool) {
    let Ok(decoder) = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(&bytes[..])) else { return };
    let mut frames = Vec::new();
    let mut total = 0usize;
    for frame in decoder.into_frames() {
        let Ok(frame) = frame else { break };
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        let delay = Duration::from_millis(u64::from(numerator / denominator.max(1)).max(20));
        let image = frame.into_buffer();
        total += image.as_raw().len();
        let first = frames.is_empty();
        frames.push((image.width(), image.height(), image.into_raw(), delay));
        if first {
            let (w, h, rgba, _) = &frames[0];
            out.set(*w, *h, rgba.clone());
        }
        if total > MAX_FRAME_BYTES || stop.load(Ordering::Relaxed) {
            frames.truncate(1);
            break;
        }
    }
    drop(bytes);
    if frames.len() < 2 {
        return;
    }
    for (w, h, rgba, delay) in frames.iter().cycle() {
        // Off screen or in a window in the background: hold until woken. The timeout only
        // guards against a missed wake.
        while paused.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed) {
            std::thread::park_timeout(Duration::from_secs(5));
        }
        if stop.load(Ordering::Relaxed) {
            return;
        }
        out.set(*w, *h, rgba.clone());
        wait_frame(*delay, stop, paused);
    }
}

/// Waits out a frame's delay (a GIF may ask for minutes), returning early once stopped or
/// paused: `Player` unparks the worker for both, so a dropped player lets go of its frames at
/// once rather than when the delay ends.
fn wait_frame(delay: Duration, stop: &AtomicBool, paused: &AtomicBool) {
    let deadline = Instant::now() + delay;
    while !stop.load(Ordering::Relaxed) && !paused.load(Ordering::Relaxed) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        std::thread::park_timeout(left);
    }
}

#[derive(Debug, Default)]
pub struct GifCache {
    entries: HashMap<String, Entry>,
    playing: VecDeque<String>,
    retries: Retries<String>,
    /// The GIFs on screen, as last shown.
    on_screen: std::collections::HashSet<String>,
}

impl GifCache {
    /// Whether `url` has to be fetched at `now`: it isn't here, and if it failed, its wait is
    /// over.
    pub fn wants(&self, url: &str, now: Instant) -> bool {
        !self.entries.contains_key(url) && self.retries.may_try(&url.to_owned(), now)
    }

    pub fn loading(&mut self, url: &str) {
        self.entries.insert(url.to_owned(), Entry::Loading);
    }

    /// A download failed: it is tried again after a wait, while its timeline is shown.
    pub fn failed(&mut self, url: &str, now: Instant) {
        self.entries.remove(url);
        self.retries.failed(url.to_owned(), now);
    }

    /// When the next failed download may be tried again, after `now`.
    pub fn next_retry(&self, now: Instant) -> Option<Instant> {
        self.retries.next_after(now)
    }

    /// Starts playing a downloaded GIF, if it is still wanted: one whose timeline closed while
    /// it downloaded is dropped.
    pub fn loaded(&mut self, url: String, bytes: Arc<[u8]>) {
        if !matches!(self.entries.get(&url), Some(Entry::Loading)) {
            return;
        }
        self.retries.succeeded(&url);
        if !bytes.starts_with(b"GIF8") {
            self.entries.insert(url, Entry::Broken);
            return;
        }
        self.entries.insert(url.clone(), Entry::Playing(Player::start(bytes)));
        self.playing.push_back(url);
        // Making room: the oldest off screen is forgotten, and fetched again if it comes back;
        // only when every one shows does the oldest stop, holding its place.
        while self.playing.len() > MAX_PLAYERS {
            match self.playing.iter().position(|url| !self.on_screen.contains(url)) {
                Some(index) => {
                    if let Some(gone) = self.playing.remove(index) {
                        self.entries.remove(&gone);
                    }
                }
                None => {
                    if let Some(oldest) = self.playing.pop_front() {
                        self.entries.insert(oldest, Entry::Stopped);
                    }
                }
            }
        }
    }

    /// Forgets every GIF not in `shown`, stopping it (or dropping it once downloaded): one
    /// shown again is fetched again.
    pub fn keep_only(&mut self, shown: &std::collections::HashSet<String>) {
        self.entries.retain(|url, _| shown.contains(url));
        self.playing.retain(|url| shown.contains(url));
        self.retries.retain(|url| shown.contains(url));
    }

    /// Plays the GIFs in `shown` while the window is focused, and holds every other.
    pub fn show(&mut self, shown: std::collections::HashSet<String>, focused: bool) {
        for (url, entry) in &self.entries {
            if let Entry::Playing(player) = entry {
                player.set_paused(!focused || !shown.contains(url));
            }
        }
        self.on_screen = shown;
    }

    pub fn player(&self, url: &str) -> Option<&Player> {
        match self.entries.get(url)? {
            Entry::Playing(player) => Some(player),
            _ => None,
        }
    }

    /// Whether the last download of `url` failed and isn't being tried again yet.
    pub fn has_failed(&self, url: &str) -> bool {
        // A retry still loading shows as failed, so the row keeps its height.
        match self.entries.get(url) {
            Some(Entry::Broken) => true,
            None | Some(Entry::Loading) => self.retries.has_failed(&url.to_owned()),
            Some(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn shown(urls: &[String]) -> HashSet<String> {
        urls.iter().cloned().collect()
    }

    #[test]
    fn a_gif_off_screen_should_be_the_one_to_make_room() {
        let mut gifs = GifCache::default();
        let urls: Vec<String> = (0..=MAX_PLAYERS).map(|i| format!("https://x/{i}.gif")).collect();
        gifs.show(urls[1..].iter().cloned().collect(), true);
        for url in &urls {
            gifs.loading(url);
            gifs.loaded(url.clone(), Arc::from(&b"GIF89a"[..]));
        }
        assert!(gifs.wants(&urls[0], Instant::now()) && gifs.player(&urls[1]).is_some());
    }

    #[test]
    fn a_gif_stopped_to_make_room_should_not_be_fetched_again_while_shown() {
        let mut gifs = GifCache::default();
        let urls: Vec<String> = (0..=MAX_PLAYERS).map(|i| format!("https://x/{i}.gif")).collect();
        gifs.show(urls.iter().cloned().collect(), true);
        for url in &urls {
            gifs.loading(url);
            gifs.loaded(url.clone(), Arc::from(&b"GIF89a"[..]));
        }
        gifs.keep_only(&shown(&urls));
        assert!(!gifs.wants(&urls[0], Instant::now()));
    }

    #[test]
    fn a_gif_no_longer_shown_should_be_let_go() {
        let mut gifs = GifCache::default();
        gifs.loading("https://x/a.gif");
        gifs.loaded("https://x/a.gif".into(), Arc::from(&b"GIF89a"[..]));
        gifs.keep_only(&HashSet::new());
        assert!(gifs.player("https://x/a.gif").is_none());
    }

    #[test]
    fn a_gif_downloaded_after_its_timeline_closed_should_not_play() {
        let mut gifs = GifCache::default();
        gifs.loading("https://x/a.gif");
        gifs.keep_only(&HashSet::new());
        gifs.loaded("https://x/a.gif".into(), Arc::from(&b"GIF89a"[..]));
        assert!(gifs.player("https://x/a.gif").is_none());
    }

    #[test]
    fn a_failed_gif_should_be_fetched_again_after_a_wait() {
        let (mut gifs, now) = (GifCache::default(), Instant::now());
        gifs.loading("https://x/a.gif");
        gifs.failed("https://x/a.gif", now);
        assert!(!gifs.wants("https://x/a.gif", now));
        assert!(gifs.wants("https://x/a.gif", now + Duration::from_secs(5)));
    }

    #[test]
    fn a_download_that_is_not_a_gif_should_show_as_failed() {
        let mut gifs = GifCache::default();
        gifs.loading("https://x/a.gif");
        gifs.loaded("https://x/a.gif".into(), Arc::from(&b"<html>"[..]));
        assert!(gifs.has_failed("https://x/a.gif") && !gifs.wants("https://x/a.gif", Instant::now()));
    }

    #[test]
    fn stopping_a_player_should_end_a_long_frame_delay_at_once() {
        let (stop, paused) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let (halt, hold) = (Arc::clone(&stop), Arc::clone(&paused));
        let worker = std::thread::spawn(move || wait_frame(Duration::from_secs(600), &halt, &hold));
        std::thread::sleep(Duration::from_millis(20));
        stop.store(true, Ordering::Relaxed);
        worker.thread().unpark();
        let started = Instant::now();
        worker.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
