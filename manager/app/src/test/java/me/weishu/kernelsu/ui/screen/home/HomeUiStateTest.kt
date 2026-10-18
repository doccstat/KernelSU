package me.weishu.kernelsu.ui.screen.home

import me.weishu.kernelsu.KernelVersion
import me.weishu.kernelsu.ui.util.module.LatestVersionInfo
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class HomeUiStateTest {
    @Test
    fun lateLoadedDriverNeverOffersBootImageUpdate() {
        val state = HomeUiState(
            kernelVersion = KernelVersion(6, 12, 69),
            ksuVersion = 32700,
            managerUAPIVersion = 6,
            kernelUAPIVersion = 6,
            lkmMode = true,
            isLkmBundled = true,
            isManager = true,
            isManagerPrBuild = false,
            isKernelPrBuild = false,
            requiresNewKernel = false,
            requiresNewManager = false,
            isRootAvailable = true,
            isSafeMode = false,
            isLateLoadMode = true,
            checkUpdateEnabled = false,
            latestVersionInfo = LatestVersionInfo(),
            currentManagerVersionCode = 32701,
            systemInfo = SystemInfo("6.12.69", "test", "yogi", "test", "Enforcing", 2),
        )
        assertFalse(state.canInstallKernelUpdate)
        assertFalse(state.showLkmUpdate)
        assertFalse(state.copy(requiresNewKernel = true).canInstallKernelUpdate)
        val bootLoaded = state.copy(isLateLoadMode = false)
        assertTrue(bootLoaded.canInstallKernelUpdate)
        assertTrue(bootLoaded.showLkmUpdate)
        assertFalse(bootLoaded.copy(lkmMode = false).showLkmUpdate)
        assertFalse(bootLoaded.copy(ksuVersion = 32701).showLkmUpdate)
    }
}
