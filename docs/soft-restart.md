# Soft restart

**Soft restart** activates staged modules without rebooting the kernel. It uses
one daemon implementation from the manager and CLI:

```sh
su -c '/data/adb/ksud soft-reboot'
```

The daemon explicitly stops all running `zygote`/`zygote_*` init services
(including secondary and prewarmed slots), applies module changes while they remain stopped, launches module services,
and resumes the runtime. SurfaceFlinger, composer and loaded kernel modules
are not restarted. Concurrent installs, toggles and restarts are rejected rather
than interleaved. Failure resumes the runtime and records the failing stage;
there is no automatic full reboot or display-stack fallback.

A successful command submission means the detached operation started. To export
its final result and diagnostics to a computer:

```sh
adb exec-out su -c '/data/adb/ksud soft-reboot --status' > soft-reboot.txt
```

The report contains daemon/driver versions, runtime properties, stage results
and errors—not device identifiers, app data or full system logs. It stays private
on the device and is also included in the manager's existing log export. Nothing
is uploaded automatically. `soft-reboot --check` prints
`ksud-framework-restart-v1` without restarting; callers must check this before
relying on the coordinated restart contract.

## Module lifecycle

- `emulated-soft-reboot.sh` runs from the installed generation while Android's
  zygotes are held stopped, including for disabled/removed modules. Modules can
  use it to release their own resources before replacement. They must not
  restart Android or display services themselves.
- Updates are promoted before common and per-module `post-fs-data` scripts.
  Old directories are retained until a real boot because existing mounts and
  open descriptors may still refer to them. A failed promotion restores the
  original pathname.
- Blocking activation scripts must finish within the shared stage deadline.
  Nonzero exits and timeouts abort activation, resume Android and are reported.
  `service` scripts launch before zygote resumes; `boot-completed` scripts run
  only after a new `sys.boot_completed=1`.
- Long-lived module daemons and mounting implementations must support replay
  and refresh their module state. A runtime restart cannot update a daemon that
  deliberately retains its original module snapshot or unload kernel code.
  Kernel/preinit changes still require a kernel boot.

Host regression tests:

```sh
for unit in reboot:: module_state:: script_wait::; do
    cargo test --locked -p ksud "$unit"
done
python3 scripts/test_yogi_contract.py -v
```
