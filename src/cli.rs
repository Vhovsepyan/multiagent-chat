//! Command-line arguments.
//!
//! Hand-rolled rather than pulling in `clap`: there are two real flags, and a
//! dependency that compiles a whole parser generator is not worth it yet. If
//! this grows past three, swap in `clap` with its derive feature.

use anyhow::{Result, bail};

const HELP: &str = "\
multiagent-chat — Gemini proposes, Claude critiques, Claude Code implements.

USAGE:
    multiagent-chat [--topic <TEXT>]
    multiagent-chat --implement-only [--topic <TEXT>]

OPTIONS:
    --topic <TEXT>    The problem to design a solution for. If omitted, you are
                      asked for it interactively. With --implement-only it is
                      only used to suggest the project name.
    --implement-only  Skip the debate and use the SPEC.md already in the chosen
                      project. Costs no debate tokens. You are still shown the
                      spec and asked to approve before anything is built.
    -h, --help        Show this help and exit.
    -V, --version     Show the version and exit.

CONFIGURATION:
    Read from .env in the working directory. See .env.example for every
    variable and what it does.";

/// What the user asked for on the command line.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    /// `None` means "ask me interactively".
    pub topic: Option<String>,
    /// Skip straight to implementing the SPEC.md that is already there.
    pub implement_only: bool,
}

/// Parse the real process arguments.
///
/// Returns `Ok(None)` when the program printed help or version and should exit
/// without doing any work.
pub fn parse() -> Result<Option<Args>> {
    // `skip(1)` drops the path to our own binary.
    let raw: Vec<String> = std::env::args().skip(1).collect();
    parse_from(&raw)
}

/// The parsing itself, split out so it can be tested without a real process.
fn parse_from(raw: &[String]) -> Result<Option<Args>> {
    let mut args = Args::default();
    let mut iter = raw.iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{HELP}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("multiagent-chat {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--implement-only" => args.implement_only = true,
            "--topic" => match iter.next() {
                Some(value) => args.topic = Some(value.clone()),
                None => bail!("--topic needs a value, e.g. --topic \"credit applications\""),
            },
            // Also accept --topic=... , which is how many people type it.
            _ if arg.starts_with("--topic=") => {
                args.topic = Some(arg["--topic=".len()..].to_string());
            }
            _ => bail!("unknown argument {arg:?} — run with --help to see the options"),
        }
    }

    // An empty --topic "" is a mistake, not a request to use an empty topic.
    if let Some(topic) = &args.topic
        && topic.trim().is_empty()
    {
        bail!("--topic cannot be empty");
    }

    Ok(Some(args))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_of(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_arguments_means_ask_interactively() {
        let parsed = parse_from(&[]).unwrap().unwrap();
        assert_eq!(parsed.topic, None);
    }

    #[test]
    fn reads_a_separate_topic_value() {
        let parsed = parse_from(&args_of(&["--topic", "credit applications"]))
            .unwrap()
            .unwrap();
        assert_eq!(parsed.topic.as_deref(), Some("credit applications"));
    }

    #[test]
    fn reads_an_equals_form_topic() {
        let parsed = parse_from(&args_of(&["--topic=a messenger"]))
            .unwrap()
            .unwrap();
        assert_eq!(parsed.topic.as_deref(), Some("a messenger"));
    }

    #[test]
    fn implement_only_defaults_to_off() {
        let parsed = parse_from(&[]).unwrap().unwrap();
        assert!(!parsed.implement_only);
    }

    #[test]
    fn reads_the_implement_only_flag() {
        let parsed = parse_from(&args_of(&["--implement-only"]))
            .unwrap()
            .unwrap();
        assert!(parsed.implement_only);
        assert_eq!(parsed.topic, None);
    }

    /// --topic is still allowed alongside it, where it only seeds the
    /// suggested project name.
    #[test]
    fn implement_only_combines_with_topic() {
        let parsed = parse_from(&args_of(&["--implement-only", "--topic", "a messenger"]))
            .unwrap()
            .unwrap();
        assert!(parsed.implement_only);
        assert_eq!(parsed.topic.as_deref(), Some("a messenger"));
    }

    #[test]
    fn help_and_version_stop_the_run() {
        assert_eq!(parse_from(&args_of(&["--help"])).unwrap(), None);
        assert_eq!(parse_from(&args_of(&["-V"])).unwrap(), None);
    }

    #[test]
    fn a_topic_flag_with_no_value_is_an_error() {
        let err = parse_from(&args_of(&["--topic"])).unwrap_err().to_string();
        assert!(err.contains("needs a value"), "unexpected: {err}");
    }

    #[test]
    fn an_empty_topic_is_an_error() {
        let err = parse_from(&args_of(&["--topic", "   "]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("cannot be empty"), "unexpected: {err}");
    }

    #[test]
    fn an_unknown_flag_is_an_error() {
        let err = parse_from(&args_of(&["--rounds", "3"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown argument"), "unexpected: {err}");
    }
}
