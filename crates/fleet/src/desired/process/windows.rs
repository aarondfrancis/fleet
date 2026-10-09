//! Windows anonymous pipes use synchronous I/O, including through Tokio.
//! Cancel their dedicated workers explicitly rather than leaving runtime shutdown blocked.

use std::io::{Read, Write};
use std::os::windows::io::AsRawHandle;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::{CommandError, CommandResult, NonZeroExit, spawn_error};

use windows_sys::Win32::System::IO::CancelSynchronousIo;

struct PipeWorker<T> {
    thread: Option<JoinHandle<T>>,
    cancelled: Arc<AtomicBool>,
}

impl<T: Send + 'static> PipeWorker<T> {
    fn spawn(work: impl FnOnce(&AtomicBool) -> T + Send + 'static) -> Self {
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        Self {
            thread: Some(std::thread::spawn(move || work(&worker_cancelled))),
            cancelled,
        }
    }

    fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    fn finish(mut self) -> T {
        self.thread
            .take()
            .expect("worker has not been joined")
            .join()
            .expect("pipe worker does not panic")
    }
}

impl<T> Drop for PipeWorker<T> {
    fn drop(&mut self) {
        let Some(thread) = self.thread.take() else { return };
        self.cancelled.store(true, Ordering::Release);
        let cleanup_deadline = Instant::now() + Duration::from_secs(1);
        while !thread.is_finished() && Instant::now() < cleanup_deadline {
            // SAFETY: JoinHandle owns this valid thread handle until it is joined.
            // The API cancels synchronous I/O on that thread without taking ownership.
            // Repeat to cover cancellation racing with the start of a read/write.
            unsafe { CancelSynchronousIo(thread.as_raw_handle()) };
            std::thread::sleep(Duration::from_millis(1));
        }
        if thread.is_finished() {
            // Worker errors cannot replace the command failure during cleanup.
            let _ = thread.join();
        }
        // Windows cancellation is best effort. If the OS refuses cancellation,
        // dropping the handle detaches the worker rather than hanging the caller.
    }
}

pub(super) fn run(
    command: &[String],
    input: Option<&str>,
    timeout: Duration,
    non_zero: NonZeroExit,
) -> Result<CommandResult, CommandError> {
    let Some((program, args)) = command.split_first() else {
        return Err(CommandError::new(command, 127, "empty command"));
    };
    let deadline = Instant::now() + timeout;
    let mut child = Command::new(program)
        .args(args)
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| spawn_error(command, program, &e))?;
    let writer = input.map(|input| {
        let input = input.to_owned();
        let mut stdin = child.stdin.take().expect("piped stdin");
        PipeWorker::spawn(move |cancelled| {
            let mut remaining = input.as_bytes();
            while !remaining.is_empty() && !cancelled.load(Ordering::Acquire) {
                match stdin.write(remaining) {
                    Ok(0) => break,
                    Ok(written) => remaining = &remaining[written..],
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    // An early stdin close is reflected in the command's status.
                    Err(_) => break,
                }
            }
        })
    });
    let stdout = read_pipe(child.stdout.take().expect("piped stdout"));
    let stderr = read_pipe(child.stderr.take().expect("piped stderr"));
    let mut status = None;
    loop {
        if status.is_none() {
            match child.try_wait() {
                Ok(result) => status = result,
                Err(e) => {
                    terminate(&mut child);
                    return Err(CommandError::new(
                        command,
                        1,
                        &format!("cannot wait for {program}: {e}"),
                    ));
                }
            }
        }
        if status.is_some()
            && writer.as_ref().is_none_or(PipeWorker::is_finished)
            && stdout.is_finished()
            && stderr.is_finished()
        {
            break;
        }
        if Instant::now() >= deadline {
            terminate(&mut child);
            // Dropping the workers requests I/O cancellation and joins them when a
            // descendant inherited the pipes. Only the direct child is killed;
            // Windows descendants can continue running (no job object is used).
            return Err(CommandError::new(
                command,
                124,
                &format!("timed out after {}s", timeout.as_secs()),
            ));
        }
        std::thread::sleep(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(10)),
        );
    }
    if let Some(writer) = writer {
        writer.finish();
    }
    let result = CommandResult {
        stdout: String::from_utf8_lossy(&stdout.finish()).into_owned(),
        stderr: String::from_utf8_lossy(&stderr.finish()).into_owned(),
        returncode: status.expect("child has exited").code().unwrap_or(-1),
    };
    non_zero.check(command, result)
}

fn read_pipe(mut pipe: impl Read + Send + 'static) -> PipeWorker<Vec<u8>> {
    PipeWorker::spawn(move |cancelled| {
        let mut output = Vec::new();
        let mut buffer = [0; 8192];
        while !cancelled.load(Ordering::Acquire) {
            match pipe.read(&mut buffer) {
                Ok(0) => break,
                Ok(length) => output.extend_from_slice(&buffer[..length]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                // Preserve output collected before a pipe error.
                Err(_) => break,
            }
        }
        output
    })
}

fn terminate(child: &mut Child) {
    // Cleanup failures do not replace the original timeout/wait error.
    let _ = child.kill();
    let reap_deadline = Instant::now() + Duration::from_secs(1);
    while matches!(child.try_wait(), Ok(None)) && Instant::now() < reap_deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desired::process::{CommandRunner, SubprocessRunner};

    fn fixture(name: &str) -> Vec<String> {
        vec![
            std::env::current_exe().unwrap().to_string_lossy().into_owned(),
            "--ignored".into(),
            "--exact".into(),
            format!("desired::process::windows::tests::{name}"),
            "--nocapture".into(),
        ]
    }

    #[test]
    #[ignore = "Subprocess fixture invoked by the deadline tests."]
    fn sleeping_fixture() {
        std::thread::sleep(Duration::from_secs(2));
    }

    #[test]
    #[ignore = "Subprocess fixture invoked by the deadline tests."]
    #[expect(
        clippy::zombie_processes,
        reason = "This Windows fixture must exit before its child closes inherited pipes."
    )]
    fn inherited_pipes_fixture() {
        let command = fixture("sleeping_fixture");
        // The fixture child lives for two seconds; Windows intentionally only
        // terminates the immediate child. No live services or hosts are touched.
        let _child = Command::new(&command[0])
            .args(&command[1..])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
    }

    #[test]
    #[ignore = "Subprocess fixture invoked by the normal-output test."]
    fn echo_fixture() {
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input).unwrap();
        print!("{input}");
        eprint!("fixture stderr");
    }

    #[test]
    fn deadline_cancels_blocked_reads_and_writes() {
        let payload = "payload".repeat(128 * 1024);
        for name in ["sleeping_fixture", "inherited_pipes_fixture"] {
            let started = Instant::now();
            let error = SubprocessRunner
                .run(
                    &fixture(name),
                    Some(&payload),
                    Duration::from_millis(100),
                    NonZeroExit::Error,
                )
                .unwrap_err();
            assert_eq!(error.returncode, 124);
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "pipe cancellation missed the deadline"
            );
        }
    }

    #[test]
    fn normal_output_and_stdin_are_preserved() {
        let payload = "large payload\n".repeat(32 * 1024);
        let output = SubprocessRunner
            .run(
                &fixture("echo_fixture"),
                Some(&payload),
                Duration::from_secs(10),
                NonZeroExit::Error,
            )
            .unwrap();
        assert!(output.stdout.contains(&payload));
        assert_eq!(output.stderr, "fixture stderr");
        assert_eq!(output.returncode, 0);
    }
}
