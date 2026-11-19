package cz.hajadlo.hark

import kotlin.math.sqrt

/** Normalized RMS of the samples AudioRecord actually returned, before WAV encoding. */
object MicLevel {
    fun rms(samples: ShortArray, count: Int): Float {
        require(count in 0..samples.size)
        if (count == 0) return 0f
        var energy = 0.0
        for (i in 0 until count) {
            val sample = samples[i].toDouble() / 32768.0
            energy += sample * sample
        }
        return sqrt(energy / count).toFloat().coerceIn(0f, 1f)
    }
}
