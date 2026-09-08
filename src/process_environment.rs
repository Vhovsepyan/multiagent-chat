//! Minimal inherited environment for external tools, not an execution sandbox.
//!
//! Keep path/runtime settings explicit. Do not pass provider, database, cloud,
//! proxy, shell startup, or tool-injection variables through this boundary.

use std::ffi::{OsStr, OsString};

const RUNTIME_VARIABLES: &[&str] = &[
    "PATH",
    "HOME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "TEMP",
    "TMP",
    "TMPDIR",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMW6432",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    "TERM",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "JAVA_HOME",
];

fn runtime_variables(
    variables: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    variables
        .into_iter()
        .filter(|(name, _)| {
            name.to_str().is_some_and(|name| {
                RUNTIME_VARIABLES.iter().any(|allowed| {
                    if cfg!(windows) {
                        name.eq_ignore_ascii_case(allowed)
                    } else {
                        name == *allowed
                    }
                })
            })
        })
        .collect()
}

pub fn command(program: impl AsRef<OsStr>) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    command
        .env_clear()
        .envs(runtime_variables(std::env::vars_os()));
    command
}

pub fn async_command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    command(program).into()
}

/// Claude uses the configured Anthropic key; no other inherited provider
/// configuration (including endpoint overrides or alternate tokens) is allowed.
pub fn implementer_command(
    program: impl AsRef<OsStr>,
    anthropic_api_key: &str,
) -> tokio::process::Command {
    let mut command = async_command(program);
    command.env("ANTHROPIC_API_KEY", anthropic_api_key);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_runtime_variables_and_excludes_unrelated_secrets() {
        let allowed = [
            "PATH",
            "HOME",
            "USERPROFILE",
            "TEMP",
            "TMP",
            "SYSTEMROOT",
            "JAVA_HOME",
        ];
        let denied = [
            "DATABASE_URL",
            "PGPASSWORD",
            "GEMINI_API_KEY",
            "ANTHROPIC_API_KEY",
            "GITHUB_TOKEN",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "AWS_SECRET_ACCESS_KEY",
            "INTERNAL_SERVICE_SECRET",
            "NODE_OPTIONS",
            "BASH_ENV",
            "PYTHONPATH",
            "GIT_CONFIG_COUNT",
            "ANTHROPIC_BASE_URL",
            "CLAUDE_CODE_OAUTH_TOKEN",
        ];
        let filtered = runtime_variables(
            allowed
                .iter()
                .chain(&denied)
                .map(|name| (OsString::from(name), OsString::from("test-only-value"))),
        );
        for name in allowed {
            assert!(
                filtered
                    .iter()
                    .any(|(key, value)| key == name && value == "test-only-value")
            );
        }
        for name in denied {
            assert!(!filtered.iter().any(|(key, _)| key == name));
        }
    }

    #[test]
    fn implementer_has_only_the_explicit_provider_credential() {
        let command = implementer_command("unused-test-command", "test-only-anthropic");
        let env: Vec<_> = command.as_std().get_envs().collect();
        assert!(env.iter().any(|(key, value)| *key == "ANTHROPIC_API_KEY"
            && *value == Some(OsStr::new("test-only-anthropic"))));
        for (key, _) in env {
            assert!(
                key == "ANTHROPIC_API_KEY"
                    || RUNTIME_VARIABLES.iter().any(|name| {
                        if cfg!(windows) {
                            key.to_string_lossy().eq_ignore_ascii_case(name)
                        } else {
                            key == *name
                        }
                    })
            );
        }
    }

    #[test]
    fn windows_runtime_variable_names_are_case_insensitive() {
        let filtered = runtime_variables([(OsString::from("Path"), OsString::from("runtime"))]);
        assert_eq!(filtered.len(), usize::from(cfg!(windows)));
    }

    // Use a subprocess rather than mutating the shared test process environment.
    // Only synthetic values are injected; no provider or external tool is used.
    #[test]
    fn clears_inherited_secrets_in_real_child_processes() {
        let status = command(std::env::current_exe().unwrap())
            .args(["--exact", "process_environment::tests::environment_probe"])
            .env("MAC_ENV_PROBE", "parent")
            .env("DATABASE_URL", "test-only-database")
            .env("GEMINI_API_KEY", "test-only-gemini")
            .env("ANTHROPIC_API_KEY", "test-only-inherited-anthropic")
            .env("GITHUB_TOKEN", "test-only-github")
            .env("GOOGLE_APPLICATION_CREDENTIALS", "test-only-cloud")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn environment_probe() {
        let Ok(role) = std::env::var("MAC_ENV_PROBE") else {
            return;
        };
        if role == "parent" {
            assert_eq!(std::env::var("DATABASE_URL").unwrap(), "test-only-database");
            for role in ["runtime", "implementer"] {
                let exe = std::env::current_exe().unwrap();
                let mut child = if role == "runtime" {
                    command(exe)
                } else {
                    implementer_command(exe, "test-only-selected-key").into_std()
                };
                let status = child
                    .args(["--exact", "process_environment::tests::environment_probe"])
                    .env("MAC_ENV_PROBE", role)
                    .status()
                    .unwrap();
                assert!(status.success());
            }
            return;
        }
        for name in [
            "DATABASE_URL",
            "GEMINI_API_KEY",
            "GITHUB_TOKEN",
            "GOOGLE_APPLICATION_CREDENTIALS",
        ] {
            assert!(
                std::env::var_os(name).is_none(),
                "unexpected inherited variable: {name}"
            );
        }
        assert!(std::env::var_os("PATH").is_some());
        if role == "implementer" {
            assert_eq!(
                std::env::var("ANTHROPIC_API_KEY").unwrap(),
                "test-only-selected-key"
            );
        } else {
            assert!(std::env::var_os("ANTHROPIC_API_KEY").is_none());
        }
    }
}
