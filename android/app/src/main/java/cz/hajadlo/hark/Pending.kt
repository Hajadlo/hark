package cz.hajadlo.hark

import org.json.JSONObject
import java.io.File
import java.util.UUID

/** A sidecar preserves the starting mode and destination without duplicating the token. */
class Pending(private val directory: File) {
    data class Clip(val file: File, val base: String, val mode: String)
    init { directory.mkdirs() }
    @Synchronized fun create(base: String, mode: String): Clip {
        val file = File(directory, "${UUID.randomUUID()}.wav")
        file.createNewFile()
        File(directory, "${file.name}.json").writeText(JSONObject().put("base", base).put("mode", mode).toString())
        return Clip(file, base, mode)
    }
    @Synchronized fun find(name: String): Clip? {
        if (!name.matches(Regex("[a-f0-9-]{36}\\.wav"))) return null
        val file = File(directory, name)
        if (!file.isFile) return null
        return try {
            val json = JSONObject(File(directory, "$name.json").readText())
            Clip(file, BaseUrl.normalize(json.getString("base")), json.getString("mode"))
        } catch (_: Exception) { null }
    }
    @Synchronized fun all(): List<Clip> = directory.listFiles().orEmpty()
        .filter { it.extension == "wav" }.sortedBy { it.lastModified() }.mapNotNull { find(it.name) }
    @Synchronized fun discard(clip: Clip) {
        clip.file.delete()
        File(directory, "${clip.file.name}.json").delete()
    }
    @Synchronized fun prune(now: Long = System.currentTimeMillis()) {
        directory.listFiles().orEmpty().filter { now - it.lastModified() > 86_400_000 }.forEach { it.delete() }
    }
}
