use std::{
    fs::{File, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    process::Command,
    thread::sleep,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use log::{error, info, warn};
use prop_rs_android::sys_prop;
use rustix::process::chdir;

use crate::{
    defs, init_event, ksucalls,
    module::ScriptWait,
    reboot::{self, Step},
    utils::{self, getprop},
};

const CONTRACT: &str = "ksud-framework-restart-v1";
const DOWN_TIMEOUT: Duration = Duration::from_secs(15);
const BOOT_TIMEOUT: Duration = Duration::from_secs(180);
const LOG_PATH: &str = concat!("/data/adb/ksu/log/", "soft-reboot.log");

fn setprop(name: &str, value: &str, skip_svc: bool) -> Result<()> {
    sys_prop::set(name, value, skip_svc).with_context(|| format!("Cannot set {name}={value}"))?;
    Ok(())
}

fn wait_property(name: &str, value: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while getprop(name).as_deref() != Some(value) {
        ensure!(
            Instant::now() < deadline,
            "Timed out waiting for {name}={value}"
        );
        sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn stop_runtime(service: &str) -> Result<()> {
    // An explicit stop sets SVC_DISABLED: init neither auto-restarts the zygote
    // nor executes its onrestart actions (which can disturb native services).
    setprop("ctl.stop", service, false)?;
    wait_property(&format!("init.svc.{service}"), "stopped", DOWN_TIMEOUT)?;
    if service == "zygote" {
        let output = Command::new("/system/bin/pidof")
            .arg("system_server")
            .output()?;
        ensure!(
            output.stdout.is_empty() && output.status.code() == Some(1),
            "system_server is still running after zygote stopped"
        );
    }
    Ok(())
}

fn prepare_restart() -> Result<()> {
    setprop("sys.boot_completed", "0", true)?;
    // Discount only this deliberate restart. Preserve real crash history,
    // observer XML, persisted mitigation counts and the recovery watchdog itself.
    for name in ["crashrecovery.rescue_boot_count", "sys.rescue_boot_count"] {
        if let Some(count) = getprop(name).and_then(|s| s.parse::<u32>().ok()) {
            setprop(name, &count.saturating_sub(1).to_string(), false)?;
        }
    }
    Ok(())
}

fn checkpoint(log: &mut File, message: &str) {
    info!("soft-reboot: {message}");
    // Logging failure must never prevent the error path from resuming Android.
    if let Err(error) = writeln!(log, "{message}").and_then(|()| log.sync_data()) {
        warn!("Cannot persist soft-reboot status: {error}");
    }
}

/// Both app and CLI callers use this implementation; --check is a non-mutating
/// capability handshake, --status exports only this operation's diagnostic record.
pub fn soft_reboot(check: bool, status: bool) -> Result<()> {
    if check {
        println!("{CONTRACT}");
        return Ok(());
    }
    if status {
        print!(
            "{}",
            std::fs::read_to_string(LOG_PATH).context("No soft-reboot report")?
        );
        return Ok(());
    }
    ksucalls::ensure_uapi_version_matched()?;
    let _lock = crate::module::lock_module_state()?;
    ensure!(
        getprop("sys.boot_completed").as_deref() == Some("1"),
        "Android is still booting"
    );
    sys_prop::init().context("Failed to initialize property API")?;
    let mut properties = Vec::new();
    sys_prop::for_each(|name, value| {
        if name == "init.svc.netd" || name.starts_with("init.svc.zygote") {
            properties.push((name.to_owned(), value.to_owned()));
        }
    });
    let services = reboot::running_services(&properties)?;
    utils::ensure_dir_exists(defs::LOG_DIR)?;
    let mut log = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(LOG_PATH)?;
    log.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    checkpoint(
        &mut log,
        &format!(
            "contract={CONTRACT} daemon={} driver={} uapi={}",
            defs::VERSION_CODE,
            ksucalls::get_version(),
            ksucalls::uapi_version()
        ),
    );
    for name in [
        "ro.build.version.sdk",
        "ro.zygote",
        "sys.boot_completed",
        "init.svc.surfaceflinger",
        "init.svc.netd",
    ] {
        checkpoint(
            &mut log,
            &format!("{name}={}", getprop(name).unwrap_or_default()),
        );
    }

    utils::daemonize_with(true, || {
        utils::switch_mnt_ns(1)?;
        chdir("/")?;
        Ok(())
    })?;

    let result = reboot::restart(&services, |step| {
        checkpoint(&mut log, &format!("begin={step:?}"));
        let result = match step {
            Step::Stop(service) => stop_runtime(service),
            Step::Prepare => prepare_restart(),
            Step::QuiesceModules => init_event::run_stage_checked(
                "emulated-soft-reboot",
                ScriptWait::FinishBy(Instant::now() + defs::EMULATED_SOFT_REBOOT_TIMEOUT),
            ),
            Step::PostFsData => init_event::on_post_fs_data(true),
            Step::Start(service) => setprop("ctl.start", service, false).and_then(|()| {
                wait_property(&format!("init.svc.{service}"), "running", DOWN_TIMEOUT)
            }),
            Step::Services => init_event::on_services(),
            Step::WaitBoot => wait_property("sys.boot_completed", "1", BOOT_TIMEOUT),
            Step::BootCompleted => init_event::on_boot_completed(),
        };
        if let Err(error) = &result {
            checkpoint(&mut log, &format!("failed={step:?} error={error:#}"));
        }
        result
    });
    checkpoint(
        &mut log,
        if result.is_ok() {
            "result=complete"
        } else {
            "result=failed"
        },
    );
    if let Err(error) = &result {
        error!("soft-reboot failed: {error:#}");
    }
    result
}
