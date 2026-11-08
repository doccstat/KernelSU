//! Serialized module mutations and non-destructive generation replacement.
use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

pub fn lock(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.try_lock()
        .context("Another module operation or soft restart is in progress")?;
    Ok(file)
}

/// Retain the old generation for existing mounts and open descriptors until a
/// real boot. If promotion fails, restore its original pathname before returning.
pub fn replace(staged: &Path, live: &Path, trash: &Path) -> Result<()> {
    ensure!(
        staged.symlink_metadata()?.is_dir(),
        "Staged module is not a directory"
    );
    let old = if live.exists() {
        Some(retire(live, trash)?)
    } else {
        None
    };
    if let Err(error) = fs::rename(staged, live) {
        if let Some(old) = old {
            fs::rename(&old, live).with_context(|| {
                format!(
                    "Promotion failed ({error}); previous generation remains at {}",
                    old.display()
                )
            })?;
        }
        return Err(error).context("Cannot activate staged module");
    }
    Ok(())
}

pub fn retire(live: &Path, trash: &Path) -> Result<std::path::PathBuf> {
    ensure!(
        live.symlink_metadata()?.is_dir(),
        "Live module is not a directory"
    );
    fs::create_dir_all(trash)?;
    let name = live.file_name().context("Module directory has no name")?;
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let old = trash.join(format!("{}.{nanos}", name.to_string_lossy()));
    fs::rename(live, &old).context("Cannot retain previous module generation")?;
    Ok(old)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn installs_and_replaces_without_unlinking_old_files() {
        let root = tempfile::tempdir().unwrap();
        let live = root.path().join("modules/example");
        let staged = root.path().join("modules_update/example");
        let trash = root.path().join("trash");
        fs::create_dir_all(live.parent().unwrap()).unwrap();
        for version in ["one", "two"] {
            fs::create_dir_all(&staged).unwrap();
            fs::write(staged.join("payload"), version).unwrap();
            replace(&staged, &live, &trash).unwrap();
            assert_eq!(fs::read_to_string(live.join("payload")).unwrap(), version);
            assert!(!staged.exists());
        }
        let old = fs::read_dir(trash).unwrap().next().unwrap().unwrap().path();
        let mut content = String::new();
        File::open(old.join("payload"))
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();
        assert_eq!(content, "one");
    }

    #[test]
    fn bad_staging_leaves_live_module_untouched() {
        let root = tempfile::tempdir().unwrap();
        let live = root.path().join("live");
        fs::create_dir(&live).unwrap();
        fs::write(live.join("payload"), "old").unwrap();
        assert!(
            replace(
                &root.path().join("missing"),
                &live,
                &root.path().join("trash")
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(live.join("payload")).unwrap(), "old");
    }

    #[test]
    fn failed_promotion_restores_the_previous_generation() {
        let root = tempfile::tempdir().unwrap();
        let live = root.path().join("live");
        // Retiring live also moves this nested source, forcing promotion to fail.
        let staged = live.join("next");
        fs::create_dir_all(&staged).unwrap();
        fs::write(live.join("payload"), "old").unwrap();
        assert!(replace(&staged, &live, &root.path().join("trash")).is_err());
        assert_eq!(fs::read_to_string(live.join("payload")).unwrap(), "old");
        assert!(staged.exists());
    }

    #[test]
    fn lock_serializes_restarts_and_installers_and_releases_on_exit() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("module.lock");
        let guard = lock(&path).unwrap();
        assert!(lock(&path).is_err());
        drop(guard);
        assert!(lock(&path).is_ok());
    }
}
