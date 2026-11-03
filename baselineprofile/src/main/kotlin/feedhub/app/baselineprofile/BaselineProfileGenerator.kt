package feedhub.app.baselineprofile

import androidx.benchmark.macro.MacrobenchmarkScope
import androidx.benchmark.macro.junit4.BaselineProfileRule
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.uiautomator.By
import androidx.test.uiautomator.Until
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Baseline Profile 生成器。跑的是 **`benchmark` 构建类型**（release 的 R8 配置 + debug 签名 +
 * 演示数据开关，`isDebuggable = false`），在模拟器 emulator-5554 上、**断网**跑演示数据（一个请求也不发）。
 *
 * 覆盖的路径：冷启动到首页（演示数据）→ 首页上下滑两三屏 → 打开一条图文详情、滑评论 →
 * 大图查看器左右滑 → 返回 → 打开一条视频详情 → 返回 → 切到「关注」/「我的」/「设置」。
 *
 * 跑法：
 * ```
 * adb -s emulator-5554 root                      # BaselineProfileRule 要读 /data/misc/profiles 下的 profile
 * svc wifi disable / 飞行模式                      # 演示数据本来不发请求，断网是双保险
 * ./gradlew :baselineprofile:connectedBenchmarkAndroidTest
 * adb -s emulator-5554 exec-out run-as feedhub.app.baselineprofile ls files/
 * adb -s emulator-5554 exec-out run-as feedhub.app.baselineprofile cat files/baseline-prof.txt \
 *   > androidApp/src/main/baselineProfiles/baseline-prof.txt
 * ```
 * 文件名带上测试方法名（`outputFilePrefix` 默认是 `BaselineProfileGenerator_startupAndBrowse`），
 * 另外还有一份 `*-startup-prof.txt`（`includeInStartupProfile = true` 才会有）。
 *
 * 找控件按**可见文字**来（Compose 的语义会进 accessibility 树），不按坐标：演示数据的标题是固定的
 * （`【演示帖】…`、`【演示】…分钟看懂 Compose…`），比坐标稳。图床是大图查看器的入口：正文配图带 caption
 * 「正文配图 1」，图就在 caption 正上方（点 caption.top 再往上一点）。
 */
@RunWith(AndroidJUnit4::class)
class BaselineProfileGenerator {
    @get:Rule
    val rule = BaselineProfileRule()

    @Test
    fun startupAndBrowse() = rule.collect(
        packageName = "feedhub.app",
        // 冷启动的类也进 startup profile：国产 ROM 上这直接决定冷启动要现解释多少类
        includeInStartupProfile = true,
        // 演示数据里有"3 小时前"这种相对时间，几次采集的 profile 可能略有差异；不因为"没稳定"整轮失败
        strictStability = false,
    ) {
        // ---- 冷启动：从桌面点进来（真正的"冷"启动，不是进程里再开 Activity），带演示数据 ----
        pressHome()
        startActivityAndWait { it.putExtra("demo", true) }
        device.wait(Until.hasObject(By.pkg("feedhub.app").depth(0)), 10_000)
        device.waitForIdle()

        // ---- 首页上下滑两三屏（瀑布流：卡片、图片解码、Compose 布局都会跑到） ----
        swipeDown(3)
        swipeUp(3) // 回顶部，后面要点第一屏的卡片

        // ---- 图文详情（贴吧的演示帖）：正文、评论、大图查看器 ----
        openByTextContains("演示帖")
        device.waitForIdle()
        // 正文里的第一张配图（caption「正文配图 1」在图的正下方），点图打开大图查看器
        findAndScrollTo("正文配图 1")
        device.findObject(By.text("正文配图 1"))?.let { caption ->
            val b = caption.visibleBounds
            device.click(b.centerX(), (b.top - 80).coerceAtLeast(200))
            device.waitForIdle()
            // 查看器里左右切（只点按钮，不靠手势，坐标无关）
            device.findObject(By.desc("下一张"))?.click()
            device.waitForIdle()
            device.findObject(By.desc("下一张"))?.click()
            device.waitForIdle()
            device.findObject(By.desc("上一张"))?.click()
            device.waitForIdle()
            device.findObject(By.desc("关闭"))?.click()
            device.waitForIdle()
        }
        // 滑到评论区（一级评论 + 楼中楼预览）
        swipeDown(2)
        // 返回首页
        device.pressBack()
        device.waitForIdle()

        // ---- 视频详情（B 站的演示视频：标题里有「分钟看懂 Compose」）----
        findAndScrollTo("分钟看懂 Compose")
        openByTextContains("分钟看懂 Compose")
        device.waitForIdle()
        // 视频详情：往下滑看简介 / 评论（占位播放器在模拟器上不会真的解码）
        swipeDown(1)
        device.pressBack()
        device.waitForIdle()

        // ---- 切到「关注」/「我的」/「设置」（底部导航的三个页面的首次组合） ----
        clickText("关注")
        device.waitForIdle()
        swipeDown(1)
        clickText("我的")
        device.waitForIdle()
        clickText("设置")
        device.waitForIdle()
    }

    // ------------------------------------------------------------------
    // 小工具（都用可见文字 / 系统 back，不写死坐标）
    // ------------------------------------------------------------------

    private fun MacrobenchmarkScope.swipeDown(times: Int) = repeat(times) {
        device.swipe(
            device.displayWidth / 2,
            (device.displayHeight * 0.75f).toInt(),
            device.displayWidth / 2,
            (device.displayHeight * 0.25f).toInt(),
            24,
        )
        device.waitForIdle()
    }

    private fun MacrobenchmarkScope.swipeUp(times: Int) = repeat(times) {
        device.swipe(
            device.displayWidth / 2,
            (device.displayHeight * 0.25f).toInt(),
            device.displayWidth / 2,
            (device.displayHeight * 0.75f).toInt(),
            24,
        )
        device.waitForIdle()
    }

    private fun MacrobenchmarkScope.clickText(text: String): Boolean {
        val obj = device.wait(Until.findObject(By.text(text)), 5_000) ?: return false
        obj.click()
        device.waitForIdle()
        return true
    }

    /** 点一条信息流卡片（按标题里的一段文字找）；找不到就往下滑几次再找。 */
    private fun MacrobenchmarkScope.openByTextContains(text: String): Boolean {
        if (findAndScrollTo(text)) {
            device.findObject(By.textContains(text))?.click()
            device.waitForIdle()
            return true
        }
        return false
    }

    /** 滚到能看见某段文字为止（最多往下滑 4 次，找不到就回到原位）。 */
    private fun MacrobenchmarkScope.findAndScrollTo(text: String): Boolean {
        if (device.wait(Until.hasObject(By.textContains(text)), 2_000)) return true
        val w = device.displayWidth
        val h = device.displayHeight
        repeat(4) {
            device.swipe(w / 2, (h * 0.75f).toInt(), w / 2, (h * 0.3f).toInt(), 24)
            device.waitForIdle()
            if (device.wait(Until.hasObject(By.textContains(text)), 1_500)) return true
        }
        return false
    }
}
