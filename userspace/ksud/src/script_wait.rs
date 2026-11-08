use std::{
    io,
    process::Child,
    thread::sleep,
    time::{Duration, Instant},
};

/// The child must own its process group. A timed-out boot script must not keep
/// changing mounts/files behind a runtime that has been resumed after failure.
pub fn finish_by(child: &mut Child, deadline: Instant) -> io::Result<()> {
    let error = loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => break io::Error::other(format!("Script failed: {status}")),
            Ok(None) if Instant::now() < deadline => sleep(Duration::from_millis(20)),
            Ok(None) => break io::Error::new(io::ErrorKind::TimedOut, "Module stage timed out"),
            Err(error) => break error,
        }
    };
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
    Err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::process::CommandExt, process::Command};

    fn shell(script: &str) -> Child {
        Command::new("sh")
            .args(["-c", script])
            .process_group(0)
            .spawn()
            .unwrap()
    }

    #[test]
    fn detects_success_failure_and_reaps_timeout() {
        assert!(
            finish_by(
                &mut shell("exit 0"),
                Instant::now() + Duration::from_secs(1)
            )
            .is_ok()
        );
        assert!(
            finish_by(
                &mut shell("exit 7"),
                Instant::now() + Duration::from_secs(1)
            )
            .is_err()
        );
        let mut child = shell("sleep 10 & wait");
        let result = finish_by(&mut child, Instant::now() + Duration::from_millis(40));
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(child.try_wait().unwrap().is_some());
    }

    #[test]
    fn failed_and_timed_out_scripts_cannot_leave_background_writers() {
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("survived");
        for ending in ["wait", "exit 7"] {
            let mut child = shell(&format!(
                "(sleep 0.2; echo survived > '{}') & {ending}",
                marker.display()
            ));
            assert!(finish_by(&mut child, Instant::now() + Duration::from_millis(40)).is_err());
            sleep(Duration::from_millis(250));
            assert!(!marker.exists());
        }
    }
}
