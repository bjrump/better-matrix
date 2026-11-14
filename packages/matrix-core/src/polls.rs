//! Polls (MSC3381, in the unstable form clients send today): starting one, with a plain-text
//! fallback listing its answers for clients that can't show polls.

use matrix_sdk::ruma::events::AnyMessageLikeEventContent;
use matrix_sdk::ruma::events::poll::start::PollKind;
use matrix_sdk::ruma::events::poll::unstable_start::{
    NewUnstablePollStartEventContent, UnstablePollAnswer, UnstablePollAnswers, UnstablePollStartContentBlock,
    UnstablePollStartEventContent,
};

use crate::Error;

/// The event that starts a poll: answers get IDs by position.
pub(crate) fn start(question: &str, answers: &[String], disclosed: bool) -> Result<AnyMessageLikeEventContent, Error> {
    if answers.len() < 2 {
        return Err(Error::InvalidPoll);
    }
    let choices: Vec<UnstablePollAnswer> =
        answers.iter().enumerate().map(|(index, text)| UnstablePollAnswer::new(index.to_string(), text)).collect();
    let choices = UnstablePollAnswers::try_from(choices).map_err(|_| Error::InvalidPoll)?;
    let mut block = UnstablePollStartContentBlock::new(question, choices);
    block.kind = if disclosed { PollKind::Disclosed } else { PollKind::Undisclosed };
    let content = UnstablePollStartEventContent::New(NewUnstablePollStartEventContent::plain_text(
        fallback(question, answers),
        block,
    ));
    Ok(content.into())
}

/// The question, then the answers numbered, one per line.
fn fallback(question: &str, answers: &[String]) -> String {
    std::iter::once(question.to_owned())
        .chain(answers.iter().enumerate().map(|(index, answer)| format!("{}. {answer}", index + 1)))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_polls_fallback_should_list_its_answers_numbered() {
        assert_eq!(fallback("Lunch?", &["Pizza".into(), "Sushi".into()]), "Lunch?\n1. Pizza\n2. Sushi");
    }

    #[test]
    fn a_poll_with_one_answer_should_be_refused() {
        assert!(start("Lunch?", &["Pizza".into()], true).is_err());
    }
}
