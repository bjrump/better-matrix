//! Media encryption keys of a MatrixRTC session, as Element Call exchanges them: each device picks
//! a random key, sends it to every other member's devices in an encrypted to-device event
//! (`io.element.call.encryption_keys`), and rotates it when someone leaves. Older clients send the
//! same keys as a room event instead. Keys are matched to LiveKit participants by identity.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The event type keys travel in, to-device and (from older clients) in the room.
pub(crate) const KEYS_EVENT: &str = "io.element.call.encryption_keys";

/// How many key indexes a participant cycles through before reusing one.
const KEY_RING: u8 = u8::MAX;

/// The user's own key for the call, and its index among the keys they have used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OwnKey {
    pub index: u8,
    pub key: [u8; 16],
}

impl OwnKey {
    /// A fresh random key at index 0.
    pub fn new() -> Result<Self, getrandom::Error> {
        let mut key = [0; 16];
        getrandom::fill(&mut key)?;
        Ok(Self { index: 0, key })
    }

    /// The next key, at the next index: after a member leaves, so they can't follow on.
    pub fn rotated(&self) -> Result<Self, getrandom::Error> {
        let mut next = Self::new()?;
        next.index = (self.index + 1) % KEY_RING;
        Ok(next)
    }
}

/// A key someone sent the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReceivedKey {
    /// The sender's user ID.
    pub user_id: String,
    /// The sender's device.
    pub device_id: String,
    /// Their MatrixRTC member ID, when the event names one (for hashed identities).
    pub member_id: Option<String>,
    /// The room it is for, when the event says (to-device keys do).
    pub room_id: Option<String>,
    pub index: u8,
    pub key: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct KeyEntry {
    index: u8,
    key: String,
}

#[derive(Deserialize)]
struct RoomKeys {
    keys: Vec<KeyEntry>,
    device_id: String,
}

/// The member ID of a membership that names none: `user:device`, as Element Call's call
/// service assumes.
pub(crate) fn default_member_id(user_id: &str, device_id: &str) -> String {
    format!("{user_id}:{device_id}")
}

/// The content of the to-device event that hands `key` to another device, as matrix-js-sdk
/// sends it: the key as `media_key` and, for older readers, `keys`; who it is from as
/// `member_id` and, for older readers, `member.id`.
pub(crate) fn to_device_content(
    key: &OwnKey,
    user_id: &str,
    device_id: &str,
    room_id: &str,
    sent_ts: u64,
) -> serde_json::Value {
    let media_key = serde_json::json!({ "index": key.index, "key": STANDARD_NO_PAD.encode(key.key) });
    let member_id = default_member_id(user_id, device_id);
    serde_json::json!({
        "room_id": room_id,
        "member_id": member_id,
        "media_key": media_key,
        "keys": media_key,
        "member": { "id": member_id, "claimed_device_id": device_id },
        "session": { "application": "m.call", "call_id": "", "scope": "m.room" },
        "sent_ts": sent_ts,
    })
}

/// Reads a key from a to-device event's content, sent by `user_id`, leniently: `media_key` or
/// the older `keys`, `member_id` or the older `member.id`; the room and the sending device are
/// required.
pub(crate) fn from_to_device(user_id: &str, content: &serde_json::Value) -> Option<ReceivedKey> {
    let entry = [&content["media_key"], &content["keys"]].into_iter().find_map(|value| {
        let index = u8::try_from(value["index"].as_u64()?).ok()?;
        Some((index, decode(value["key"].as_str().filter(|key| !key.is_empty())?)?))
    })?;
    let member_id = content["member_id"].as_str().or_else(|| content["member"]["id"].as_str()).map(str::to_owned);
    Some(ReceivedKey {
        user_id: user_id.to_owned(),
        device_id: content["member"]["claimed_device_id"].as_str().filter(|device| !device.is_empty())?.to_owned(),
        member_id,
        room_id: Some(content["room_id"].as_str()?.to_owned()),
        index: entry.0,
        key: entry.1,
    })
}

/// Reads the keys from an older client's room event, sent by `user_id`.
pub(crate) fn from_room_event(user_id: &str, content: &serde_json::Value) -> Vec<ReceivedKey> {
    let Ok(keys) = serde_json::from_value::<RoomKeys>(content.clone()) else { return Vec::new() };
    keys.keys
        .into_iter()
        .filter_map(|entry| {
            Some(ReceivedKey {
                user_id: user_id.to_owned(),
                device_id: keys.device_id.clone(),
                member_id: None,
                room_id: None,
                index: entry.index,
                key: decode(&entry.key)?,
            })
        })
        .collect()
}

/// Base64, padded or not, as clients differ.
fn decode(text: &str) -> Option<Vec<u8>> {
    STANDARD_NO_PAD.decode(text.trim_end_matches('=')).or_else(|_| STANDARD.decode(text)).ok()
}

/// The LiveKit identities a member's device may join as, as matrix-js-sdk and the call service
/// derive them: `user:device` (through the older token endpoint), SHA-256 of `[user, member]`
/// (newer memberships) and of `[user, device, member]` (the newer endpoint for older
/// memberships), unpadded base64. A membership without a member ID has `user:device`.
pub(crate) fn identities(user_id: &str, device_id: &str, member_id: Option<&str>) -> Vec<String> {
    let member = member_id.map_or_else(|| default_member_id(user_id, device_id), str::to_owned);
    let hash = |parts: &[&str]| STANDARD_NO_PAD.encode(Sha256::digest(serde_json::to_vec(parts).unwrap_or_default()));
    let mut identities =
        vec![default_member_id(user_id, device_id), hash(&[user_id, &member]), hash(&[user_id, device_id, &member])];
    identities.dedup();
    identities
}

/// The user and device in an identity of the older form, `@user:server:DEVICE`.
pub(crate) fn parse_identity(identity: &str) -> Option<(&str, &str)> {
    let (user, device) = identity.rsplit_once(':')?;
    (user.starts_with('@') && user.contains(':') && !device.is_empty()).then_some((user, device))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_sent_to_a_device_should_read_back_as_sent() {
        let key = OwnKey { index: 3, key: [7; 16] };
        let content = to_device_content(&key, "@ada:example.org", "DEV", "!room:example.org", 1);
        let received = from_to_device("@ada:example.org", &content).unwrap_or_else(|| panic!("unreadable"));
        assert_eq!((received.device_id.as_str(), received.index), ("DEV", 3));
        assert_eq!((received.key, received.room_id.as_deref()), (vec![7; 16], Some("!room:example.org")));
    }

    #[test]
    fn an_older_clients_room_event_should_yield_each_of_its_keys() {
        let content = serde_json::json!({ "keys": [{ "index": 0, "key": "AAAAAAAAAAAAAAAAAAAAAA==" }], "device_id": "D", "call_id": "" });
        let keys = from_room_event("@ada:example.org", &content);
        assert_eq!(keys.len(), 1);
        assert_eq!((keys[0].key.len(), keys[0].device_id.as_str()), (16, "D"));
    }

    #[test]
    fn rotating_a_key_should_change_it_and_move_to_the_next_index() {
        let key = OwnKey::new().unwrap_or_else(|_| panic!("no randomness"));
        let next = key.rotated().unwrap_or_else(|_| panic!("no randomness"));
        assert_eq!(next.index, 1);
        assert_ne!(next.key, key.key);
    }

    #[test]
    fn the_older_identity_should_name_the_user_and_device() {
        assert_eq!(parse_identity("@ada:example.org:DEV"), Some(("@ada:example.org", "DEV")));
        assert_eq!(parse_identity("c29tZWhhc2g"), None);
    }

    #[test]
    fn a_device_should_answer_to_its_plain_and_hashed_identities() {
        // As matrix-js-sdk and the call service compute them: SHA-256 of the JSON list, unpadded.
        let hash = |json: &[u8]| STANDARD_NO_PAD.encode(Sha256::digest(json));
        let found = identities("@ada:example.org", "DEV", Some("m1"));
        assert_eq!(
            found,
            [
                "@ada:example.org:DEV".to_owned(),
                hash(br#"["@ada:example.org","m1"]"#),
                hash(br#"["@ada:example.org","DEV","m1"]"#),
            ]
        );
    }

    #[test]
    fn a_key_from_a_current_sdk_should_read_from_media_key_and_member_id() {
        let content = serde_json::json!({
            "room_id": "!r", "member_id": "uuid-1",
            "media_key": { "index": 2, "key": "AAAAAAAAAAAAAAAAAAAAAA" },
            "member": { "claimed_device_id": "DEV" }
        });
        let key = from_to_device("@ada:example.org", &content).unwrap_or_else(|| panic!("unreadable"));
        assert_eq!((key.index, key.member_id.as_deref()), (2, Some("uuid-1")));
    }

    #[test]
    fn a_key_without_a_room_should_be_ignored() {
        let content =
            serde_json::json!({ "keys": { "index": 0, "key": "AAAA" }, "member": { "claimed_device_id": "D" } });
        assert!(from_to_device("@ada:example.org", &content).is_none());
    }
}
