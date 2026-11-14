//! Polls: the form that makes one from a composer, and what a press on an answer votes.

use matrix_core::{AccountId, Outgoing, Poll, TimelineKey};
use rune::TextEdit;

use super::ComposerTarget;

/// The most answers a poll may have (MSC3381).
pub const MAX_ANSWERS: usize = 20;

/// A poll being written: its question, two to twenty answers, and whether votes show while it's
/// open.
#[derive(Clone, Debug)]
pub struct PollForm {
    /// The composer it was opened from.
    pub target: ComposerTarget,
    /// The timeline the poll goes to: the composer's when it was opened.
    pub account: AccountId,
    pub key: TimelineKey,
    pub question: TextEdit,
    pub answers: Vec<TextEdit>,
    pub disclosed: bool,
}

impl Default for PollForm {
    fn default() -> Self {
        Self::new(ComposerTarget::Room, AccountId::new(""), TimelineKey::room(""))
    }
}

impl PollForm {
    /// A blank poll with two empty answers, the question ready to type into.
    pub fn new(target: ComposerTarget, account: AccountId, key: TimelineKey) -> Self {
        let mut question = TextEdit::default();
        question.focused = true;
        let answers = vec![TextEdit::default(), TextEdit::default()];
        Self { target, account, key, question, answers, disclosed: true }
    }

    /// The answers written so far, trimmed, blanks left out.
    fn written(&self) -> Vec<String> {
        self.answers.iter().map(|answer| answer.text.trim().to_owned()).filter(|answer| !answer.is_empty()).collect()
    }

    /// Whether it can be sent: a question and at least two answers.
    pub fn ready(&self) -> bool {
        !self.question.text.trim().is_empty() && self.written().len() >= 2
    }

    /// The poll to send, once ready.
    pub fn outgoing(&self) -> Option<Outgoing> {
        self.ready().then(|| Outgoing::Poll {
            question: self.question.text.trim().to_owned(),
            answers: self.written(),
            disclosed: self.disclosed,
        })
    }

    /// Adds an empty answer, focused, up to the limit.
    pub fn add_answer(&mut self) {
        if self.answers.len() < MAX_ANSWERS {
            self.question.focused = false;
            self.answers.iter_mut().for_each(|answer| answer.focused = false);
            let mut answer = TextEdit::default();
            answer.focused = true;
            self.answers.push(answer);
        }
    }

    /// Removes an answer, keeping two at least.
    pub fn remove_answer(&mut self, index: usize) {
        if self.answers.len() > 2 && index < self.answers.len() {
            self.answers.remove(index);
        }
    }

    /// The answer at `index`, or the last one when it's gone: edits from a stale row land
    /// somewhere harmless.
    pub fn answer_mut(&mut self, index: usize) -> &mut TextEdit {
        let last = self.answers.len().saturating_sub(1);
        &mut self.answers[index.min(last)]
    }
}

/// The user's vote after pressing `answer`: in a single-choice poll, that answer (or none, when
/// it was theirs, taking the vote back); in a multiple-choice one, their picks with it toggled,
/// up to the poll's limit.
pub fn next_vote(poll: &Poll, answer: &str) -> Vec<String> {
    let mine: Vec<String> = poll.answers.iter().filter(|a| a.mine).map(|a| a.id.clone()).collect();
    if mine.iter().any(|id| id == answer) {
        return mine.into_iter().filter(|id| id != answer).collect();
    }
    if poll.max_selections <= 1 {
        return vec![answer.to_owned()];
    }
    let mut picks = mine;
    if picks.len() < poll.max_selections as usize {
        picks.push(answer.to_owned());
    }
    picks
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_core::PollAnswer;

    fn poll(max: u32, mine: &[&str]) -> Poll {
        let answer = |id: &str| PollAnswer { id: id.into(), text: id.into(), votes: 0, mine: mine.contains(&id) };
        Poll {
            question: "?".into(),
            answers: vec![answer("a"), answer("b"), answer("c")],
            max_selections: max,
            ..Poll::default()
        }
    }

    #[test]
    fn a_single_choice_vote_should_move_to_the_answer_pressed() {
        assert_eq!(next_vote(&poll(1, &["a"]), "b"), ["b"]);
    }

    #[test]
    fn pressing_ones_own_answer_should_take_the_vote_back() {
        assert!(next_vote(&poll(1, &["a"]), "a").is_empty());
    }

    #[test]
    fn a_multiple_choice_vote_should_stop_at_its_limit() {
        assert_eq!(next_vote(&poll(2, &["a"]), "b"), ["a", "b"]);
        assert_eq!(next_vote(&poll(2, &["a", "b"]), "c"), ["a", "b"]);
    }

    #[test]
    fn a_poll_needs_a_question_and_two_answers() {
        let mut form = PollForm::default();
        form.question.insert("Lunch?");
        form.answers[0].insert("Pizza");
        assert!(!form.ready());
        form.answers[1].insert("  Sushi ");
        assert_eq!(
            form.outgoing(),
            Some(Outgoing::Poll {
                question: "Lunch?".into(),
                answers: vec!["Pizza".into(), "Sushi".into()],
                disclosed: true
            })
        );
    }

    #[test]
    fn a_poll_should_keep_two_answers_at_least() {
        let mut form = PollForm::default();
        form.remove_answer(0);
        assert_eq!(form.answers.len(), 2);
    }
}
