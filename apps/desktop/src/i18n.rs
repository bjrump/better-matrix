//! Translations. The catalogs are `i18n/app/<locale>.json`, compiled in by `build.rs`; English is
//! the source and fallback. Look messages up with [`t!`] and fill placeholders with [`tf!`]; a key
//! missing from the English catalog fails the build.

use rune_i18n::I18n;

include!(concat!(env!("OUT_DIR"), "/i18n.rs"));

/// The app's translator. Its locale is process-wide: [`Language::apply`] switches every later
/// lookup.
pub static I18N: I18n = I18n::new(&CATALOG);

/// The message for a key, in the current language.
macro_rules! t {
    ($key:literal) => {
        $crate::i18n::I18N.text(const { ::rune_i18n::Key::named($crate::i18n::KEYS, $key) })
    };
}

/// The message for a key with its `{{name}}` placeholders filled: `tf!("key", name = value)`.
macro_rules! tf {
    ($key:literal, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::I18N.format(
            const { ::rune_i18n::Key::named($crate::i18n::KEYS, $key) },
            &[$((stringify!($name), &$value as &dyn ::std::fmt::Display)),+],
        )
    };
}

/// The plural form for `count` in the current language, with `{{count}}` filled in:
/// `tp!("overview.rooms", n)` for `overview.rooms.one` / `overview.rooms.other`.
macro_rules! tp {
    ($key:literal, $count:expr) => {
        $crate::i18n::I18N.plural_format(
            const { ::rune_i18n::PluralKey::named($crate::i18n::PLURALS, $key) },
            $count as u64,
            &[],
        )
    };
}

pub(crate) use {t, tf, tp};

/// The language the user chose: the system's, or one of the catalog's locales by ID.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Language {
    #[default]
    System,
    Locale(&'static str),
}

impl Language {
    /// Every choice the settings offer: the system's, then each catalog locale.
    pub fn all() -> impl Iterator<Item = Language> {
        std::iter::once(Language::System).chain(I18N.locales().iter().map(|locale| Language::Locale(locale.id())))
    }

    /// Reads a stored choice; unknown IDs mean the system's language.
    pub fn from_id(id: &str) -> Self {
        I18N.find_locale(id).map_or(Language::System, |index| Language::Locale(I18N.locales()[index].id()))
    }

    /// How the choice is stored.
    pub fn id(self) -> &'static str {
        match self {
            Language::System => "system",
            Language::Locale(id) => id,
        }
    }

    /// The locale index this choice resolves to on this machine.
    pub fn resolve(self) -> usize {
        match self {
            Language::System => I18N.negotiate(rune_i18n::system_languages().iter().map(String::as_str)),
            Language::Locale(id) => I18N.find_locale(id).unwrap_or(0),
        }
    }

    /// Switches the app to this language.
    pub fn apply(self) {
        I18N.set_locale(self.resolve());
    }

    /// The name shown in the language picker, in that language itself ("Deutsch", "Français").
    pub fn label(self) -> String {
        match self {
            Language::System => tf!("settings.language_system", language = I18N.locales()[self.resolve()].label()),
            Language::Locale(id) => {
                I18N.find_locale(id).map_or_else(|| id.to_owned(), |index| I18N.locales()[index].label().to_owned())
            }
        }
    }
}

/// The system's region (ISO 3166, e.g. `DE`), from its first language tag that names one: where
/// a phone number without a country code is dialled from.
pub fn system_region() -> Option<String> {
    rune_i18n::system_languages().iter().find_map(|tag| region_of(tag))
}

/// The region subtag of a language tag (`de-DE`, `fr_CA.UTF-8`, `zh-Hant-TW`), upper-cased.
fn region_of(tag: &str) -> Option<String> {
    tag.split(['-', '_', '.'])
        .skip(1)
        .find(|part| part.len() == 2 && part.chars().all(|c| c.is_ascii_alphabetic()))
        .map(str::to_ascii_uppercase)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn a_language_tag_should_yield_its_region() {
        assert_eq!(region_of("de-DE").as_deref(), Some("DE"));
        assert_eq!(region_of("fr_ca.UTF-8").as_deref(), Some("CA"));
        assert_eq!(region_of("zh-Hant-TW").as_deref(), Some("TW"));
    }

    #[test]
    fn a_language_tag_without_a_region_should_yield_none() {
        assert_eq!(region_of("en"), None);
    }

    fn keys(json: &str) -> BTreeSet<String> {
        let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(json).unwrap();
        map.into_iter().map(|(key, _)| key).collect()
    }

    const EN: &str = include_str!("../i18n/app/en.json");

    #[test]
    fn german_should_translate_every_english_key() {
        assert_eq!(keys(include_str!("../i18n/app/de.json")), keys(EN));
    }

    #[test]
    fn french_should_translate_every_english_key() {
        assert_eq!(keys(include_str!("../i18n/app/fr.json")), keys(EN));
    }

    #[test]
    fn english_german_and_french_should_be_offered() {
        let ids: Vec<_> = I18N.locales().iter().map(|l| l.id()).collect();
        assert_eq!(ids, ["en", "de", "fr"]);
    }

    #[test]
    fn a_stored_language_should_read_back() {
        assert_eq!(Language::from_id("de"), Language::Locale("de"));
    }

    #[test]
    fn an_unknown_stored_language_should_mean_the_system_language() {
        assert_eq!(Language::from_id("xx"), Language::System);
    }

    #[test]
    fn plurals_should_pick_the_form_for_the_count() {
        let rooms = const { rune_i18n::PluralKey::named(PLURALS, "overview.rooms") };
        assert_eq!(I18N.plural_form(rooms, rune_i18n::PluralCategory::One), "{{count}} room");
    }

    #[test]
    fn messages_should_come_from_the_requested_locale() {
        let key = const { rune_i18n::Key::named(KEYS, "settings.title") };
        let de = I18N.find_locale("de").unwrap();
        assert_eq!(I18N.text_in(de, key), "Einstellungen");
    }
}
