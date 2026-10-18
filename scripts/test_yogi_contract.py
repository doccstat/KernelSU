#!/usr/bin/env python3
"""Host-only checks for the fork's manager identity and late-load integration."""

from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
PACKAGE = "com.lixingchi.kernelsu"
SIGNER = "ff8c6f43e0bdd88057103c9faa5d78d7f43ee80a3c1ffa3e20eed06dcb640050"


def source(path):
    return (ROOT / path).read_text()


class YogiContractTest(unittest.TestCase):
    def test_kbuild_production_and_pr_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            tmp = Path(directory)
            header = tmp / "arch/arm64/include/asm/cacheflush.h"
            header.parent.mkdir(parents=True)
            header.write_text("")
            recipe = (f"include {ROOT}/kernel/Kbuild\n"
                      'identity:\n\t@echo ID=$(KSU_PACKAGE_NAME):$(KSU_EXPECTED_SIZE):$(KSU_EXPECTED_HASH):$(KSU_IS_PR_BUILD)\n')
            for args, identity in (
                ([], f"{PACKAGE}:0x034b:{SIGNER}:0"),
                (["KSU_IS_PR_BUILD=1", "KSU_EXPECTED_SIZE=0x123", "KSU_EXPECTED_HASH=test"],
                 f"{PACKAGE}:0x123:test:1"),
            ):
                output = subprocess.check_output(
                    ["make", "-sf", "-", "identity", f"srctree={tmp}", f"objtree={tmp}", *args],
                    input=recipe, text=True, cwd=tmp, stderr=subprocess.DEVNULL,
                )
                self.assertIn(f"ID={identity}", output)
        self.assertNotIn("KSU_MANAGER_PACKAGE", source("kernel/Kbuild"))
        self.assertNotIn("EXPECTED_HASH2", source("kernel/manager/apk_sign.c"))
        self.assertIn("KSU_PACKAGE_NAME", source("kernel/manager/throne_tracker.c"))

    def test_workflow_identity_and_release_contract(self):
        manager = source(".github/workflows/build-manager.yml")
        self.assertIn(f"package_name: {PACKAGE}", manager)
        self.assertEqual(manager.count(f"-PKSU_PACKAGE_NAME={PACKAGE} -PKSU_NAME=KernelSU"), 2)
        for key in ("expected_size", "expected_hash"):
            self.assertIn(f"{key}: ${{{{ needs.generate-key.outputs.{key}", manager)
            self.assertNotIn(f"{key}2:", manager)
        self.assertIn('"uapiVersion": 6', manager)
        self.assertIn('"upstreamCommit": "96770dfb807fea61e06819e4cbb438384df54b63"', manager)
        self.assertIn("KERNEL_SU_UAPI_VERSION = 6;", source("uapi/supercall.h"))
        self.assertIn("kmi: android16-6.12", source(".github/workflows/ghostlock-lkm.yml"))
        self.assertIn("fetch-depth: 0", source(".github/workflows/ddk-lkm.yml"))
        self.assertIn("-a arm64-v8a", manager)
        self.assertEqual(manager.count(":app:testDebugUnitTest"), 2)
        self.assertNotIn("testReleaseUnitTest", manager)
        self.assertIn("GhostLock-manager.apk", manager)
        self.assertNotIn("ghostlock-ksud-published", source(".github/workflows/ghostlock-lkm.yml"))

    def test_pages_requires_upstream_or_explicit_fork_opt_in(self):
        pages = source(".github/workflows/deploy-website.yml")
        self.assertIn("    if: github.repository == 'tiann/KernelSU' || vars.ENABLE_PAGES == 'true'", pages)
        self.assertIn("    needs: build", pages)
        self.assertNotIn("enablement: true", pages)

    def test_framework_only_restart_and_service_order(self):
        restart = re.sub(r"//[^\n]*", "", source("userspace/ksud/src/soft_reboot.rs"))
        self.assertNotRegex(restart, r'Command::new\("(?:stop|start|rmmod)"\)')
        self.assertNotRegex(restart, r"delete_module|insmod|unload::")
        self.assertIn('.args(["-9", "system_server"])', restart)
        self.assertIn("ScriptWait::Until", restart)
        body = restart[restart.index("pub fn soft_reboot()") :]
        ordered = ["ensure_uapi_version_matched", "restart_framework()?", "on_post_fs_data()?",
                   "wait_for_boot_completed()", "on_services()", "on_boot_completed()"]
        positions = [body.index(token) for token in ordered]
        self.assertEqual(positions, sorted(positions))
        self.assertEqual(body.count("on_services()"), 1)
        self.assertNotIn("on_post_data_fs", restart)

    def test_real_report_event_resets_services_without_reloading_driver(self):
        dispatch = source("kernel/supercall/dispatch.c")
        function = dispatch[dispatch.index("static int do_report_event(") : dispatch.index("static int do_set_sepolicy(")]
        harness = r'''
#include <cassert>
#include <cerrno>
#include <cstring>
#include <cstddef>
#include "uapi/supercall.h"
#include "uapi/app_profile.h"
#define __user
#define pr_info(...) ((void)0)
#define copy_from_user(d,s,n) (memcpy(d,s,n), 0)
static bool ksu_late_loaded = true;
static void on_post_fs_data() { assert(false); }
static void on_boot_completed() { assert(false); }
static void on_module_mounted() {}
static_assert(sizeof(app_profile) == 784);
static_assert(offsetof(app_profile, rp_config) == 272);
'''
        harness += function
        harness += r'''
int main() {
    assert(KERNEL_SU_UAPI_VERSION == 6);
    ksu_report_event_cmd services = {EVENT_SERVICES};
    ksu_report_event_cmd post = {EVENT_POST_FS_DATA};
    ksu_report_event_cmd boot = {EVENT_BOOT_COMPLETED};
    for (int i = 0; i < 3; ++i) {
        assert(do_report_event(&post) == 0);
        assert(do_report_event(&services) == 1);
        assert(do_report_event(&services) == 0);
        assert(do_report_event(&boot) == 0);
    }
}
'''
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "report-event"
            subprocess.run(["c++", "-std=c++17", "-x", "c++", "-I", str(ROOT), "-o", str(binary), "-"],
                           input=harness, text=True, check=True)
            subprocess.run([str(binary)], check=True)

    def test_late_load_keeps_susfs_provider_and_shell_grant(self):
        self.assertIn("bool ksu_uid_should_umount(uid_t uid)", source("kernel/policy/allowlist.c"))
        self.assertIn("module_param(allow_shell, bool, 0)", source("kernel/core/init.c"))
        self.assertIn("DEFINE_STATIC_KEY_FALSE(ksu_shell_su_adb_only)", source("kernel/feature/shell_su_adb_only.c"))
        self.assertIn("&& !ksu_late_loaded", source("kernel/supercall/dispatch.c"))
        self.assertIn("ksuinit::load_module(&module_data, &cparams)", source("userspace/ksud/src/debug.rs"))


if __name__ == "__main__":
    unittest.main()
