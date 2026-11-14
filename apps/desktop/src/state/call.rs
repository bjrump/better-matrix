//! The call under way, as the desktop shows it: the engine's [`CallInfo`], how loud everyone is,
//! the image each video streams into, and whether the room's chat shows instead of the stage.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use matrix_core::{CallInfo, VideoId};
use rune::Pixels;

use super::call_glow::CallGlow;

/// Loudness (0 to 1) from which someone counts as speaking, for their ring on the stage.
pub const SPEAKING: f32 = 0.06;
/// How long someone still counts as speaking after going quiet, so their ring doesn't flicker
/// between words.
const SPEAKING_HOLD: Duration = Duration::from_millis(400);

#[derive(Debug)]
pub struct Call {
    pub info: CallInfo,
    /// Each participant's loudness, 0 to 1, by participant key; the user's own under "".
    pub levels: HashMap<String, f32>,
    /// Who is speaking now, by participant key.
    pub speaking: HashSet<String>,
    /// When each participant was last loud enough to count as speaking.
    loud_at: HashMap<String, Instant>,
    /// The engine streams the videos into their images: while the stage shows.
    pub watching: bool,
    /// Each video shown, by ID: the image its frames stream into.
    pub videos: HashMap<VideoId, Pixels>,
    /// The room's chat shows instead of the stage.
    pub chat: bool,
    pub glow: CallGlow,
}

/// What changed in the videos when the call did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct VideoChanges {
    /// Videos to start watching.
    pub added: Vec<VideoId>,
    /// Videos no longer in the call.
    pub removed: Vec<VideoId>,
}

impl Call {
    pub fn new(info: CallInfo) -> Self {
        Self {
            info,
            levels: HashMap::new(),
            speaking: HashSet::new(),
            loud_at: HashMap::new(),
            watching: false,
            videos: HashMap::new(),
            chat: false,
            glow: CallGlow::default(),
        }
    }

    /// Every video the call carries now: the user's own previews, then everyone else's.
    fn carried(info: &CallInfo) -> Vec<VideoId> {
        let own = [info.camera, info.screen];
        let others = info.participants.iter().flat_map(|p| [p.camera, p.screen]);
        own.into_iter().chain(others).flatten().collect()
    }

    /// Takes the engine's latest view of the call; reports the videos that came and went, each
    /// added one with a fresh image.
    pub fn update(&mut self, info: CallInfo) -> VideoChanges {
        self.info = info;
        let carried: HashSet<VideoId> = Self::carried(&self.info).into_iter().collect();
        let mut changes = VideoChanges::default();
        for id in &carried {
            if !self.videos.contains_key(id) {
                self.videos.insert(*id, Pixels::new());
                changes.added.push(*id);
            }
        }
        self.videos.retain(|id, _| {
            let keep = carried.contains(id);
            if !keep {
                changes.removed.push(*id);
            }
            keep
        });
        changes.added.sort();
        changes.removed.sort();
        let present = |key: &String| key.is_empty() || self.info.participants.iter().any(|p| p.key == *key);
        self.levels.retain(|key, _| present(key));
        self.loud_at.retain(|key, _| present(key));
        changes
    }

    /// Takes everyone's loudness (0 to 255) at `now`. True when who is speaking changed, which
    /// is all the view shows of it; the glow reads the levels on its own.
    pub fn heard(&mut self, levels: Vec<(String, u8)>, now: Instant) -> bool {
        self.levels = levels.into_iter().map(|(key, level)| (key, f32::from(level) / 255.0)).collect();
        for (key, _) in self.levels.iter().filter(|(_, level)| **level >= SPEAKING) {
            self.loud_at.insert(key.clone(), now);
        }
        self.loud_at.retain(|_, at| now.duration_since(*at) < SPEAKING_HOLD);
        let speaking: HashSet<String> = self.loud_at.keys().cloned().collect();
        let changed = speaking != self.speaking;
        self.speaking = speaking;
        changed
    }

    /// The levels the glow follows: the user first, then everyone in the call's order.
    pub fn glow_levels(&self) -> Vec<f32> {
        let own = self.levels.get("").copied().unwrap_or(0.0);
        std::iter::once(own)
            .chain(self.info.participants.iter().map(|p| self.levels.get(&p.key).copied().unwrap_or(0.0)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_core::{AccountId, CallParticipant, CallPhase, CallVia, Person};

    fn info(participants: Vec<CallParticipant>) -> CallInfo {
        CallInfo {
            call: 1,
            account: AccountId::new("@me:example.org"),
            room_id: "!r".into(),
            via: CallVia::Rtc,
            phase: CallPhase::Connected,
            video: true,
            microphone: true,
            camera: None,
            screen: None,
            participants,
        }
    }

    fn person(key: &str, camera: Option<u64>) -> CallParticipant {
        CallParticipant {
            key: key.into(),
            person: Person { id: key.into(), name: key.into(), avatar: None },
            muted: false,
            camera: camera.map(VideoId),
            screen: None,
        }
    }

    #[test]
    fn a_new_camera_should_be_watched_and_a_gone_one_dropped() {
        let mut call = Call::new(info(Vec::new()));
        let first = call.update(info(vec![person("a", Some(1))]));
        assert_eq!(first.added, [VideoId(1)]);
        let second = call.update(info(vec![person("a", Some(2))]));
        assert_eq!((second.added, second.removed), (vec![VideoId(2)], vec![VideoId(1)]));
    }

    #[test]
    fn someone_starting_to_speak_should_change_the_view() {
        let mut call = Call::new(info(vec![person("a", None)]));
        let now = Instant::now();
        assert!(call.heard(vec![("a".into(), 200)], now));
        assert!(!call.heard(vec![("a".into(), 180)], now));
        assert!(call.heard(vec![("a".into(), 0)], now + SPEAKING_HOLD));
    }

    #[test]
    fn a_pause_between_words_should_not_stop_the_speaking_ring() {
        let mut call = Call::new(info(vec![person("a", None)]));
        let now = Instant::now();
        call.heard(vec![("a".into(), 200)], now);
        assert!(!call.heard(vec![("a".into(), 0)], now + Duration::from_millis(100)));
        assert!(call.speaking.contains("a"));
    }

    #[test]
    fn the_glow_should_follow_the_user_then_everyone_else() {
        let mut call = Call::new(info(vec![person("a", None), person("b", None)]));
        call.heard(vec![("b".into(), 255), (String::new(), 0)], Instant::now());
        assert_eq!(call.glow_levels(), [0.0, 0.0, 1.0]);
    }
}
