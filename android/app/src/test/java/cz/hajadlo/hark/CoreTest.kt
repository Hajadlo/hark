package cz.hajadlo.hark

import com.sun.net.httpserver.HttpServer
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.RandomAccessFile
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference

class CoreTest {
    private fun temp() = File.createTempFile("hark-test", ".wav").apply { deleteOnExit() }
    @Test fun wavHeaderAndSignedSamples() {
        val file = temp()
        Wav(file).use { it.write(shortArrayOf(0, 32767, -32768, -1)) }
        val bytes = file.readBytes()
        val buffer = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
        assertEquals(52, bytes.size)
        assertEquals("RIFF", bytes.copyOfRange(0, 4).toString(Charsets.US_ASCII))
        assertEquals("WAVEfmt ", bytes.copyOfRange(8, 16).toString(Charsets.US_ASCII))
        assertEquals(44, buffer.getInt(4)); assertEquals(16, buffer.getInt(16))
        assertEquals(1, buffer.getShort(20).toInt()); assertEquals(1, buffer.getShort(22).toInt())
        assertEquals(16000, buffer.getInt(24)); assertEquals(32000, buffer.getInt(28))
        assertEquals(2, buffer.getShort(32).toInt()); assertEquals(16, buffer.getShort(34).toInt())
        assertEquals("data", bytes.copyOfRange(36, 40).toString(Charsets.US_ASCII)); assertEquals(8, buffer.getInt(40))
        assertArrayEquals(byteArrayOf(0, 0, -1, 127, 0, -128, -1, -1), bytes.copyOfRange(44, 52))
    }
    @Test fun multipartExactBytesAndLength() {
        val file = temp().apply { writeBytes(byteArrayOf(0, -1, 13, 10)) }
        val body = Multipart(file, "raw", "test-boundary")
        val expected = ("--test-boundary\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nraw\r\n" +
            "--test-boundary\r\nContent-Disposition: form-data; name=\"response_format\"\r\n\r\njson\r\n" +
            "--test-boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n" +
            "Content-Type: audio/wav\r\n\r\n").toByteArray() + file.readBytes() + "\r\n--test-boundary--\r\n".toByteArray()
        val out = ByteArrayOutputStream(); body.writeTo(out)
        assertArrayEquals(expected, out.toByteArray()); assertEquals(expected.size.toLong(), body.length)
    }
    @Test fun baseUrlNormalization() {
        listOf(" https://EXAMPLE.com:8766/v1/ ", "https://example.com:8766///", "https://example.com:8766")
            .forEach { assertEquals("https://example.com:8766", BaseUrl.normalize(it)) }
        listOf("http://example.com", "https://example.com/path", "https://user:secret@example.com", "https://example.com?token=x",
            "https://example.com#v1", "https://example.com:99999", "file:///tmp/a").forEach {
            assertThrows(Exception::class.java) { BaseUrl.normalize(it) }
        }
    }
    @Test fun retryDecisionPerStatus() {
        listOf(400, 401, 403, 404, 411, 413, 422, 429, 200, 302).forEach { assertFalse(Responses.retry(it)) }
        listOf(500, 502, 503, 504, 599).forEach { assertTrue(Responses.retry(it)) }
        assertTrue(Responses.retry(null))
    }
    @Test fun parsesResponsesAndErrorShape() {
        assertEquals("Žluťoučký\ntext", Responses.text("""{"text":"Žluťoučký\ntext","id":42,"cleanup":"ok"}"""))
        assertEquals("", Responses.text("""{"text":"","id":null}"""))
        assertThrows(Exception::class.java) { Responses.text("""{"text":null}""") }
        assertThrows(Exception::class.java) { Responses.text("not JSON") }
        assertEquals(listOf("clean", "raw"), Responses.models("""{"data":[{"id":"clean"},{"id":"raw"}]}"""))
        assertEquals("Busy", Responses.error(503, """{"error":{"message":"Busy"}}"""))
        assertTrue(Responses.error(401, """{"error":{"message":"Bad credential"}}""").contains("check the token"))
        assertEquals("Server returned HTTP 502", Responses.error(502, "<html>"))
    }
    @Test fun retriesTransportWithFixedLengthAndHeaders() {
        val server = HttpServer.create(InetSocketAddress("127.0.0.1", 0), 0)
        val file = temp().apply { Wav(this).use { it.write(shortArrayOf(1, 2, 3)) } }
        var requests = 0
        val delays = mutableListOf<Long>()
        server.createContext("/v1/audio/transcriptions") { exchange ->
            assertEquals("Bearer synthetic-test-token", exchange.requestHeaders.getFirst("Authorization"))
            assertEquals("test", exchange.requestHeaders.getFirst("X-Hark-Device"))
            assertEquals("hark-android/test", exchange.requestHeaders.getFirst("User-Agent"))
            assertNull(exchange.requestHeaders.getFirst("Transfer-Encoding"))
            val body = exchange.requestBody.readBytes()
            assertEquals(body.size.toString(), exchange.requestHeaders.getFirst("Content-Length"))
            val boundary = exchange.requestHeaders.getFirst("Content-Type").substringAfter("boundary=")
            val expected = ByteArrayOutputStream().apply { Multipart(file, "raw", boundary).writeTo(this) }.toByteArray()
            assertArrayEquals(expected, body)
            val status = if (++requests < 3) 503 else 200
            val response = (if (status == 200) """{"text":"Synthetic test."}""" else """{"error":{"message":"Busy"}}""").toByteArray()
            exchange.sendResponseHeaders(status, response.size.toLong())
            exchange.responseBody.use { it.write(response) }
        }
        server.start()
        try {
            assertEquals("Synthetic test.", Uploader("http://127.0.0.1:${server.address.port}", "synthetic-test-token", "test", "test") { delays.add(it) }.upload(file, "raw"))
            assertEquals(3, requests); assertEquals(listOf(1000L, 2000L), delays); assertTrue(file.exists())
        } finally { server.stop(0) }
    }
    @Test fun actualGatewayIntegration() {
        val url = System.getenv("HARK_TEST_URL").orEmpty()
        assumeTrue("HARK_TEST_URL is unset", url.isNotBlank())
        val token = File(requireNotNull(System.getenv("HARK_TEST_TOKEN_FILE"))).readText().trim()
        val wav = File(requireNotNull(System.getenv("HARK_TEST_WAV")))
        val bytes = wav.readBytes()
        assertEquals(16000, ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN).getInt(24))
        val result = Uploader(url, token, "integration", "test").upload(wav, "raw")
        assertTrue("Synthetic speech should be transcribed", result.isNotBlank())
        println("Real gateway integration passed: fixed-length WAV, device=test, transcript nonempty; token redacted")
    }
    @Test fun permanentFailuresKeepAudioWithoutRetryAndRedactToken() {
        val secret = "synthetic-test-token"
        listOf(401 to """{"error":{"message":"Rejected $secret"}}""", 200 to "broken JSON", 302 to "redirect")
            .forEach { (status, body) ->
                val server = HttpServer.create(InetSocketAddress("127.0.0.1", 0), 0)
                var requests = 0
                server.createContext("/v1/audio/transcriptions") { exchange ->
                    requests++
                    exchange.requestBody.readBytes()
                    if (status == 302) exchange.responseHeaders.set("Location", "/should-not-follow")
                    val bytes = body.toByteArray()
                    exchange.sendResponseHeaders(status, bytes.size.toLong())
                    exchange.responseBody.use { it.write(bytes) }
                }
                server.start()
                val file = temp().apply { writeBytes(Wav.header(0)) }
                try {
                    val error = assertThrows(ApiFailure::class.java) {
                        Uploader("http://127.0.0.1:${server.address.port}", secret, "test", "test") {
                            fail("Permanent responses must not sleep/retry")
                        }.upload(file, "raw")
                    }
                    assertFalse(error.message.orEmpty().contains(secret))
                    assertEquals(status, error.status)
                    assertEquals(1, requests); assertTrue(file.exists())
                } finally { server.stop(0) }
            }
        assertThrows(IllegalArgumentException::class.java) { Uploader("https://example.com", "bad\ntoken", "test") }
    }
    @Test fun pendingSurvivesRestartAndExpiresOrDiscards() {
        val directory = java.nio.file.Files.createTempDirectory("hark-pending").toFile()
        try {
            val first = Pending(directory).create("https://example.com", "raw")
            first.file.writeBytes(Wav.header(0))
            val restarted = Pending(directory)
            assertEquals("raw", restarted.all().single().mode)
            assertEquals("https://example.com", restarted.all().single().base)
            assertNull(restarted.find("../../secret.wav"))
            restarted.discard(first)
            assertTrue(directory.listFiles().orEmpty().isEmpty())
            val old = restarted.create("https://example.com", "clean")
            val now = System.currentTimeMillis()
            directory.listFiles().orEmpty().forEach { assertTrue(it.setLastModified(now - 86_400_001)) }
            restarted.prune(now)
            assertFalse(old.file.exists()); assertTrue(directory.listFiles().orEmpty().isEmpty())
        } finally { directory.deleteRecursively() }
    }
    @Test(timeout = 10_000) fun eachRetryUsesExactlyOneFreshCredentialSnapshot() {
        val first = HttpServer.create(InetSocketAddress("127.0.0.1", 0), 0)
        val second = HttpServer.create(InetSocketAddress("127.0.0.1", 0), 0)
        val a = Credentials("http://127.0.0.1:${first.address.port}", "synthetic-token-a")
        val b = Credentials("http://127.0.0.1:${second.address.port}", "synthetic-token-b")
        val config = AtomicReference(a)
        val reads = AtomicInteger()
        listOf(Triple(first, a, 503), Triple(second, b, 200)).forEach { (server, expected, status) ->
            server.createContext("/v1/audio/transcriptions") { exchange ->
                assertEquals("Bearer ${expected.token}", exchange.requestHeaders.getFirst("Authorization"))
                exchange.requestBody.readBytes()
                val body = (if (status == 503) """{"error":{"message":"Busy"}}""" else """{"text":"Test."}""").toByteArray()
                exchange.sendResponseHeaders(status, body.size.toLong())
                exchange.responseBody.use { it.write(body) }
            }
            server.start()
        }
        val file = temp().apply { writeBytes(Wav.header(0)) }
        try {
            val uploader = Uploader({
                reads.incrementAndGet()
                config.getAndSet(b) // Simulate a settings save immediately after the snapshot read.
            }, "test", "test", deadlineMillis = 1000, sleep = {})
            assertEquals("Test.", uploader.upload(file, "raw"))
            assertEquals(2, reads.get())
        } finally { first.stop(0); second.stop(0) }
    }
    @Test(timeout = 15_000) fun deadlineRetriesWhenServerAcceptsButNeverReads() {
        val server = ServerSocket().apply {
            receiveBufferSize = 1024
            bind(InetSocketAddress("127.0.0.1", 0))
        }
        val accepted = CopyOnWriteArrayList<Socket>()
        val release = CountDownLatch(1)
        val listener = Thread({
            try { while (!server.isClosed) accepted.add(server.accept()) } catch (_: java.io.IOException) { }
        }, "never-reading-server").apply { isDaemon = true; start() }
        val cleanup = Thread({
            // A broken watchdog must fail the test rather than hang the Gradle worker forever.
            release.await(8, TimeUnit.SECONDS)
            server.close()
            listener.join()
            accepted.forEach { it.close() }
        }, "stalled-server-cleanup").apply { isDaemon = true; start() }
        val file = temp()
        RandomAccessFile(file, "rw").use {
            val size = 16 * 1024 * 1024 // Far larger than the socket send buffer; all synthetic silence.
            it.setLength(size + 44L)
            it.write(Wav.header(size))
        }
        val delays = mutableListOf<Long>()
        val started = System.nanoTime()
        try {
            val error = assertThrows(ApiFailure::class.java) {
                Uploader("http://127.0.0.1:${server.localPort}", "synthetic-test-token", "test", "test",
                    deadlineMillis = 300) { delays.add(it) }.upload(file, "raw")
            }
            assertNull(error.status)
            assertTrue(Responses.retry(error.status))
            assertEquals(listOf(1000L, 2000L), delays)
            assertEquals(3, accepted.size)
            assertTrue("The watchdog must release the blocked write", System.nanoTime() - started < TimeUnit.SECONDS.toNanos(4))
            assertTrue(file.exists())
        } finally {
            release.countDown()
            cleanup.join()
            file.delete()
        }
    }
}
