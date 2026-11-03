package feedhub.cli

import java.io.File

/**
 * 平台插件清单的读写：仓库根的 `plugins.toml`（启用哪些模块、什么顺序）和每个模块根的 `plugin.toml`（id + 描述常量）。
 * 读法和 `settings.gradle.kts` 的 `tomlStrings` 一样（只认 `key = "字符串"` / `key = ["字符串", …]` 和 `#` 注释），
 * 两边的检查也一样——这里先查一遍，免得改完清单才在构建时报错。
 *
 * 只改文本文件，不构建、不加载插件的类（插件可能还没编译过）；改完由用户 / `./feedhub` 下次运行时重新构建。
 */
class PluginManifest(val root: File) {
    val file: File = File(root, MANIFEST)

    /** `plugins.toml` 里启用的模块（顺序 = 界面顺序）。 */
    fun enabled(): List<String> =
        Toml.strings(file.readText(), "plugins") ?: throw PluginManifestException("$file 里没有 plugins = [ … ]")

    /** 仓库里所有带 `plugin.toml` 的模块（启用没启用都算），按目录名排。 */
    fun available(): List<PluginModule> =
        root.listFiles { f -> f.isDirectory && File(f, PLUGIN_META).isFile }.orEmpty().sortedBy { it.name }.map { read(it.name) }

    /** 读一个模块的 `plugin.toml`（读不出来的项是 null，[problems] 会说）。 */
    fun read(module: String): PluginModule {
        val dir = File(root, module)
        val meta = File(dir, PLUGIN_META).takeIf { it.isFile }?.readText()
        return PluginModule(
            module = module,
            dir = dir,
            id = meta?.let { runCatching { Toml.strings(it, "id")?.singleOrNull() }.getOrNull() },
            descriptor = meta?.let { runCatching { Toml.strings(it, "descriptor")?.singleOrNull() }.getOrNull() },
        )
    }

    /**
     * 一个模块能不能启用（和 settings.gradle.kts 同样的检查，外加"描述常量和 Platform.define 真的写在源码里"）。
     * 空列表 = 没问题。不编译、不加载类，所以描述写得对不对（颜色、域名、和 Source 对不对得上）要靠插件自己的契约测试。
     */
    fun problems(module: String, others: List<String> = enabled() - module): List<String> = buildList {
        if (!MODULE.matches(module)) add("模块名不合法：$module（小写字母开头，只能有小写字母、数字、-）")
        val m = read(module)
        if (!m.dir.isDirectory) { add("没有这个目录：${m.dir}"); return@buildList }
        if (!File(m.dir, "build.gradle.kts").isFile) add("$module 不是一个模块（没有 build.gradle.kts）")
        if (!File(m.dir, PLUGIN_META).isFile) { add("$module 没有 plugin.toml（平台插件的构建期描述）"); return@buildList }
        val id = m.id ?: run { add("$module/plugin.toml 缺 id = \"…\""); return@buildList }
        val descriptor = m.descriptor ?: run { add("$module/plugin.toml 缺 descriptor = \"…\""); return@buildList }
        if (!ID.matches(id)) add("id 不合法：$id（小写字母开头，只能有小写字母、数字、-、_，最长 32）")
        if (!DESCRIPTOR.matches(descriptor)) {
            add("descriptor 要写成完整名字，例如 feedhub.source.bilibili.BilibiliPlugin（现在是 $descriptor）")
            return@buildList
        }
        val sources = File(m.dir, "src/main/kotlin").walkTopDown().filter { it.isFile && it.extension == "kt" }.map { it.readText() }.toList()
        val pkg = descriptor.substringBeforeLast('.')
        val name = descriptor.substringAfterLast('.')
        val declared = Regex("""(?m)^\s*(public\s+)?val\s+${Regex.escape(name)}\s*(:\s*PlatformPlugin\b|=\s*PlatformPlugin\()""")
        if (sources.none { Regex("""(?m)^package\s+${Regex.escape(pkg)}\s*$""").containsMatchIn(it) && declared.containsMatchIn(it) }) {
            add("在 $module/src/main/kotlin 的包 $pkg 里没找到 `val $name: PlatformPlugin`")
        }
        if (sources.none { "Platform.define(\"$id\"" in it }) add("源码里没有 Platform.define(\"$id\", …)：plugin.toml 的 id 要和描述里的一样")
        for (other in others) {
            if (read(other).id == id) add("id $id 已经被启用的 $other 用了")
        }
    }

    /** 启用：加到清单末尾。返回启用后的清单。 */
    fun add(module: String): List<String> {
        val now = enabled()
        if (module in now) throw PluginManifestException("$module 已经启用了")
        val problems = problems(module, now)
        if (problems.isNotEmpty()) throw PluginManifestException("$module 不能启用：\n" + problems.joinToString("\n") { "  - $it" })
        return (now + module).also(::write)
    }

    /** 停用：按 id 或模块名从清单里删掉。返回被删的模块。 */
    fun remove(idOrModule: String): String {
        val now = enabled()
        val module = now.firstOrNull { it == idOrModule } ?: now.firstOrNull { read(it).id == idOrModule }
            ?: throw PluginManifestException("清单里没有 $idOrModule（可选：${now.joinToString { m -> read(m).id ?: m }}）")
        if (now.size == 1) throw PluginManifestException("$module 是最后一个插件了：一个平台都没有的 App 什么也干不了")
        write(now - module)
        return module
    }

    /** 把 `plugins = [ … ]` 换成新的列表（一行一个），清单里别的文字（注释）原样留着。 */
    fun write(modules: List<String>) {
        val text = file.readText()
        val m = ARRAY.find(text) ?: throw PluginManifestException("$file 里没有 plugins = [ … ]")
        val body = modules.joinToString("") { "    \"$it\",\n" }
        file.writeText(text.substring(0, m.range.first) + "plugins = [\n$body]" + text.substring(m.range.last + 1))
    }

    companion object {
        const val MANIFEST = "plugins.toml"
        const val PLUGIN_META = "plugin.toml"
        const val TEMPLATE = "tools/plugin-template"
        private val MODULE = Regex("[a-z][a-z0-9-]*")
        val ID = Regex("[a-z][a-z0-9_-]{0,31}")
        private val DESCRIPTOR = Regex("[a-z][a-z0-9_]*(\\.[a-z][a-z0-9_]*)*\\.[A-Za-z_][A-Za-z0-9_]*")
        private val ARRAY = Regex("""(?m)^plugins\s*=\s*\[[^\]]*]""")

        /** 仓库根：`./feedhub` 设的 `FEEDHUB_REPO`，没有就从当前目录往上找 `plugins.toml`。 */
        fun locate(env: String? = System.getenv("FEEDHUB_REPO"), from: File = File("").absoluteFile): File? {
            env?.let { File(it) }?.takeIf { File(it, MANIFEST).isFile }?.let { return it }
            return generateSequence(from) { it.parentFile }.firstOrNull { File(it, MANIFEST).isFile && File(it, "settings.gradle.kts").isFile }
        }
    }
}

data class PluginModule(val module: String, val dir: File, val id: String?, val descriptor: String?)

class PluginManifestException(message: String) : Exception(message)

/**
 * 从模板（`tools/plugin-template/`）生成一个新的插件模块 `source-<id>`。不启用（写完了再 `plugin add`）。
 * 模板里的占位：`__ID__`（id）、`__LABEL__`（显示名）、`__PKG__`（包名的最后一段）、`__NAME__`（类名前缀）、
 * `__CONST__`（平台常量名）；文件名 / 目录名里的也换。
 */
object PluginTemplate {
    data class Names(val id: String, val label: String, val pkg: String, val name: String, val const: String)

    fun names(id: String, label: String): Names {
        require(PluginManifest.ID.matches(id)) { "id 不合法：$id（小写字母开头，只能有小写字母、数字、-、_，最长 32）" }
        val parts = id.split('-', '_').filter { it.isNotEmpty() }
        return Names(
            id = id,
            label = label,
            pkg = parts.joinToString(""),
            name = parts.joinToString("") { p -> p.replaceFirstChar { it.uppercaseChar() } },
            const = parts.joinToString("_") { it.uppercase() },
        )
    }

    /** 生成到 `<root>/source-<id>`，返回模块名。 */
    fun create(manifest: PluginManifest, id: String, label: String): String {
        val n = names(id, label)
        val module = "source-$id"
        val target = File(manifest.root, module)
        if (target.exists()) throw PluginManifestException("$target 已经有了")
        manifest.available().firstOrNull { it.id == id }?.let { throw PluginManifestException("id $id 已经被 ${it.module} 用了") }
        val template = File(manifest.root, PluginManifest.TEMPLATE)
        if (!template.isDirectory) throw PluginManifestException("找不到模板 $template")
        fun fill(s: String) = s.replace("__ID__", n.id).replace("__LABEL__", n.label).replace("__PKG__", n.pkg)
            .replace("__NAME__", n.name).replace("__CONST__", n.const)
        for (f in template.walkTopDown().filter { it.isFile }) {
            val out = File(target, fill(f.relativeTo(template).path))
            out.parentFile.mkdirs()
            out.writeText(fill(f.readText()))
        }
        return module
    }
}

/** 只认这里用到的那点 TOML：`key = "字符串"`、`key = ["字符串", …]`、`#` 注释（和 settings.gradle.kts 同一个规则）。 */
object Toml {
    fun strings(text: String, key: String): List<String>? {
        val noComments = text.lines().joinToString("\n") { line ->
            var inStr = false
            val cut = line.indexOfFirst { c -> if (c == '"') inStr = !inStr; c == '#' && !inStr }
            if (cut >= 0) line.substring(0, cut) else line
        }
        val m = Regex("""(?m)^\s*${Regex.escape(key)}\s*=\s*(\[[^\]]*]|"[^"\n]*")""").find(noComments) ?: return null
        val value = m.groupValues[1]
        val strings = Regex(""""([^"\n]*)"""").findAll(value).map { it.groupValues[1] }.toList()
        if (value.startsWith("[")) {
            val rest = value.removePrefix("[").removeSuffix("]").replace(Regex(""""[^"\n]*""""), "").replace(",", "").trim()
            if (rest.isNotEmpty()) throw PluginManifestException("$key 的数组里只能有字符串（多了 `$rest`）")
        }
        return strings
    }
}
