package cz.hajadlo.hark

import java.io.File
import java.io.RandomAccessFile
import java.nio.ByteBuffer
import java.nio.ByteOrder

class Wav(file: File) : AutoCloseable {
    private val out = RandomAccessFile(file, "rw")
    private var bytes = 0
    init { out.setLength(0); out.write(header(0)) }
    fun write(samples: ShortArray, count: Int = samples.size) {
        require(count in 0..samples.size)
        val buffer = ByteBuffer.allocate(count * 2).order(ByteOrder.LITTLE_ENDIAN)
        repeat(count) { buffer.putShort(samples[it]) }
        out.write(buffer.array())
        bytes += count * 2
    }
    override fun close() {
        try { out.seek(0); out.write(header(bytes)) } finally { out.close() }
    }
    companion object {
        const val RATE = 16_000
        const val MAX_SAMPLES = RATE * 600
        fun header(bytes: Int): ByteArray = ByteBuffer.allocate(44).order(ByteOrder.LITTLE_ENDIAN).apply {
            put("RIFF".toByteArray()); putInt(36 + bytes); put("WAVEfmt ".toByteArray())
            putInt(16); putShort(1); putShort(1); putInt(RATE); putInt(RATE * 2)
            putShort(2); putShort(16); put("data".toByteArray()); putInt(bytes)
        }.array()
    }
}
