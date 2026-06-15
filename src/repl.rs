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
use reedline::{
    ColumnarMenu, Completer, DefaultPrompt, DefaultPromptSegment, Emacs, KeyCode, KeyModifiers,
    MenuBuilder, Reedline, ReedlineEvent, ReedlineMenu, Signal, Span, Suggestion,
    default_emacs_keybindings,
};

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
    // Tab opens a completion menu over the command grammar (commands,
    // subcommands, flags); pressing it again cycles entries.
    let mut keybindings = default_emacs_keybindings();
    keybindings.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu("completion_menu".to_string()),
            ReedlineEvent::MenuNext,
        ]),
    );
    let completion_menu = ColumnarMenu::default().with_name("completion_menu");
    let mut editor = Reedline::create()
        .with_completer(Box::new(AargCompleter))
        .with_menu(ReedlineMenu::EngineCompleter(Box::new(completion_menu)))
        .with_edit_mode(Box::new(Emacs::new(keybindings)));
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

/// Tab completion for the REPL, driven by the same `clap` grammar the
/// parser uses — so completions never drift from the real commands.
struct AargCompleter;

/// A positional argument some commands accept whose values aren't in the
/// grammar but can be looked up live: build ids and stored key labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dynamic {
    None,
    BuildIds,
    KeyLabels,
}

impl Completer for AargCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let (start, mut values, dynamic) = candidates(line, pos);

        // Fold in live values for the positional we're on (build ids, key
        // labels). The lookup touches the filesystem, so it only runs here
        // (on Tab), never in the pure grammar walk above.
        let word = &line[start.min(line.len())..pos.min(line.len())];
        let live = match dynamic {
            Dynamic::None => Vec::new(),
            Dynamic::BuildIds => build_ids(),
            Dynamic::KeyLabels => key_labels(),
        };
        values.extend(live.into_iter().filter(|value| value.starts_with(word)));
        values.sort();
        values.dedup();

        values
            .into_iter()
            .map(|value| Suggestion {
                value,
                description: None,
                style: None,
                extra: None,
                span: Span::new(start, pos),
                append_whitespace: true,
                display_override: None,
                match_indices: None,
            })
            .collect()
    }
}

/// Stored build ids, newest-first as `history` lists them. An unreadable
/// builds directory just yields no completions.
fn build_ids() -> Vec<String> {
    crate::history::list()
        .map(|builds| builds.into_iter().map(|build| build.id).collect())
        .unwrap_or_default()
}

/// Labels of the API keys recorded in config. An unreadable config yields
/// no completions.
fn key_labels() -> Vec<String> {
    crate::config::Config::load()
        .map(|config| config.anthropic.keys)
        .unwrap_or_default()
}

/// The completion candidates for `line` at byte position `pos`, plus the
/// byte index where the word under the cursor starts (the span to replace).
/// Pure, so the grammar walk is unit-testable without a terminal.
///
/// It walks the clap command tree by the words already typed: at the root
/// it offers commands (and the REPL built-ins); after a command it offers
/// that command's subcommands; a word starting with `-` offers long flags.
/// Most positional values aren't completed, but a few are looked up live
/// (see the returned [`Dynamic`]): build ids and stored key labels.
fn candidates(line: &str, pos: usize) -> (usize, Vec<String>, Dynamic) {
    let prefix = &line[..pos.min(line.len())];
    // The word under the cursor begins just after the last whitespace.
    let word_start = prefix
        .rfind(char::is_whitespace)
        .map(|i| i + 1)
        .unwrap_or(0);
    let word = &prefix[word_start..];
    // The command path already typed before the current word.
    let path: Vec<&str> = prefix[..word_start].split_whitespace().collect();

    // Descend the command tree as far as the path matches subcommands,
    // counting how many words were consumed getting there.
    let root = Cli::command();
    let mut command = &root;
    let mut consumed = 0;
    for token in &path {
        match command
            .get_subcommands()
            .find(|sub| sub.get_name() == *token || sub.get_all_aliases().any(|a| a == *token))
        {
            Some(sub) => {
                command = sub;
                consumed += 1;
            }
            // A positional or flag value: stop — subcommands end here.
            None => break,
        }
    }

    // Words typed under the command that aren't flags are positional values
    // already given; their count is the index of the one being completed.
    let positional_index = path[consumed..]
        .iter()
        .filter(|token| !token.starts_with('-'))
        .count();
    let dynamic = dynamic_for(command.get_name(), positional_index, word);

    let mut pool: Vec<String> = Vec::new();
    if word.starts_with('-') {
        // Completing a flag: this command's long flags.
        for arg in command.get_arguments() {
            if let Some(long) = arg.get_long() {
                pool.push(format!("--{long}"));
            }
        }
    } else {
        // Completing a (sub)command name.
        for sub in command.get_subcommands() {
            pool.push(sub.get_name().to_string());
        }
        // At the very start, the REPL's own built-ins are valid too.
        if path.is_empty() {
            for builtin in ["help", "exit", "quit"] {
                pool.push(builtin.to_string());
            }
        }
    }

    pool.retain(|candidate| candidate.starts_with(word));
    pool.sort();
    pool.dedup();
    (word_start, pool, dynamic)
}

/// Which live-looked-up values, if any, complete the `positional_index`-th
/// positional of `command`. Pure (no I/O): the actual lookup happens in the
/// `Completer` impl, so this stays unit-testable. A flag word completes no
/// positional.
fn dynamic_for(command: &str, positional_index: usize, word: &str) -> Dynamic {
    if word.starts_with('-') {
        return Dynamic::None;
    }
    match command {
        // `attack <build>` and `diff <from> <to>` take build ids.
        "attack" if positional_index == 0 => Dynamic::BuildIds,
        "diff" if positional_index <= 1 => Dynamic::BuildIds,
        // `history rm <ids...>` is variadic — keep offering build ids.
        "rm" => Dynamic::BuildIds,
        // `key use <label>` and `key remove <label>` take stored labels.
        "use" | "remove" if positional_index == 0 => Dynamic::KeyLabels,
        _ => Dynamic::None,
    }
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

    // Helper: just the candidate values for a line completed at its end.
    fn complete(line: &str) -> Vec<String> {
        candidates(line, line.len()).1
    }

    #[test]
    fn empty_line_offers_commands_and_builtins() {
        let values = complete("");
        assert!(values.contains(&"tailor".to_string()));
        assert!(values.contains(&"key".to_string()));
        // REPL built-ins are valid first words too.
        assert!(values.contains(&"help".to_string()));
        assert!(values.contains(&"exit".to_string()));
    }

    #[test]
    fn a_prefix_narrows_the_top_level_commands() {
        let values = complete("co");
        assert!(values.contains(&"config".to_string()));
        assert!(values.contains(&"completions".to_string()));
        // A non-matching command is filtered out.
        assert!(!values.contains(&"tailor".to_string()));
    }

    #[test]
    fn after_a_command_its_subcommands_are_offered() {
        let (start, values, _) = candidates("key ", 4);
        // The new word starts at the cursor (nothing to replace yet).
        assert_eq!(start, 4);
        for sub in ["list", "add", "use", "remove"] {
            assert!(values.contains(&sub.to_string()), "missing {sub}");
        }
        // Built-ins do not leak past the first word.
        assert!(!values.contains(&"help".to_string()));
    }

    #[test]
    fn a_subcommand_prefix_narrows_and_reports_its_span() {
        let (start, values, _) = candidates("key re", 6);
        // The span replaces the partial word "re" (starts at byte 4).
        assert_eq!(start, 4);
        assert_eq!(values, vec!["remove".to_string()]);
    }

    #[test]
    fn a_dash_offers_long_flags_of_the_current_command() {
        let values = complete("tailor --");
        assert!(values.contains(&"--variant".to_string()));
        assert!(values.contains(&"--template".to_string()));
        // Subcommand names are not offered when completing a flag.
        assert!(!values.contains(&"key".to_string()));
    }

    // Helper: the dynamic-value intent for a line completed at its end.
    fn dynamic(line: &str) -> Dynamic {
        candidates(line, line.len()).2
    }

    #[test]
    fn build_id_positionals_are_flagged_for_live_lookup() {
        // `attack <build>`: first positional.
        assert_eq!(dynamic("attack "), Dynamic::BuildIds);
        assert_eq!(dynamic("attack 02"), Dynamic::BuildIds);
        // `diff <from> <to>`: both positionals.
        assert_eq!(dynamic("diff "), Dynamic::BuildIds);
        assert_eq!(dynamic("diff 020 "), Dynamic::BuildIds);
        // `history rm <ids...>`: variadic, stays on build ids.
        assert_eq!(dynamic("history rm "), Dynamic::BuildIds);
        assert_eq!(dynamic("history rm 019 "), Dynamic::BuildIds);
    }

    #[test]
    fn key_label_positionals_are_flagged_for_live_lookup() {
        assert_eq!(dynamic("key use "), Dynamic::KeyLabels);
        assert_eq!(dynamic("key remove pe"), Dynamic::KeyLabels);
        // `key add <label>` names a NEW label, so nothing to suggest.
        assert_eq!(dynamic("key add "), Dynamic::None);
    }

    #[test]
    fn commands_without_completable_positionals_stay_static() {
        // A JD path is not completed from the grammar.
        assert_eq!(dynamic("tailor "), Dynamic::None);
        // A flag word never triggers a positional lookup.
        assert_eq!(dynamic("attack -"), Dynamic::None);
        // Past the second positional, diff has no more ids to give.
        assert_eq!(dynamic("diff 020 021 "), Dynamic::None);
    }
}
