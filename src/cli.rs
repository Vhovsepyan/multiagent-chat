//! Command-line arguments.
//!
//! Hand-rolled rather than pulling in `clap`. At four flags this is past the
//! line I set for switching; `clap` derive is the right move if it grows again.

use anyhow::{Result, bail};

const HELP: &str = "\
multiagent-chat — Gemini proposes, Claude critiques, Claude Code implements.

USAGE:
    multiagent-chat                       start the web UI (default)
    multiagent-chat --cli [--topic <TEXT>]
    multiagent-chat --cli --implement-only [--topic <TEXT>]

OPTIONS:
    --topic <TEXT>    The problem to design a solution for. If omitted, you are
                      asked for it interactively. With --implement-only it is
                      only used to suggest the project name.
    --implement-only  Skip the debate and use the SPEC.md already in the chosen
                      project. Costs no debate tokens. You are still shown the
                      spec and asked to approve before anything is built.
    --web             Start the web UI. This is the default, so the flag is only
                      needed for clarity. Port comes from PORT in .env
                      (default 3000).
    --cli             Run the original terminal pipeline instead of the server.
                      Implied by --topic and --implement-only.
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
    /// Serve the web UI. The default since Phase 10 (DP-12).
    pub web: bool,
    /// Force the terminal pipeline.
    pub cli: bool,
}

impl Args {
    /// True when the terminal pipeline should run instead of the server.
    ///
    /// `--topic` and `--implement-only` only mean anything to the CLI, so
    /// passing either implies it — typing `--cli` as well would be noise.
    pub fn wants_cli(&self) -> bool {
        self.cli || self.implement_only || self.topic.is_some()
    }
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
            "--web" => args.web = true,
            "--cli" => args.cli = true,
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

    // The web server has its own lifecycle; the pipeline flags mean nothing to
    // it, and silently ignoring them would be confusing.
    if args.web && (args.cli || args.implement_only || args.topic.is_some()) {
        bail!("--web cannot be combined with --cli, --topic or --implement-only");
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
    fn no_arguments_means_the_web_ui() {
        let parsed = parse_from(&[]).unwrap().unwrap();
        assert!(!parsed.wants_cli(), "web is the default since Phase 10");
    }

    #[test]
    fn the_cli_flag_forces_the_terminal_pipeline() {
        let parsed = parse_from(&args_of(&["--cli"])).unwrap().unwrap();
        assert!(parsed.wants_cli());
    }

    /// Typing --cli alongside these would be noise, so they imply it.
    #[test]
    fn topic_and_implement_only_imply_cli() {
        assert!(
            parse_from(&args_of(&["--topic", "x"]))
                .unwrap()
                .unwrap()
                .wants_cli()
        );
        assert!(
            parse_from(&args_of(&["--implement-only"]))
                .unwrap()
                .unwrap()
                .wants_cli()
        );
    }

    #[test]
    fn web_conflicts_with_the_cli_flags() {
        let err = parse_from(&args_of(&["--web", "--cli"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("cannot be combined"), "unexpected: {err}");
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
