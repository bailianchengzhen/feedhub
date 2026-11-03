package feedhub.cli

import com.github.ajalt.clikt.core.CliktCommand
import com.github.ajalt.clikt.core.CliktError
import com.github.ajalt.clikt.core.Context
import com.github.ajalt.clikt.core.requireObject
import com.github.ajalt.clikt.parameters.arguments.argument
import com.github.ajalt.clikt.parameters.options.option
import feedhub.plugins.InstalledPlugins

/**
 * `./feedhub plugin …`：看 / 改启用了哪些平台插件（仓库根的 plugins.toml）。只改清单，不构建；
 * 改完下次 `./feedhub` 运行时会自动重新构建，App 要重新打包。
 *
 * 这一组命令不建 [Env]（不读 Cookie、不连网），`./feedhub` 跑它们之前也不先构建（清单可能正改到一半，构建会失败）。
 */
class PluginCmd : CliktCommand(name = "plugin") {
    override fun help(context: Context) = "平台插件：list / add <模块> / remove <id> / new <id>（改的是仓库根的 plugins.toml）"
    override fun run() = Unit
}

private fun manifest(): PluginManifest {
    val root = PluginManifest.locate() ?: throw CliktError("找不到仓库根（plugins.toml）：在仓库目录里运行，或者用 ./feedhub")
    return PluginManifest(root)
}

private fun <T> manifestErrors(block: () -> T): T = try {
    block()
} catch (e: PluginManifestException) {
    throw CliktError(e.message.orEmpty())
} catch (e: IllegalArgumentException) {
    throw CliktError(e.message.orEmpty())
}

private const val REBUILD_HINT = "重新构建才生效：./feedhub 下次运行会自动增量构建；App 要重新打包（桌面 ./gradlew :app:packageDeb，安卓 ./gradlew :androidApp:assembleDebug）。"

class PluginListCmd : CliktCommand(name = "list") {
    override fun help(context: Context) = "列出启用的插件（顺序 = 界面顺序）和仓库里没启用的插件模块"
    override fun run() = manifestErrors {
        val m = manifest()
        val enabled = m.enabled()
        val labels = InstalledPlugins.catalog.plugins.associate { it.id to it.label }
        val w = (m.available().mapNotNull { it.id } + enabled).maxOf { it.length }.coerceAtLeast(7) + 1
        echo("启用（plugins.toml 的顺序 = 界面顺序）：")
        enabled.forEachIndexed { i, module ->
            val p = m.read(module)
            val id = p.id ?: "?"
            echo("  ${(i + 1).toString().padStart(2)}. ${id.padEnd(w)}${module.padEnd(w + 12)}${labels[id] ?: "（这次构建里没有）"}")
        }
        val rest = m.available().filter { it.module !in enabled }
        if (rest.isNotEmpty()) {
            echo("没启用（./feedhub plugin add <模块>）：")
            for (p in rest) echo("      ${(p.id ?: "?").padEnd(w)}${p.module}")
        }
        if (InstalledPlugins.modules != enabled) echo("⚠ 清单改过还没重新构建：这次构建装的是 ${InstalledPlugins.modules.joinToString(" ")}")
    }
}

class PluginAddCmd : CliktCommand(name = "add") {
    private val session by requireObject<Session>(SESSION_KEY)
    private val module by argument(help = "模块目录名，如 source-bilibili")
    override fun help(context: Context) = "启用一个插件模块（先检查模块在、plugin.toml 和描述常量对得上、id 不撞），加到清单末尾"
    override fun run() = manifestErrors {
        session.rejectInAgentMode("启用插件")
        val m = manifest()
        m.add(module)
        echo("已启用 $module（id ${m.read(module).id}），排在最后；要换位置直接改 plugins.toml 里的顺序。")
        echo(REBUILD_HINT)
        echo("新插件默认开不开看它描述里的 defaults：老用户的设置读出来时会按它补进各组（见 docs/平台插件指南.md）。")
    }
}

class PluginRemoveCmd : CliktCommand(name = "remove") {
    private val session by requireObject<Session>(SESSION_KEY)
    private val target by argument(name = "id", help = "插件 id（如 yt）或模块名（如 source-youtube）")
    override fun help(context: Context) = "停用一个插件（从清单里删掉，模块目录不动）"
    override fun run() = manifestErrors {
        session.rejectInAgentMode("停用插件")
        val module = manifest().remove(target)
        echo("已停用 $module（目录还在，装回来：./feedhub plugin add $module）。")
        echo(REBUILD_HINT)
        echo("设置、收藏、历史里关于它的数据都留着：不显示、不请求，装回来照旧。")
    }
}

class PluginNewCmd : CliktCommand(name = "new") {
    private val session by requireObject<Session>(SESSION_KEY)
    private val id by argument(help = "新平台的 id（小写字母开头，只能有小写字母、数字、-、_；定了就别改）")
    private val label by option("--name", help = "显示名（默认用 id）")
    override fun help(context: Context) = "从模板（tools/plugin-template）生成一个新的插件模块 source-<id>，不启用"
    override fun run() = manifestErrors {
        session.rejectInAgentMode("新建插件")
        val m = manifest()
        val module = PluginTemplate.create(m, id, label ?: id)
        echo("已生成 $module（骨架：一个推荐流 + 一条离线测试，接口和域名都是 example.com 的占位）。接下来：")
        echo("  1. 照 docs/平台插件指南.md 填描述（${module}/src/main/kotlin/…/*Plugin.kt）和 Source；")
        echo("  2. ./feedhub plugin add $module")
        echo("  3. ./gradlew :$module:test :plugin-registry:test")
    }
}
