//! The one boundary through which fleet operations run programs (`gh`, `ssh`,
//! `tailscale`). Tests replace it, so they never touch a real host or org.

#[cfg(not(windows))]
use std::process::Stdio;
use std::time::Duration;

#[cfg(not(windows))]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(not(windows))]
use tokio::process::{Child, Command};

#[cfg(windows)]
mod windows;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    pub stdout: String,
    pub stderr: String,
    pub returncode: i32,
}

impl CommandResult {
    pub fn ok(stdout: impl Into<String>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: String::new(),
            returncode: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", self.message())]
pub struct CommandError {
    pub command: Vec<String>,
    pub returncode: i32,
    pub output: String,
}

impl CommandError {
    pub fn new(command: &[String], returncode: i32, output: &str) -> Self {
        Self {
            command: command.to_vec(),
            returncode,
            output: output.trim().to_string(),
        }
    }

    fn message(&self) -> String {
        if self.output.is_empty() {
            format!("command exited {}", self.returncode)
        } else {
            self.output.clone()
        }
    }
}

/// What a non-zero exit status means to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonZeroExit {
    /// It's an error carrying the command's output.
    Error,
    /// It's returned in the result for the caller to inspect.
    Return,
}

impl NonZeroExit {
    /// The finished command's result, or an error carrying its stderr (else
    /// its stdout) when a non-zero exit is an error.
    fn check(self, command: &[String], result: CommandResult) -> Result<CommandResult, CommandError> {
        if self == Self::Error && result.returncode != 0 {
            let output = if result.stderr.trim().is_empty() {
                result.stdout.trim()
            } else {
                result.stderr.trim()
            };
            return Err(CommandError::new(command, result.returncode, output));
        }
        Ok(result)
    }
}

/// Why `program` could not be started.
fn spawn_error(command: &[String], program: &str, error: &std::io::Error) -> CommandError {
    if error.kind() == std::io::ErrorKind::NotFound {
        CommandError::new(command, 127, &format!("{program} is not installed"))
    } else {
        CommandError::new(command, 127, &format!("cannot run {program}: {error}"))
    }
}

pub trait CommandRunner: Send + Sync {
    /// Runs `command` with a deadline covering stdin, output collection, and process exit.
    fn run(
        &self,
        command: &[String],
        input: Option<&str>,
        timeout: Duration,
        non_zero: NonZeroExit,
    ) -> Result<CommandResult, CommandError>;
}

/// Runs real programs. Output is decoded lossily: Windows consoles emit
/// non-UTF-8 bytes. Timeouts cancel pipe I/O and kill Unix process groups.
/// On Windows only the direct child is killed; descendants may continue running.
/// Cleanup is bounded and best effort if the OS delays termination or I/O cancellation.
#[derive(Debug, Default, Clone, Copy)]
pub struct SubprocessRunner;

impl CommandRunner for SubprocessRunner {
    fn run(
        &self,
        command: &[String],
        input: Option<&str>,
        timeout: Duration,
        non_zero: NonZeroExit,
    ) -> Result<CommandResult, CommandError> {
        #[cfg(windows)]
        return windows::run(command, input, timeout, non_zero);

        #[cfg(not(windows))]
        // This synchronous boundary is also called from Tokio tasks. A scoped thread
        // keeps its private runtime outside any runtime entered by the caller.
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| CommandError::new(command, 1, &format!("cannot start command runtime: {e}")))?;
                    runtime.block_on(run_subprocess(command, input, timeout, non_zero))
                })
                .join()
                .expect("command runtime thread does not panic")
        })
    }
}

#[cfg(not(windows))]
async fn run_subprocess(
    command: &[String],
    input: Option<&str>,
    timeout: Duration,
    non_zero: NonZeroExit,
) -> Result<CommandResult, CommandError> {
    let Some((program, args)) = command.split_first() else {
        return Err(CommandError::new(command, 127, "empty command"));
    };
    let deadline = tokio::time::Instant::now() + timeout;
    let mut builder = Command::new(program);
    builder
        .args(args)
        // Also covers Tailscale invoked through an SSH ProxyCommand.
        .env("TAILSCALE_BE_CLI", "1")
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Descendants can retain the pipes after the immediate child exits.
    #[cfg(unix)]
    builder.process_group(0);
    let mut child = builder.spawn().map_err(|e| spawn_error(command, program, &e))?;
    // Keep this before wait() clears the child ID; the process group may outlive it.
    #[cfg(unix)]
    let process_group = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .and_then(rustix::process::Pid::from_raw);

    let result = tokio::time::timeout_at(deadline, collect_output(&mut child, input)).await;
    let result = match result {
        Ok(Ok(result)) => result,
        failure => {
            #[cfg(unix)]
            if let Some(process_group) = process_group {
                // Cleanup is best effort: the group may already have exited.
                let _ = rustix::process::kill_process_group(process_group, rustix::process::Signal::KILL);
            }
            // The timed-out future has dropped every pipe, cancelling all I/O.
            // Cleanup errors do not replace the original timeout/wait failure.
            let _ = child.start_kill();
            // Bound reaping too; kill_on_drop is a fallback if the OS delays exit.
            let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            return Err(match failure {
                Err(_) => CommandError::new(command, 124, &format!("timed out after {}s", timeout.as_secs())),
                Ok(Err(e)) => CommandError::new(command, 1, &format!("cannot wait for {program}: {e}")),
                Ok(Ok(_)) => unreachable!("successful output was handled above"),
            });
        }
    };
    non_zero.check(command, result)
}

#[cfg(not(windows))]
async fn collect_output(child: &mut Child, input: Option<&str>) -> std::io::Result<CommandResult> {
    let stdin = child.stdin.take();
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let writer = async move {
        if let Some(mut stdin) = stdin {
            // Programs that exit without reading input communicate that in their status.
            let _ = stdin.write_all(input.unwrap_or_default().as_bytes()).await;
        }
    };
    let out_reader = async move {
        let mut buf = Vec::new();
        // A read error leaves the output collected so far.
        let _ = stdout.read_to_end(&mut buf).await;
        buf
    };
    let err_reader = async move {
        let mut buf = Vec::new();
        // A read error leaves the output collected so far.
        let _ = stderr.read_to_end(&mut buf).await;
        buf
    };
    let (_, stdout, stderr, status) = tokio::join!(writer, out_reader, err_reader, child.wait());
    Ok(CommandResult {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        returncode: status?.code().unwrap_or(-1),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Instant;

    fn sh(script: &str) -> Vec<String> {
        vec!["/bin/sh".into(), "-c".into(), script.into()]
    }

    #[test]
    fn replaces_non_utf8_console_bytes() {
        let result = SubprocessRunner
            .run(
                &sh("printf 'windows: \\373\\n'"),
                None,
                Duration::from_secs(10),
                NonZeroExit::Error,
            )
            .unwrap();
        assert_eq!(result.stdout, "windows: \u{fffd}\n");
    }

    #[test]
    fn failures_carry_stderr_then_stdout() {
        let e = SubprocessRunner
            .run(
                &sh("echo out; echo bad >&2; exit 3"),
                None,
                Duration::from_secs(10),
                NonZeroExit::Error,
            )
            .unwrap_err();
        assert_eq!((e.returncode, e.to_string()), (3, "bad".to_string()));
        let e = SubprocessRunner
            .run(&sh("exit 4"), None, Duration::from_secs(10), NonZeroExit::Error)
            .unwrap_err();
        assert_eq!(e.to_string(), "command exited 4");
    }

    #[test]
    fn stdin_is_delivered_and_timeouts_kill() {
        let r = SubprocessRunner
            .run(&sh("cat"), Some("payload"), Duration::from_secs(10), NonZeroExit::Error)
            .unwrap();
        assert_eq!(r.stdout, "payload");
        let e = SubprocessRunner
            .run(&sh("sleep 5"), None, Duration::from_millis(200), NonZeroExit::Error)
            .unwrap_err();
        assert_eq!(e.returncode, 124);
    }

    #[test]
    fn exited_child_with_inherited_pipes_still_times_out() {
        for script in ["sleep 2 2>/dev/null & exit 0", "sleep 2 >/dev/null & exit 0"] {
            let started = Instant::now();
            let error = SubprocessRunner
                .run(&sh(script), None, Duration::from_millis(100), NonZeroExit::Error)
                .unwrap_err();
            assert_eq!(error, CommandError::new(&sh(script), 124, "timed out after 0s"));
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "output draining exceeded the deadline"
            );
        }
    }

    #[test]
    fn blocked_stdin_is_cancelled_even_after_child_exit() {
        let payload = "x".repeat(1024 * 1024);
        for script in ["sleep 2", "sleep 2 <&0 >/dev/null 2>&1 & exit 0"] {
            let started = Instant::now();
            let error = SubprocessRunner
                .run(
                    &sh(script),
                    Some(&payload),
                    Duration::from_millis(100),
                    NonZeroExit::Error,
                )
                .unwrap_err();
            assert_eq!(error.returncode, 124);
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "stdin writing exceeded the deadline"
            );
        }
    }

    #[test]
    fn timeout_kills_descendant_after_direct_child_exits() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("descendant-survived");
        // Pass the path as a shell argument so no path bytes become shell syntax.
        let command = vec![
            "/bin/sh".into(),
            "-c".into(),
            "(sleep 0.4; printf survived > \"$1\") & exit 0".into(),
            "fleet-process-test".into(),
            marker.to_string_lossy().into_owned(),
        ];
        let error = SubprocessRunner
            .run(&command, None, Duration::from_millis(100), NonZeroExit::Error)
            .unwrap_err();
        assert_eq!(error.returncode, 124);
        std::thread::sleep(Duration::from_millis(500));
        assert!(!marker.exists(), "the descendant survived timeout cleanup");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn synchronous_runner_can_be_called_inside_tokio() {
        let payload = "output\n".repeat(32 * 1024);
        let result = SubprocessRunner
            .run(
                &sh("cat | cat; printf error >&2; exit 7"),
                Some(&payload),
                Duration::from_secs(10),
                NonZeroExit::Return,
            )
            .unwrap();
        assert_eq!(
            result,
            CommandResult {
                stdout: payload,
                stderr: "error".into(),
                returncode: 7
            }
        );
    }

    #[test]
    fn missing_program_is_reported() {
        let e = SubprocessRunner
            .run(
                &["/nonexistent/fleet-test".into()],
                None,
                Duration::from_secs(1),
                NonZeroExit::Error,
            )
            .unwrap_err();
        assert_eq!(e.to_string(), "/nonexistent/fleet-test is not installed");
    }
}
