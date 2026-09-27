use std::{
    process::Command,
    thread::sleep,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use libc::_exit;
use log::{error, info, warn};
use prop_rs_android::{resetprop::ResetProp, sys_prop};
use rustix::process::chdir;

use crate::{
    init_event::{on_boot_completed, on_post_data_fs, on_services, run_stage},
    ksucalls,
    utils::{self, switch_mnt_ns},
};

/// How long to wait for `system_server` to actually exit after the kill.
const FRAMEWORK_DOWN_TIMEOUT: Duration = Duration::from_secs(15);

const fn resetprop() -> ResetProp {
    ResetProp {
        skip_svc: true,
        persistent: false,
        persist_only: false,
        verbose: false,
        show_context: false,
        rebuild: false,
    }
}

fn reset_boot_completed() -> Result<()> {
    sys_prop::init().context("Failed to initialize system property API")?;
    let rp = resetprop();
    // Set prop value to 0 in advance to ensure resetprop -w works
    info!("reset boot complete prop to 0");
    rp.set("sys.boot_completed", "0")
        .context("Failed to set sys.boot_completed to 0")?;
    Ok(())
}

fn wait_for_boot_completed() -> Result<()> {
    sys_prop::init().context("Failed to initialize system property API")?;
    let rp = resetprop();
    info!("waiting for boot complete");
    rp.wait("sys.boot_completed", Some("0"), None)
        .context("wait for sys.boot_completed failed")?;
    Ok(())
}

fn system_server_running() -> bool {
    Command::new("pidof")
        .arg("system_server")
        .output()
        .map(|output| !output.stdout.is_empty())
        .unwrap_or(true)
}

/// Restart only the Android framework.
///
/// This is deliberately *not* init's blanket `stop`/`start` that a stock
/// `ksud soft-reboot` runs. `stop` tears the whole display stack down; on a
/// foldable the cover panel then comes back with stale DPMS/PSR state, which
/// shows up as a small fixed artifact (the "three dots"). Re-initialising the
/// panel afterwards is not possible from userspace - restarting the composer HAL
/// escalates to a RescueParty/recovery boot and unbinding the panel driver
/// panics the kernel - so the panel must not be staled in the first place.
///
/// Killing only `system_server` is enough. init restarts it (and `zygote64`, so
/// a Zygisk/Xposed module installed after the LKM load is re-injected) while
/// SurfaceFlinger and the composer HAL keep running.
fn restart_framework() -> Result<()> {
    let status = Command::new("killall")
        .args(["-9", "system_server"])
        .status()
        .context("killall system_server failed")?;
    if !status.success() {
        warn!("killall exited with status: {status}");
    }

    let deadline = Instant::now() + FRAMEWORK_DOWN_TIMEOUT;
    while Instant::now() < deadline {
        if !system_server_running() {
            return Ok(());
        }
        sleep(Duration::from_millis(100));
    }
    warn!("system_server still running after killall; continuing");
    Ok(())
}

pub fn soft_reboot() -> Result<()> {
    // check it avoid user click "soft_reboot" in manager when version mismatch
    if let Err(e) = ksucalls::ensure_uapi_version_matched() {
        error!("{e:#}, skip soft_reboot");
        return Ok(());
    }

    utils::daemonize_with(true, || -> Result<()> {
        switch_mnt_ns(1)?;
        chdir("/")?;
        Ok(())
    })?;

    info!("emulating soft_reboot!");
    if let Err(e) = reset_boot_completed() {
        warn!("reset boot completed failed: {e}");
    }
    run_stage("emulated-soft-reboot", true);

    info!("restarting the framework (zygote + system_server)");
    restart_framework()?;

    // `post-fs-data` has to run while the framework is down, which is why it
    // goes before the wait for BOOT_COMPLETED; `service` and `boot-completed`
    // run once it is back, exactly like a real boot.
    info!("post-fs-data");
    on_post_data_fs()?;

    if let Err(e) = wait_for_boot_completed() {
        warn!("wait for boot completed failed: {e}");
    }
    info!("services");
    on_services();
    on_boot_completed();

    unsafe {
        _exit(0);
    }
}
