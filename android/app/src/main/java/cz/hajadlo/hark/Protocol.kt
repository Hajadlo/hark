package cz.hajadlo.hark

import org.json.JSONObject
import java.io.File
import java.io.IOException
import java.io.OutputStream
import java.net.HttpURLConnection
import java.net.URI
import java.util.UUID
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

object BaseUrl {
    fun normalize(value: String): String {
        val text = value.trim().trimEnd('/').removeSuffix("/v1").trimEnd('/')
        val uri = URI(text)
        require(uri.scheme.equals("https", true) && !uri.host.isNullOrBlank()) { "Enter an HTTPS server URL" }
        require(uri.rawUserInfo == null && uri.rawQuery == null && uri.rawFragment == null &&
            uri.rawPath.orEmpty().isEmpty()) { "Use the server base URL, without a path, query or credentials" }
        require(uri.port == -1 || uri.port in 1..65535) { "Invalid server port" }
        return "https://${uri.rawAuthority.lowercase()}"
    }
}

class Multipart(private val file: File, mode: String, val boundary: String = "hark-${UUID.randomUUID()}") {
    init {
        require(boundary.matches(Regex("[A-Za-z0-9-]{1,70}")))
        require(mode.matches(Regex("[a-z0-9-]{1,32}"))) { "Mode must be 1–32 lowercase letters, numbers or hyphens" }
    }
    private val before = ("--$boundary\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\n$mode\r\n" +
        "--$boundary\r\nContent-Disposition: form-data; name=\"response_format\"\r\n\r\njson\r\n" +
        "--$boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n" +
        "Content-Type: audio/wav\r\n\r\n").toByteArray(Charsets.UTF_8)
    private val after = "\r\n--$boundary--\r\n".toByteArray(Charsets.UTF_8)
    val length: Long get() = before.size + file.length() + after.size
    fun writeTo(out: OutputStream) {
        out.write(before)
        file.inputStream().use { it.copyTo(out) }
        out.write(after)
    }
}

object Responses {
    fun text(body: String): String {
        val json = JSONObject(body)
        require(json.has("text") && json.get("text") is String) { "Invalid transcription response" }
        return json.getString("text")
    }
    fun models(body: String): List<String> {
        val data = JSONObject(body).getJSONArray("data")
        return (0 until data.length()).map { data.getJSONObject(it).getString("id") }
    }
    fun error(status: Int, body: String): String {
        val message = try { JSONObject(body).getJSONObject("error").getString("message") }
            catch (_: Exception) { "Server returned HTTP $status" }
        return if (status == 401 || status == 403) "$message — check the token" else message
    }
    fun retry(status: Int?): Boolean = status == null || status in 500..599
}

class ApiFailure(val status: Int?, message: String) : IOException(message)

data class Credentials(val base: String, val token: String) {
    init { require(token.none { it.code < 32 || it.code == 127 }) { "Invalid token: remove control characters" } }
}

/** No Android calls: the integration test executes this exact production transport. */
class Uploader(private val credentials: () -> Credentials, private val version: String,
    private val device: String = "phone", private val deadlineMillis: Long = 120_000,
    private val sleep: (Long) -> Unit = { Thread.sleep(it) }) {
    constructor(base: String, token: String, version: String, device: String = "phone",
        deadlineMillis: Long = 120_000, sleep: (Long) -> Unit = { Thread.sleep(it) }) :
        this({ Credentials(base, token) }, version, device, deadlineMillis, sleep) {
        Credentials(base, token)
    }
    init { require(deadlineMillis > 0) }
    private fun connection(path: String, config: Credentials): HttpURLConnection {
        // Tests may reach only their loopback gateway over HTTP; production always supplies HTTPS.
        val uri = URI(config.base)
        require(uri.scheme == "https" || (device == "test" && uri.scheme == "http" &&
            uri.host in listOf("127.0.0.1", "localhost", "::1")))
        return (URI(config.base + path).toURL().openConnection() as HttpURLConnection).apply {
            connectTimeout = 15_000
            readTimeout = 120_000
            instanceFollowRedirects = false // Never forward the bearer token to a redirect target.
            setRequestProperty("Authorization", "Bearer ${config.token}")
            setRequestProperty("X-Hark-Device", device)
            setRequestProperty("User-Agent", "hark-android/$version")
        }
    }
    private fun response(connection: HttpURLConnection, token: String): String {
        val status = connection.responseCode
        val stream = if (status in 200..299) connection.inputStream else connection.errorStream
        val bytes = stream?.use { it.readNBytes(1_048_577) } ?: byteArrayOf()
        if (bytes.size > 1_048_576) throw ApiFailure(status, "Server response is too large")
        val body = bytes.toString(Charsets.UTF_8)
        if (status != 200) {
            val message = Responses.error(status, body)
            throw ApiFailure(status, if (token.isEmpty()) message else message.replace(token, "[redacted]"))
        }
        return body
    }
    private fun <T> request(path: String, operation: (HttpURLConnection, String) -> T): T {
        val config = credentials() // One immutable URL/token pair for this attempt, including redaction.
        val conn = connection(path, config)
        val expired = AtomicBoolean(false)
        val watchdog = Executors.newSingleThreadScheduledExecutor { runnable ->
            Thread(runnable, "hark-request-deadline").apply { isDaemon = true }
        }
        // Android has no write timeout. Disconnect also covers a peer that never reads the body.
        val deadline = watchdog.schedule({ expired.set(true); conn.disconnect() }, deadlineMillis, TimeUnit.MILLISECONDS)
        try {
            val result = operation(conn, config.token)
            if (expired.get()) throw ApiFailure(null, "Network error or timeout")
            return result
        } catch (e: IOException) {
            if (expired.get()) throw ApiFailure(null, "Network error or timeout")
            throw e
        } finally {
            deadline.cancel(false)
            watchdog.shutdownNow()
            conn.disconnect()
        }
    }
    fun models(): List<String> {
        return request("/v1/models") { conn, token -> Responses.models(response(conn, token)) }
    }
    fun upload(file: File, mode: String): String {
        var last: ApiFailure? = null
        for (attempt in 0..2) {
            if (attempt > 0) sleep(attempt * 1000L)
            try {
                return request("/v1/audio/transcriptions") { conn, token ->
                    val body = Multipart(file, mode)
                    conn.requestMethod = "POST"
                    conn.doOutput = true
                    conn.setRequestProperty("Content-Type", "multipart/form-data; boundary=${body.boundary}")
                    conn.setFixedLengthStreamingMode(body.length)
                    conn.outputStream.use { body.writeTo(it) }
                    val json = response(conn, token)
                    try { Responses.text(json) }
                        catch (_: Exception) { throw ApiFailure(200, "Invalid transcription response") }
                }
            } catch (e: IOException) {
                // Transport exception text can include credentials supplied by callers; do not surface it.
                last = e as? ApiFailure ?: ApiFailure(null, "Network error or timeout")
                if (!Responses.retry(last.status)) throw last
            }
        }
        throw last ?: ApiFailure(null, "Upload failed")
    }
}
