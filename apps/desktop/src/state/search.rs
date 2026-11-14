//! Finding things: rooms, spaces and direct chats on this device, and people in the homeserver's
//! user directory. The top bar's search and the invite dialog share [`PeopleSearch`].

use std::time::{Duration, Instant};

use matrix_core::{Person, RequestId};
use rune::TextEdit;

/// How long the text must rest before it is searched: a word typed letter by letter is one
/// search, not one per letter.
pub const SEARCH_PAUSE: Duration = Duration::from_millis(300);

/// A query for the user directory as it is typed. A search goes out once the text rests for
/// [`SEARCH_PAUSE`] (the state's `poll` sends it); only the answer to the latest counts.
#[derive(Debug)]
pub struct PeopleSearch {
    pub field: TextEdit,
    /// The text the results are for: `None` while the text has moved on and its search hasn't
    /// gone out yet.
    searched: Option<String>,
    /// Text not searched for yet, and when it was first seen.
    resting: Option<(String, Instant)>,
    /// The search on its way, if any.
    pub request: Option<RequestId>,
    pub people: Vec<Person>,
}

impl Default for PeopleSearch {
    fn default() -> Self {
        Self {
            field: TextEdit::new(""),
            searched: Some(String::new()),
            resting: None,
            request: None,
            people: Vec::new(),
        }
    }
}

impl PeopleSearch {
    /// The text to search for at `now`: when it changed since the last search and has rested
    /// since. Clearing the text clears the results at once; results for no text (suggestions)
    /// stay until something is typed.
    pub fn due(&mut self, now: Instant) -> Option<String> {
        let text = self.field.text.trim();
        if self.searched.as_deref() == Some(text) {
            self.resting = None;
            return None;
        }
        if text.is_empty() {
            self.searched = Some(String::new());
            self.resting = None;
            self.request = None;
            self.people.clear();
            return None;
        }
        match &self.resting {
            Some((resting, since)) if resting == text && now.duration_since(*since) >= SEARCH_PAUSE => {}
            Some((resting, _)) if resting == text => return None,
            _ => {
                // The text moved on: an answer to the last search no longer fits it, and going
                // back to that text searches again.
                self.request = None;
                self.searched = None;
                self.resting = Some((text.to_owned(), now));
                return None;
            }
        }
        self.resting = None;
        self.searched = Some(text.to_owned());
        Some(text.to_owned())
    }

    /// How long until the resting text is due, if any is waiting.
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        self.resting.as_ref().map(|(_, since)| SEARCH_PAUSE.saturating_sub(now.duration_since(*since)))
    }

    /// The people found by `request`, if it is still the latest search.
    pub fn found(&mut self, request: RequestId, people: Vec<Person>) {
        if self.request == Some(request) {
            self.people = people;
        }
    }

    /// The trimmed query.
    pub fn query(&self) -> &str {
        self.field.text.trim()
    }
}

/// Whether `name` matches a typed query: every word of it starts a word of the name, ignoring
/// case ("gr ho" finds "Grace Hopper").
pub fn matches(name: &str, query: &str) -> bool {
    let name = name.to_lowercase();
    let words: Vec<&str> = name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    query.to_lowercase().split_whitespace().all(|part| words.iter().any(|word| word.starts_with(part)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_should_match_the_starts_of_words() {
        assert!(matches("Grace Hopper", "gr ho"));
        assert!(matches("#release-notes", "notes"));
        assert!(!matches("Grace Hopper", "race"));
    }

    /// Types `text` and lets it rest, returning what is due then.
    fn typed(search: &mut PeopleSearch, text: &str, at: Instant) -> Option<String> {
        search.field = TextEdit::new(text);
        search.due(at);
        search.due(at + SEARCH_PAUSE)
    }

    #[test]
    fn a_changed_query_should_be_searched_once() {
        let (mut search, now) = (PeopleSearch::default(), Instant::now());
        assert_eq!(typed(&mut search, "ada", now).as_deref(), Some("ada"));
        assert_eq!(search.due(now + SEARCH_PAUSE * 2), None);
    }

    #[test]
    fn a_query_still_being_typed_should_not_be_searched() {
        let (mut search, now) = (PeopleSearch::default(), Instant::now());
        search.field = TextEdit::new("a");
        search.due(now);
        search.field = TextEdit::new("ad");
        assert_eq!(search.due(now + SEARCH_PAUSE), None);
        assert_eq!(search.due(now + SEARCH_PAUSE * 2).as_deref(), Some("ad"));
    }

    #[test]
    fn clearing_the_query_should_clear_the_results() {
        let (mut search, now) = (PeopleSearch::default(), Instant::now());
        typed(&mut search, "ada", now);
        search.people.push(Person::default());
        search.field = TextEdit::new("  ");
        assert_eq!(search.due(now), None);
        assert!(search.people.is_empty());
    }

    #[test]
    fn suggestions_should_stay_while_nothing_is_typed() {
        let mut search = PeopleSearch { request: Some(RequestId(1)), ..PeopleSearch::default() };
        search.found(RequestId(1), vec![Person::default()]);
        assert_eq!(search.due(Instant::now()), None);
        assert_eq!(search.people.len(), 1);
    }

    #[test]
    fn an_answer_to_an_old_search_should_be_dropped() {
        let mut search = PeopleSearch { request: Some(RequestId(2)), ..PeopleSearch::default() };
        search.found(RequestId(1), vec![Person::default()]);
        assert!(search.people.is_empty());
    }

    #[test]
    fn an_answer_arriving_after_the_text_changed_should_be_dropped() {
        let (mut search, now) = (PeopleSearch::default(), Instant::now());
        typed(&mut search, "ada", now);
        search.request = Some(RequestId(1));
        search.field = TextEdit::new("grace");
        search.due(now + SEARCH_PAUSE * 2);
        search.found(RequestId(1), vec![Person::default()]);
        assert!(search.people.is_empty());
    }

    #[test]
    fn going_back_to_a_query_whose_search_was_dropped_should_search_it_again() {
        let (mut search, now) = (PeopleSearch::default(), Instant::now());
        typed(&mut search, "ada", now);
        search.field = TextEdit::new("ad");
        search.due(now + SEARCH_PAUSE);
        assert_eq!(typed(&mut search, "ada", now + SEARCH_PAUSE * 2).as_deref(), Some("ada"));
    }
}
