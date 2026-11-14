//! The Profile page's form: the display name being edited, and adding an email address or phone
//! number, each a small state machine from typing the address to confirming it.

use matrix_core::{AccountId, ContactKind, PendingContact, RequestId};
use rune::TextEdit;

/// Adding one contact detail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContactStep {
    /// Typing the address or number.
    Entering,
    /// The server is sending a link or code.
    Requesting(RequestId),
    /// The link or code is out; the user confirms it. `busy` while the code or the addition is
    /// on its way.
    Sent { pending: PendingContact, busy: bool },
}

/// The form for one kind of contact detail.
#[derive(Debug)]
pub struct ContactForm {
    pub kind: ContactKind,
    pub address: TextEdit,
    /// The code texted to a phone number.
    pub code: TextEdit,
    pub step: ContactStep,
    pub error: Option<String>,
}

impl ContactForm {
    fn new(kind: ContactKind) -> Self {
        Self { kind, address: TextEdit::new(""), code: TextEdit::new(""), step: ContactStep::Entering, error: None }
    }

    /// Back to an empty address, after adding or giving up.
    pub fn reset(&mut self) {
        *self = Self::new(self.kind);
    }

    /// The detail waiting for confirmation, if any.
    pub fn pending(&self) -> Option<&PendingContact> {
        match &self.step {
            ContactStep::Sent { pending, .. } => Some(pending),
            _ => None,
        }
    }

    /// Fails the current step: shows `error` and goes back one step.
    pub fn fail(&mut self, error: String) {
        self.step = match std::mem::replace(&mut self.step, ContactStep::Entering) {
            ContactStep::Sent { pending, .. } => ContactStep::Sent { pending, busy: false },
            _ => ContactStep::Entering,
        };
        self.error = Some(error);
    }
}

/// What the Profile page edits, for one account at a time.
#[derive(Debug)]
pub struct ProfileForm {
    /// Whose profile; the form starts over when another account's opens.
    pub account: Option<AccountId>,
    pub name: TextEdit,
    pub saving_name: bool,
    pub saving_avatar: bool,
    pub email: ContactForm,
    pub phone: ContactForm,
}

impl Default for ProfileForm {
    fn default() -> Self {
        Self {
            account: None,
            name: TextEdit::new(""),
            saving_name: false,
            saving_avatar: false,
            email: ContactForm::new(ContactKind::Email),
            phone: ContactForm::new(ContactKind::Phone),
        }
    }
}

impl ProfileForm {
    /// A fresh form for `account`, its name field holding `name`.
    pub fn open(account: AccountId, name: &str) -> Self {
        Self { account: Some(account), name: TextEdit::new(name), ..Self::default() }
    }

    pub fn contact(&self, kind: ContactKind) -> &ContactForm {
        match kind {
            ContactKind::Email => &self.email,
            ContactKind::Phone => &self.phone,
        }
    }

    pub fn contact_mut(&mut self, kind: ContactKind) -> &mut ContactForm {
        match kind {
            ContactKind::Email => &mut self.email,
            ContactKind::Phone => &mut self.phone,
        }
    }

    /// The contact form waiting on `request` for its link or code.
    pub fn requesting(&mut self, request: RequestId) -> Option<&mut ContactForm> {
        [&mut self.email, &mut self.phone].into_iter().find(|form| form.step == ContactStep::Requesting(request))
    }
}
