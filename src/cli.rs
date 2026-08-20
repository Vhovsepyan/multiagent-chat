//! Command-line arguments.
//!
//! Hand-rolled rather than pulling in `clap`: there is one real flag, and a
//! dependency that compiles a whole parser generator is not worth it yet. If
//! this grows past three flags, swap in `clap` with its derive feature.

use anyhow::{Result, bail};

const HELP: &str = "\
multiagent-chat — Gemini proposes, Claude critiques, Claude Code implements.

USAGE:
    multiagent-chat [--topic <TEXT>]

OPTIONS:
    --topic <TEXT>    The problem to design a solution for. If omitted, you are
                      asked for it interactively.
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
