//! Effective OpenSSH settings used to bind a reviewed transport.

use std::time::Duration;

use crate::desired::process::{CommandRunner, NonZeroExit, SubprocessRunner};
use crate::desired::remote::{MAX_CONNECT_TIMEOUT_SECS, OPENSSH_OPTIONS, SSH_PROGRAM};

/// `ssh -G` can run `Match exec` commands from the user's configuration, so
/// it gets a deadline like any other program.
const EVALUATE_TIMEOUT: Duration = Duration::from_secs(10);

/// A complete effective `ssh -G` configuration, including transport and authentication options.
#[derive(Clone, PartialEq, Eq)]
pub struct SshTransportIdentity {
    hostname: String,
    configuration: Vec<String>,
    jumps: Vec<Self>,
}

impl std::fmt::Debug for SshTransportIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Proxy commands can contain credentials; never print the effective configuration.
        f.debug_struct("SshTransportIdentity").finish_non_exhaustive()
    }
}

impl SshTransportIdentity {
    /// Parse effective OpenSSH output, preserving option order and repeated options.
    pub fn from_config(output: &str) -> Option<Self> {
        let configuration: Vec<String> = output
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        let hostname = configuration
            .iter()
            .find_map(|line| line.strip_prefix("hostname "))?
            .trim()
            .to_string();
        if hostname.is_empty() {
            return None;
        }
        Some(Self {
            hostname,
            configuration,
            jumps: Vec::new(),
        })
    }

    pub(super) fn hostname(&self) -> &str {
        &self.hostname
    }
}

pub(super) fn resolve(target: &str) -> Option<SshTransportIdentity> {
    resolve_with(target, SshRole::Destination, &mut 32, &|target, role| {
        evaluate(&SubprocessRunner, target, role)
    })
}

#[derive(Clone, Copy)]
enum SshRole {
    Destination,
    Jump,
}

fn resolve_with(
    target: &str,
    role: SshRole,
    remaining: &mut usize,
    evaluate: &impl Fn(&str, SshRole) -> Option<String>,
) -> Option<SshTransportIdentity> {
    // Fail closed on cyclic or excessive jump chains instead of recursing indefinitely.
    *remaining = remaining.checked_sub(1)?;
    let mut identity = SshTransportIdentity::from_config(&evaluate(target, role)?)?;
    let jumps = identity
        .configuration
        .iter()
        .find_map(|line| line.strip_prefix("proxyjump "));
    if let Some(jumps) = jumps.filter(|value| *value != "none") {
        for jump in jumps.split(',') {
            // OpenSSH accepts SSH URIs, including user, port, and bracketed IPv6 addresses.
            let target = if jump.starts_with("ssh://") {
                jump.to_string()
            } else {
                format!("ssh://{jump}")
            };
            identity
                .jumps
                .push(resolve_with(&target, SshRole::Jump, remaining, evaluate)?);
        }
    }
    Some(identity)
}

fn evaluate(commands: &dyn CommandRunner, target: &str, role: SshRole) -> Option<String> {
    let mut command = vec![SSH_PROGRAM.to_string(), "-G".into()];
    if let SshRole::Destination = role {
        // The options `RemoteClient` connects with. Jump subprocesses evaluate their own settings.
        command.extend(OPENSSH_OPTIONS.map(String::from));
        command.extend(["-o".into(), format!("ConnectTimeout={MAX_CONNECT_TIMEOUT_SECS}")]);
    }
    command.extend(["--".into(), target.into()]);
    commands
        .run(&command, None, EVALUATE_TIMEOUT, NonZeroExit::Error)
        .ok()
        .map(|result| result.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jump_alias_configuration_changes_transport_identity() {
        let destination = "hostname fake-destination\nuser fake-ci\nport 22\nproxyjump fake-user@fake-bastion:2222\n";
        let original = "hostname fake-proxy\nuser fake-user\nport 2222\nproxycommand none\n";
        let resolve = |jump: &str| {
            resolve_with("fake-alias", SshRole::Destination, &mut 32, &|target, _| match target {
                "fake-alias" => Some(destination.into()),
                "ssh://fake-user@fake-bastion:2222" => Some(jump.into()),
                _ => None,
            })
        };
        let before = resolve(original).unwrap();
        for (option, changed) in [
            ("fake-proxy", "different-proxy"),
            ("user fake-user", "user different-user"),
            ("port 2222", "port 3333"),
            ("proxycommand none", "proxycommand fake-helper %h %p"),
        ] {
            let after = resolve(&original.replace(option, changed)).unwrap();
            assert_eq!(before.hostname(), after.hostname());
            assert_ne!(before, after, "{option}");
        }
        assert_eq!(before, resolve(original).unwrap());
    }

    #[test]
    fn unresolved_or_cyclic_jump_configuration_fails_closed() {
        let configuration = "hostname fake-destination\nproxyjump fake-bastion\n";
        assert!(
            resolve_with("fake-alias", SshRole::Destination, &mut 32, &|target, _| {
                (target == "fake-alias").then(|| configuration.into())
            })
            .is_none()
        );
        assert!(
            resolve_with("fake-alias", SshRole::Destination, &mut 32, &|_, _| Some(
                configuration.into()
            ))
            .is_none()
        );
    }

    #[test]
    fn evaluation_uses_the_connection_program_and_options_with_a_deadline() {
        let commands = crate::desired::testing::RecordingRunner::new(|_| "hostname fake-destination\n".into());
        assert_eq!(
            evaluate(commands.as_ref(), "fake-alias", SshRole::Destination).as_deref(),
            Some("hostname fake-destination\n")
        );
        evaluate(commands.as_ref(), "ssh://fake-jump", SshRole::Jump);
        let runs = commands.runs();
        let destination = &runs[0].command;
        assert_eq!(destination[..2], [SSH_PROGRAM, "-G"]);
        assert!(destination.iter().any(|a| a == "StrictHostKeyChecking=yes"));
        assert_eq!(destination[destination.len() - 2..], ["--", "fake-alias"]);
        assert_eq!(runs[1].command, [SSH_PROGRAM, "-G", "--", "ssh://fake-jump"]);
        assert!(runs.iter().all(|run| run.timeout == EVALUATE_TIMEOUT));
    }

    #[test]
    fn transport_debug_output_redacts_proxy_credentials() {
        let identity = SshTransportIdentity::from_config(
            "hostname fake-destination\nproxycommand helper --password secret-value\n",
        )
        .unwrap();
        assert!(!format!("{identity:?}").contains("secret-value"));
    }
}
