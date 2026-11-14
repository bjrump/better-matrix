//! Signing in, as a two-step state machine:
//!
//! 1. **Homeserver**: the user types a server name, URL or Matrix ID; the engine resolves it
//!    (`.well-known` first) and reports how it signs users in.
//! 2. **Sign in**: OAuth servers go straight to the browser; password servers show username and
//!    password. A server offering both starts in the browser and offers the password instead.
//!
//! Used for the first account and every one after it.

use matrix_core::{AccountId, Homeserver, LoginRequest, RequestId};
use rune::TextEdit;

/// Where the sign-in is. Each step holds only what is valid in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInStep {
    /// Step 1: choosing a homeserver.
    Server { error: Option<String> },
    /// Step 1: resolving it.
    Discovering(RequestId),
    /// Step 2 on an OAuth server: the user finishes in the browser. `request` is `None` after a
    /// failure, until the user tries again.
    Browser { server: Homeserver, request: Option<RequestId>, url: Option<String>, error: Option<String> },
    /// Step 2 on a password server. `submitting` is the login in flight.
    Password { server: Homeserver, submitting: Option<RequestId>, error: Option<String> },
}

#[derive(Debug)]
pub struct SignInForm {
    pub homeserver: TextEdit,
    pub username: TextEdit,
    pub password: TextEdit,
    pub step: SignInStep,
    /// The account signed out softly that this sign-in takes back, keeping its data.
    pub resume: Option<AccountId>,
}

impl Default for SignInForm {
    fn default() -> Self {
        Self {
            homeserver: TextEdit::new("matrix.org"),
            username: TextEdit::new(""),
            password: TextEdit::new("").secure(),
            step: SignInStep::Server { error: None },
            resume: None,
        }
    }
}

impl SignInForm {
    /// The discovered homeserver, in step 2.
    pub fn server(&self) -> Option<&Homeserver> {
        match &self.step {
            SignInStep::Browser { server, .. } | SignInStep::Password { server, .. } => Some(server),
            SignInStep::Server { .. } | SignInStep::Discovering(_) => None,
        }
    }

    /// The password login the form describes, once both fields are filled in.
    pub fn password_request(&self) -> Option<LoginRequest> {
        Some(LoginRequest {
            homeserver: self.server()?.url.clone(),
            username: Some(self.username.text.trim().to_owned()).filter(|text| !text.is_empty())?,
            password: Some(self.password.text.clone()).filter(|text| !text.is_empty())?,
            resume: self.resume.clone(),
        })
    }

    /// The request the form is waiting on, if any.
    pub fn pending(&self) -> Option<RequestId> {
        match &self.step {
            SignInStep::Discovering(request) => Some(*request),
            SignInStep::Browser { request, .. } => *request,
            SignInStep::Password { submitting, .. } => *submitting,
            SignInStep::Server { .. } => None,
        }
    }

    pub fn error(&self) -> Option<&str> {
        match &self.step {
            SignInStep::Server { error } | SignInStep::Browser { error, .. } | SignInStep::Password { error, .. } => {
                error.as_deref()
            }
            SignInStep::Discovering(_) => None,
        }
    }

    /// Records a failed request: discovery returns to step 1, a sign-in stays on step 2.
    pub fn fail(&mut self, request: RequestId, message: String) {
        match &mut self.step {
            SignInStep::Discovering(r) if *r == request => self.step = SignInStep::Server { error: Some(message) },
            SignInStep::Browser { request: r @ Some(_), error, .. } if *r == Some(request) => {
                *r = None;
                *error = Some(message);
            }
            SignInStep::Password { submitting: r @ Some(_), error, .. } if *r == Some(request) => {
                *r = None;
                *error = Some(message);
            }
            _ => {}
        }
    }
}
