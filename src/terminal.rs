//! The two terminal-world implementations of `UserHandle` (the third,
//! `ScriptedUser`, ships with the trait for tests).
//!
//! `InteractiveUser` wraps `inquire` prompts. `NonInteractiveUser` is
//! the scriptability rule made concrete: every `ask` fails with a
//! typed error naming what was needed (CI surfaces the gap instead of
//! hanging on a prompt nobody will answer), every `confirm` takes the
//! caller's default (an optional detour declined is not an error), and
//! notifications go to stderr so piped stdout stays clean.
//!
//! `auto_user` picks between them the way the rest of the CLI behaves:
//! interactive only when stdin is a real terminal and `CI` is unset.

use std::io::IsTerminal;

use async_trait::async_trait;

use crate::user::{Answer, AskError, Question, UserHandle};

/// Pick the right implementation for this invocation.
pub fn auto_user() -> Box<dyn UserHandle> {
    if std::io::stdin().is_terminal() && std::env::var_os("CI").is_none() {
        Box::new(InteractiveUser)
    } else {
        Box::new(NonInteractiveUser)
    }
}

/// Prompts on a real terminal via `inquire`.
pub struct InteractiveUser;

/// Converts any inquire failure (including the user pressing Esc) into
/// the trait's error type, keeping the prompt text for the message.
fn io_err(what: &str, source: inquire::InquireError) -> AskError {
    AskError::Io {
        what: what.to_string(),
        source: Box::new(source),
    }
}

#[async_trait]
impl UserHandle for InteractiveUser {
    // The prompts below block the thread until the user answers. In an
    // async trait that is normally a smell, but a CLI mid-interview has
    // nothing else to do — the user *is* the critical path.
    async fn ask(&self, question: Question) -> Result<Answer, AskError> {
        match question {
            Question::Select { prompt, options } => {
                let chosen = inquire::Select::new(&prompt, options.clone())
                    .prompt()
                    .map_err(|e| io_err(&prompt, e))?;
                // Select returns the chosen string; the trait speaks in
                // indexes so callers can match on position.
                let index = options
                    .iter()
                    .position(|option| *option == chosen)
                    .unwrap_or_default();
                Ok(Answer::Choice(index))
            }
            Question::MultiSelect { prompt, options } => {
                let chosen = inquire::MultiSelect::new(&prompt, options.clone())
                    .prompt()
                    .map_err(|e| io_err(&prompt, e))?;
                // Map the chosen strings back to their positions, in the
                // options' original order — callers branch on index.
                let indexes = options
                    .iter()
                    .enumerate()
                    .filter(|(_, option)| chosen.contains(*option))
                    .map(|(index, _)| index)
                    .collect();
                Ok(Answer::Choices(indexes))
            }
            Question::Text { prompt } => {
                let text = inquire::Text::new(&prompt)
                    .prompt()
                    .map_err(|e| io_err(&prompt, e))?;
                Ok(Answer::Text(text))
            }
        }
    }

    async fn confirm(&self, prompt: &str, default: bool) -> Result<bool, AskError> {
        inquire::Confirm::new(prompt)
            .with_default(default)
            .prompt()
            .map_err(|e| io_err(prompt, e))
    }

    fn notify(&self, message: &str) {
        // stderr, like every other human-facing line: it keeps the anchor on
        // the same stream as inquire's prompts (which also use stderr), and
        // it's the stream the `style` color helpers detect a terminal on.
        eprintln!("{message}");
    }

    fn is_interactive(&self) -> bool {
        true
    }
}

/// The world without a person in it: scripts, pipes, CI.
pub struct NonInteractiveUser;

#[async_trait]
impl UserHandle for NonInteractiveUser {
    async fn ask(&self, question: Question) -> Result<Answer, AskError> {
        Err(AskError::NotInteractive {
            what: question.prompt().to_string(),
        })
    }

    async fn confirm(&self, _prompt: &str, default: bool) -> Result<bool, AskError> {
        Ok(default)
    }

    fn notify(&self, message: &str) {
        eprintln!("{message}");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_non_interactive_user_fails_asks_and_defaults_confirms() {
        let user = NonInteractiveUser;
        let err = user
            .ask(Question::Text {
                prompt: "which role?".into(),
            })
            .await
            .unwrap_err();
        // The error names what was needed — that's what CI logs show.
        assert!(err.to_string().contains("which role?"));

        assert!(user.confirm("verify now?", true).await.unwrap());
        assert!(!user.confirm("verify now?", false).await.unwrap());
    }
}
