//! The human-in-the-loop seam: how runtime code asks the person a
//! question without knowing whether a person is there.
//!
//! `UserHandle` is the `LlmClient` move applied to the user: one small
//! trait, implemented once for a real terminal, once for scripted/CI
//! runs, once for tests. Code that needs an answer writes the same
//! lines in all three worlds; the *world* decides what a question
//! costs:
//!
//! - interactive: a prompt the person answers;
//! - non-interactive: `ask` fails with a typed error naming exactly
//!   what was needed (so CI surfaces the gap instead of hanging), while
//!   `confirm` quietly takes the caller's default — an optional detour
//!   declined is not an error;
//! - scripted (tests): queued answers, recorded notifications.
//!
//! The terminal-backed implementation lives in the binary crate — this
//! crate defines the capability and ships the test double, the same
//! split as `Tool` and `fetch_jd`.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;

/// Something only the user can answer.
#[derive(Debug, Clone, PartialEq)]
pub enum Question {
    /// Pick one option by index.
    Select {
        prompt: String,
        options: Vec<String>,
    },
    /// Pick any number of options by index — including none.
    MultiSelect {
        prompt: String,
        options: Vec<String>,
    },
    /// Free text; empty is a valid answer.
    Text { prompt: String },
}

impl Question {
    /// The prompt text, whatever the kind — used in error messages.
    pub fn prompt(&self) -> &str {
        match self {
            Question::Select { prompt, .. }
            | Question::MultiSelect { prompt, .. }
            | Question::Text { prompt } => prompt,
        }
    }
}

/// The user's answer, shaped like its question.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// Index into the `Select` options.
    Choice(usize),
    /// Indexes into the `MultiSelect` options; may be empty.
    Choices(Vec<usize>),
    Text(String),
}

/// Why an answer couldn't be had.
#[derive(Debug, thiserror::Error)]
pub enum AskError {
    #[error("{what:?} needs an interactive terminal")]
    NotInteractive { what: String },

    #[error("could not read the answer to {what:?}")]
    Io {
        what: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// The human-in-the-loop primitive. Implementations decide what a
/// question costs; callers just ask.
#[async_trait]
pub trait UserHandle: Send + Sync {
    /// Ask something only the user can answer. Non-interactive
    /// implementations fail this with a typed error.
    async fn ask(&self, question: Question) -> Result<Answer, AskError>;

    /// Offer an optional detour. Non-interactive implementations
    /// return `default` — declining an offer is never an error.
    async fn confirm(&self, prompt: &str, default: bool) -> Result<bool, AskError>;

    /// Tell the user something; never fails, never blocks on input.
    fn notify(&self, message: &str);

    /// Whether a real person is driving. Lets callers offer an optional
    /// interactive step only when someone can actually answer it,
    /// instead of offering it and failing the `ask`. Defaults to false;
    /// the interactive implementation overrides it.
    fn is_interactive(&self) -> bool {
        false
    }
}

/// A `UserHandle` for tests: queued answers in, recorded notifications
/// out. Ships in the library for the same reason `MockLlmClient` does —
/// downstream tests need it.
#[derive(Debug, Default)]
pub struct ScriptedUser {
    answers: Mutex<VecDeque<Answer>>,
    confirms: Mutex<VecDeque<bool>>,
    notices: Mutex<Vec<String>>,
}

impl ScriptedUser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn answer(&self, answer: Answer) {
        lock(&self.answers).push_back(answer);
    }

    pub fn confirm_with(&self, value: bool) {
        lock(&self.confirms).push_back(value);
    }

    /// Everything `notify` was given, in order.
    pub fn notices(&self) -> Vec<String> {
        lock(&self.notices).clone()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[async_trait]
impl UserHandle for ScriptedUser {
    async fn ask(&self, question: Question) -> Result<Answer, AskError> {
        lock(&self.answers)
            .pop_front()
            .ok_or_else(|| AskError::NotInteractive {
                what: question.prompt().to_string(),
            })
    }

    async fn confirm(&self, _prompt: &str, default: bool) -> Result<bool, AskError> {
        Ok(lock(&self.confirms).pop_front().unwrap_or(default))
    }

    fn notify(&self, message: &str) {
        lock(&self.notices).push(message.to_string());
    }

    /// A scripted user stands in for a present person, so flows gated on
    /// interactivity run in tests.
    fn is_interactive(&self) -> bool {
        true
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scripted_answers_replay_in_order() {
        let user = ScriptedUser::new();
        user.answer(Answer::Choice(2));
        user.answer(Answer::Text("two years".into()));

        let q = Question::Select {
            prompt: "which?".into(),
            options: vec!["a".into(), "b".into(), "c".into()],
        };
        assert_eq!(user.ask(q).await.unwrap(), Answer::Choice(2));
        let q = Question::Text {
            prompt: "how long?".into(),
        };
        assert_eq!(user.ask(q).await.unwrap(), Answer::Text("two years".into()));
    }

    #[tokio::test]
    async fn an_exhausted_script_fails_like_a_missing_terminal() {
        let user = ScriptedUser::new();
        let err = user
            .ask(Question::Text {
                prompt: "anything?".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, AskError::NotInteractive { .. }));
    }

    #[tokio::test]
    async fn unscripted_confirms_take_the_default() {
        let user = ScriptedUser::new();
        assert!(user.confirm("proceed?", true).await.unwrap());
        assert!(!user.confirm("proceed?", false).await.unwrap());
        user.confirm_with(false);
        assert!(!user.confirm("proceed?", true).await.unwrap());
    }

    #[tokio::test]
    async fn notifications_are_recorded() {
        let user = ScriptedUser::new();
        user.notify("one");
        user.notify("two");
        assert_eq!(user.notices(), vec!["one", "two"]);
    }
}
