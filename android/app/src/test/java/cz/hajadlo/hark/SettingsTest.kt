package cz.hajadlo.hark

import android.content.SharedPreferences
import org.junit.Assert.*
import org.junit.Test
import java.lang.reflect.Proxy

class SettingsTest {
    private class Preferences {
        var values = mapOf("base" to "https://a.example", "token" to "token-a", "mode" to "clean")
        val published = mutableListOf<Map<String, String>>()
        var afterRead: (() -> Unit)? = null
        @Suppress("UNCHECKED_CAST")
        private fun <T> proxy(type: Class<T>, call: (String, Array<out Any?>) -> Any?): T =
            Proxy.newProxyInstance(type.classLoader, arrayOf(type)) { _, method, args ->
                call(method.name, args ?: emptyArray())
            } as T
        val prefs = proxy(SharedPreferences::class.java) { method, args ->
            when (method) {
                "getAll" -> values.toMap().also { afterRead?.invoke() }
                "getString" -> (values[args[0]] ?: args[1]).also { afterRead?.invoke() }
                "edit" -> {
                    val updates = mutableMapOf<String, String>()
                    lateinit var editor: SharedPreferences.Editor
                    editor = proxy(SharedPreferences.Editor::class.java) { operation, parameters ->
                        when (operation) {
                            "putString" -> { updates[parameters[0] as String] = parameters[1] as String; editor }
                            "apply" -> { values = values + updates; published.add(values); null }
                            else -> error("Unexpected editor call: $operation")
                        }
                    }
                    editor
                }
                else -> error("Unexpected preferences call: $method")
            }
        }
    }
    @Test fun snapshotKeepsUrlAndTokenTogetherWhenSaveInterleaves() {
        val store = Preferences()
        val settings = Settings(store.prefs)
        store.afterRead = { settings.save("https://b.example/v1/", "token-b", "raw") }
        val snapshot = settings.snapshot()
        assertEquals("https://a.example", snapshot.base)
        assertEquals("token-a", snapshot.token)
        assertEquals("https://b.example", store.values["base"])
        assertEquals("token-b", store.values["token"])
    }
    @Test fun savePublishesPairTogetherAndInvalidSavePublishesNothing() {
        val store = Preferences()
        val settings = Settings(store.prefs)
        settings.save("https://b.example/v1/", " token-b ", "raw")
        assertEquals(1, store.published.size)
        assertEquals(mapOf("base" to "https://b.example", "token" to "token-b", "mode" to "raw"), store.published.single())
        assertThrows(IllegalArgumentException::class.java) { settings.save("https://c.example", "bad\ntoken", "clean") }
        assertEquals(1, store.published.size)
    }
}
