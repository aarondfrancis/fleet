//! Running local command-line tools (`gh`, `tailscale`) with a time limit.

use std::process::Output;
use std::time::Duration;

use tokio::process::Command;

/// How long a local CLI may take before the app gives up on it.
pub(crate) const CLI_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, thiserror::Error)]
pub(crate) enum CliError {
    #[error("{0}")]
    Spawn(#[from] std::io::Error),
    #[error("The command didn't respond within {} seconds.", .0.as_secs())]
    TimedOut(Duration),
}

/// Run `command` to completion, killing it if it takes longer than `limit`.
pub(crate) async fn output(mut command: Command, limit: Duration) -> Result<Output, CliError> {
    command.kill_on_drop(true);
    tokio::time::timeout(limit, command.output())
        .await
        .map_err(|_| CliError::TimedOut(limit))?
        .map_err(CliError::Spawn)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_hung_command_is_killed_at_the_limit() {
        let mut command = Command::new("sleep");
        command.arg("30");
        let started = std::time::Instant::now();
        let error = output(command, Duration::from_millis(100)).await.unwrap_err();
        assert!(matches!(error, CliError::TimedOut(_)), "{error:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_prompt_command_returns_its_output() {
        let mut command = Command::new("echo");
        command.arg("ok");
        let out = output(command, CLI_TIMEOUT).await.unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok");
    }
}
