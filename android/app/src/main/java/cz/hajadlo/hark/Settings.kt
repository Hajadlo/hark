package cz.hajadlo.hark

import android.content.Context
import android.content.SharedPreferences

class Settings internal constructor(private val prefs: SharedPreferences) {
    constructor(context: Context) : this(context.getSharedPreferences("hark", Context.MODE_PRIVATE))
    val base: String
        get() = prefs.getString("base", "https://gpu-host.your-tailnet.ts.net:8766")!!
    val token: String
        get() = prefs.getString("token", "")!!
    val mode: String
        get() = prefs.getString("mode", "clean")!!
    var directKeyboardInsertion: Boolean
        get() = prefs.getBoolean("keyboard-direct-insertion", false)
        set(value) { prefs.edit().putBoolean("keyboard-direct-insertion", value).apply() }
    var showBubble: Boolean
        get() = prefs.getBoolean("show-bubble", true)
        set(value) { prefs.edit().putBoolean("show-bubble", value).apply() }
    fun save(base: String, token: String, mode: String) {
        val config = Credentials(BaseUrl.normalize(base), token.trim())
        require(mode.matches(Regex("[a-z0-9-]{1,32}"))) { "Invalid mode name" }
        prefs.edit().putString("base", config.base).putString("token", config.token).putString("mode", mode).apply()
    }
    fun snapshot(): Credentials {
        // getAll copies the preference map under its lock; separate getters can span a save.
        val values = prefs.all
        return Credentials(values["base"] as? String ?: "https://gpu-host.your-tailnet.ts.net:8766", values["token"] as? String ?: "")
    }
    var x: Int
        get() = prefs.getInt("bubble-x", 24)
        set(value) { prefs.edit().putInt("bubble-x", value).apply() }
    var y: Int
        get() = prefs.getInt("bubble-y", -1)
        set(value) { prefs.edit().putInt("bubble-y", value).apply() }
    fun uploader() = Uploader({ snapshot() }, BuildConfig.VERSION_NAME)
}
