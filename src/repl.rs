//! The interactive REPL (FR-6.1): `aarg` with no arguments drops into a
//! `reedline` shell that runs the same commands without the `aarg` prefix.
//!
//! It is a thin wrapper over `commands::dispatch` — the exact dispatch the
//! binary uses — so every command behaves identically inside the shell:
//! streaming output, confirm-before-write, and typed errors all come for
//! free. A command's error ends that command, never the session.
//!
//! Session note: each command still loads its own dataset/config (the
//! commands are self-contained). Holding that state across the loop is a
//! later optimization; today the win is line editing, history, and not
//! re-typing `aarg` or paying process startup per command.

use std::io::IsTerminal;

use clap::{CommandFactory, Parser};
use reedline::{DefaultPrompt, DefaultPromptSegment, Reedline, Signal};

use crate::cli::{Cli, Command};
use crate::commands::{self, CliError};
use crate::style;

/// One interpreted line of input: a built-in, a parseable command, or an
/// error to show. `Command` is boxed because it is a large enum.
#[derive(Debug)]
enum Line {
    Empty,
    Exit,
    Help,
    Run(Box<Command>),
    Error(String),
}

/// Interpret a typed line into a built-in, a command, or an error — with no
/// I/O, so the parsing is unit-testable apart from the reedline loop.
fn interpret(line: &str) -> Line {
    match line.trim() {
        "" => return Line::Empty,
        "exit" | "quit" => return Line::Exit,
        "help" | "?" => return Line::Help,
        _ => {}
    }
    let tokens = match shell_words::split(line) {
        Ok(tokens) => tokens,
        Err(e) => return Line::Error(format!("unbalanced quotes: {e}\n")),
    };
    // Parse as if it were `aarg <tokens>`, reusing the whole clap grammar.
    match Cli::try_parse_from(std::iter::once("aarg".to_string()).chain(tokens)) {
        Ok(cli) => match cli.command {
            Some(command) => Line::Run(Box::new(command)),
            None => Line::Empty,
        },
        // clap's error Display carries the usage message; show it and continue.
        Err(e) => Line::Error(e.to_string()),
    }
}

/// Run the interactive shell until the user exits (`exit`/`quit`, Ctrl-C,
/// or Ctrl-D).
pub async fn run() -> Result<(), CliError> {
    // The shell is a line editor: it needs a real terminal. A piped or CI
    // invocation of bare `aarg` gets a pointer to subcommands instead of a
    // raw-mode error.
    if !std::io::stdin().is_terminal() {
        eprintln!(
            "{}",
            style::dim(
                "`aarg` with no command starts the interactive shell, which needs a terminal."
            )
        );
        eprintln!(
            "{}",
            style::dim("run a subcommand instead, e.g. `aarg tailor <jd>` — see `aarg --help`.")
        );
        return Ok(());
    }

    eprintln!("{}", style::bold("aarg interactive shell"));
    eprintln!(
        "{}",
        style::dim("run any command without the `aarg` prefix  ·  `help`  ·  `exit`")
    );
    let mut editor = Reedline::create();
    let prompt = DefaultPrompt::new(
        DefaultPromptSegment::Basic("aarg".to_string()),
        DefaultPromptSegment::Empty,
    );

    loop {
        match editor.read_line(&prompt) {
            Ok(Signal::Success(line)) => match interpret(&line) {
                Line::Empty => {}
                Line::Exit => break,
                Line::Help => eprintln!("{}", Cli::command().render_long_help()),
                Line::Error(message) => eprint!("{message}"),
                Line::Run(command) => {
                    // A command error ends the command, not the session.
                    if let Err(error) = commands::dispatch(*command).await {
                        eprintln!("{:?}", miette::Report::new(error));
                    }
                }
            },
            Ok(Signal::CtrlC | Signal::CtrlD) => break,
            // `Signal` is non-exhaustive; ignore any future variant.
            Ok(_) => {}
            Err(error) => {
                eprintln!("{} {error}", style::red("input error:"));
                break;
            }
        }
    }
    eprintln!("{}", style::dim("bye"));
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn blanks_and_builtins() {
        assert!(matches!(interpret(""), Line::Empty));
        assert!(matches!(interpret("   "), Line::Empty));
        assert!(matches!(interpret("exit"), Line::Exit));
        assert!(matches!(interpret("quit"), Line::Exit));
        assert!(matches!(interpret("help"), Line::Help));
        assert!(matches!(interpret("?"), Line::Help));
    }

    #[test]
    fn a_command_parses_without_the_aarg_prefix() {
        match interpret("tailor jd.txt --variant human") {
            Line::Run(command) => assert!(matches!(*command, Command::Tailor { .. })),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn quoted_arguments_are_tokenized() {
        match interpret(r#"ingest "my resume.md""#) {
            Line::Run(command) => match *command {
                Command::Ingest { path } => {
                    assert_eq!(path, std::path::PathBuf::from("my resume.md"));
                }
                other => panic!("expected ingest, got {other:?}"),
            },
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_or_incomplete_command_is_an_error_not_a_panic() {
        assert!(matches!(interpret("bogus-command"), Line::Error(_)));
        // `tailor` without its required JD is a clap error, surfaced, not fatal.
        assert!(matches!(interpret("tailor"), Line::Error(_)));
        // An unbalanced quote is reported, not a crash.
        assert!(matches!(interpret(r#"ingest "oops"#), Line::Error(_)));
    }
}
