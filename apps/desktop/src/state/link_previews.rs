//! Link preview cards: each link's card as the homeserver of the account showing it described
//! the page (another account's server may describe it differently, or refuse), fetched for the
//! messages on screen, kept while there's room, and tried again after a wait when fetching
//! failed.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use matrix_core::AccountId;
use matrix_core::links::LinkPreview;

use super::retry::Retries;

/// How many links' cards (and failed links) are kept; past it, those not on screen go.
const KEPT: usize = 500;

/// A link as one account's homeserver is asked about it.
pub type LinkKey = (AccountId, String);

#[derive(Debug, Default)]
pub struct LinkPreviews {
    /// Each link asked about, by account: `None` while loading or when its page has nothing to
    /// show. Nested so a lookup borrows rather than builds a key.
    entries: HashMap<AccountId, HashMap<String, Option<LinkPreview>>>,
    retries: Retries<LinkKey>,
}

impl LinkPreviews {
    /// Whether `key` has to be asked about at `now`: it isn't here, and if it failed, its wait
    /// is over.
    pub fn wants(&self, key: &LinkKey, now: Instant) -> bool {
        let (account, url) = key;
        !self.entries.get(account).is_some_and(|links| links.contains_key(url)) && self.retries.may_try(key, now)
    }

    pub fn loading(&mut self, (account, url): LinkKey) {
        self.entries.entry(account).or_default().insert(url, None);
    }

    pub fn loaded(&mut self, key: LinkKey, preview: Option<LinkPreview>) {
        self.retries.succeeded(&key);
        let (account, url) = key;
        self.entries.entry(account).or_default().insert(url, preview);
    }

    /// Fetching failed: it is tried again after a wait, while shown.
    pub fn failed(&mut self, key: LinkKey, now: Instant) {
        if let Some(links) = self.entries.get_mut(&key.0) {
            links.remove(&key.1);
        }
        self.retries.failed(key, now);
    }

    pub fn next_retry(&self, now: Instant) -> Option<Instant> {
        self.retries.next_after(now)
    }

    /// The card for `url` as `account`'s homeserver described it, once there is one.
    pub fn get(&self, account: &AccountId, url: &str) -> Option<&LinkPreview> {
        self.entries.get(account)?.get(url)?.as_ref()
    }

    /// Makes room once there are too many cards, or too many failed links: only those `shown`
    /// stay.
    pub fn trim(&mut self, shown: &HashSet<LinkKey>) {
        if self.entries.values().map(HashMap::len).sum::<usize>() > KEPT {
            for (account, links) in &mut self.entries {
                links.retain(|url, _| shown.contains(&(account.clone(), url.clone())));
            }
            self.entries.retain(|_, links| !links.is_empty());
        }
        if self.retries.len() > KEPT {
            self.retries.retain(|key| shown.contains(key));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(account: &str, url: &str) -> LinkKey {
        (AccountId::new(account), url.to_owned())
    }

    #[test]
    fn a_link_should_be_asked_about_once() {
        let mut previews = LinkPreviews::default();
        let now = Instant::now();
        assert!(previews.wants(&key("@a:x", "https://a"), now));
        previews.loading(key("@a:x", "https://a"));
        assert!(!previews.wants(&key("@a:x", "https://a"), now));
        previews.loaded(key("@a:x", "https://a"), None);
        assert!(!previews.wants(&key("@a:x", "https://a"), now));
    }

    #[test]
    fn another_accounts_homeserver_should_be_asked_about_the_same_link() {
        let mut previews = LinkPreviews::default();
        previews.loaded(key("@a:x", "https://a"), None);
        assert!(previews.wants(&key("@b:y", "https://a"), Instant::now()));
    }

    #[test]
    fn a_failed_link_should_wait_before_it_is_asked_about_again() {
        let mut previews = LinkPreviews::default();
        let now = Instant::now();
        previews.loading(key("@a:x", "https://a"));
        previews.failed(key("@a:x", "https://a"), now);
        assert!(!previews.wants(&key("@a:x", "https://a"), now));
    }

    #[test]
    fn failed_links_off_screen_should_go_once_there_are_too_many() {
        let mut previews = LinkPreviews::default();
        let now = Instant::now();
        for n in 0..=KEPT {
            previews.failed(key("@a:x", &format!("https://{n}")), now);
        }
        previews.trim(&HashSet::new());
        assert_eq!(previews.retries.len(), 0);
    }
}
