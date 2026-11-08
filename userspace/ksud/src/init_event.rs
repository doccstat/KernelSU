use crate::module::{ScriptWait, handle_updated_modules, prune_modules};
use crate::utils::is_safe_mode;
use crate::{
    assets, defs, ksucalls, metamodule, restorecon,
    utils::{self},
};
use anyhow::{Context, Result};
use log::{info, warn};
use std::{path::Path, time::Instant};

pub fn on_post_fs_data(soft_reboot: bool) -> Result<()> {
    ksucalls::ensure_uapi_version_matched()?;
    let checked = |result: Result<()>| -> Result<()> {
        if soft_reboot {
            result
        } else {
            if let Err(error) = result {
                warn!("{error:#}");
            }
            Ok(())
        }
    };

    ksucalls::report_post_fs_data();

    utils::umask(0);

    // Clear all temporary module configs early
    if let Err(e) = crate::module_config::clear_all_temp_configs() {
        warn!("clear temp configs failed: {e}");
    }

    let _ = catch_bootlog("logcat", &["logcat", "-b", "all"]);
    let _ = catch_bootlog("dmesg", &["dmesg", "-w", "-r"]);

    if utils::has_magisk() {
        warn!("Magisk detected, skip post-fs-data!");
        return Ok(());
    }

    let safe_mode = crate::utils::is_safe_mode();
    let deadline = Instant::now() + defs::BOOT_STAGE_TIMEOUT;
    let wait = if soft_reboot {
        ScriptWait::FinishBy(deadline)
    } else {
        ScriptWait::Until(deadline)
    };

    let module_dir = defs::MODULE_DIR;
    utils::ensure_dir_exists(module_dir)?;

    assets::ensure_binaries(true).with_context(|| "Failed to extract bin assets")?;

    if !soft_reboot {
        crate::hot_install::clean_trash();
    }

    // if we are in safe mode, we should disable all modules
    if safe_mode {
        warn!("safe mode, skip post-fs-data scripts and disable all modules!");
        if let Err(e) = crate::module::disable_all_modules() {
            warn!("disable all modules failed: {e}");
        }
        return Ok(());
    }

    checked(handle_updated_modules().context("Applying module updates failed"))?;
    checked(
        prune_modules(if soft_reboot {
            wait
        } else {
            ScriptWait::Forever
        })
        .context("Pruning modules failed"),
    )?;

    // Hooks must see the generation being activated, not the one just replaced.
    checked(
        crate::module::exec_common_scripts("post-fs-data.d", wait)
            .context("Common post-fs-data scripts failed"),
    )?;

    // Refresh /metadata/watchdog/ksu/{modules.rc,preinit/} so the next boot's kernel hook sees the
    // current module set. Acts as a safety net when state was changed outside
    // of ksud's normal mutation commands.
    if let Err(e) = crate::module::regenerate_preinit() {
        warn!("regenerate preinit failed: {e:#}");
    }

    if let Err(e) = restorecon::restorecon() {
        warn!("restorecon failed: {e}");
    }

    // load sepolicy.rule
    if crate::module::load_sepolicy_rule().is_err() {
        warn!("load sepolicy.rule failed");
    }

    if let Err(e) = crate::profile::apply_sepolies() {
        warn!("apply root profile sepolicy failed: {e}");
    }

    crate::feature::init_forced_features();

    // load feature config
    if is_safe_mode() {
        warn!("safe mode, skip load feature config");
    } else if let Err(e) = crate::feature::init_features() {
        warn!("init features failed: {e}");
    }

    // execute metamodule post-fs-data script first (priority)
    checked(
        metamodule::exec_stage_script("post-fs-data", wait)
            .context("Metamodule post-fs-data failed"),
    )?;

    // exec modules post-fs-data scripts
    checked(
        crate::module::exec_stage_script("post-fs-data", wait)
            .context("Module post-fs-data failed"),
    )?;

    // load system.prop
    if let Err(e) = crate::module::load_system_prop() {
        warn!("load system.prop failed: {e}");
    }

    // execute metamodule mount script
    let mount_wait = if soft_reboot {
        wait
    } else {
        ScriptWait::Forever
    };
    checked(
        metamodule::exec_mount_script(module_dir, mount_wait).context("Metamodule mount failed"),
    )?;

    checked(run_stage_checked("post-mount", wait))?;

    std::env::set_current_dir("/").with_context(|| "failed to chdir to /")?;

    Ok(())
}

pub fn run_stage(stage: &str, wait: ScriptWait) {
    if let Err(error) = run_stage_checked(stage, wait) {
        warn!("{stage} failed: {error:#}");
    }
}

pub fn run_stage_checked(stage: &str, wait: ScriptWait) -> Result<()> {
    utils::umask(0);

    if utils::has_magisk() {
        warn!("Magisk detected, skip {stage}");
        return Ok(());
    }

    if crate::utils::is_safe_mode() {
        warn!("safe mode, skip {stage} scripts");
        return Ok(());
    }

    let checked = |result: Result<()>| -> Result<()> {
        if matches!(wait, ScriptWait::FinishBy(_) | ScriptWait::NoWait) {
            result
        } else {
            if let Err(error) = result {
                warn!("{stage} script failed: {error:#}");
            }
            Ok(())
        }
    };
    checked(crate::module::exec_common_scripts(
        &format!("{stage}.d"),
        wait,
    ))?;
    checked(metamodule::exec_stage_script(stage, wait))?;
    checked(crate::module::exec_stage_script(stage, wait))
}

pub fn on_services() -> Result<()> {
    ksucalls::ensure_uapi_version_matched()?;
    if !ksucalls::report_services()? {
        info!("services already started, skipping");
        return Ok(());
    }

    info!("on_services triggered!");
    run_stage_checked("service", ScriptWait::NoWait)
}

pub fn on_boot_completed() -> Result<()> {
    ksucalls::ensure_uapi_version_matched()?;

    ksucalls::report_boot_complete();
    info!("on_boot_completed triggered!");

    crate::preinit::on_boot_completed();

    run_stage_checked("boot-completed", ScriptWait::NoWait)
}

fn catch_bootlog(logname: &str, command: &[&str]) -> Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    let logdir = Path::new(defs::LOG_DIR);
    utils::ensure_dir_exists(logdir)?;
    let bootlog = logdir.join(format!("{logname}.log"));
    let oldbootlog = logdir.join(format!("{logname}.old.log"));

    if bootlog.exists() {
        std::fs::rename(&bootlog, oldbootlog)?;
    }

    let bootlog = std::fs::File::create(bootlog)?;

    let mut args = vec!["-s", "9", defs::BOOTLOG_TIMEOUT];
    args.extend_from_slice(command);
    // timeout -s 9 30s logcat > boot.log
    let result = unsafe {
        std::process::Command::new("timeout")
            .process_group(0)
            .pre_exec(|| {
                utils::switch_cgroups();
                Ok(())
            })
            .args(args)
            .stdout(Stdio::from(bootlog))
            .spawn()
    };

    if let Err(e) = result {
        warn!("Failed to start logcat: {e:#}");
    }

    Ok(())
}
