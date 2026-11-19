package cz.hajadlo.hark

import org.junit.Assert.*
import org.junit.Test

class MicLevelTest {
    @Test fun silenceAndEmptyInputHaveNoLevel() {
        assertEquals(0f, MicLevel.rms(ShortArray(2048), 2048), 0f)
        assertEquals(0f, MicLevel.rms(shortArrayOf(Short.MAX_VALUE), 0), 0f)
    }
    @Test fun measuresOnlyReturnedSamplesInAReusedCaptureBuffer() {
        val buffer = shortArrayOf(8192, -8192, Short.MAX_VALUE, Short.MIN_VALUE)
        assertEquals(.25f, MicLevel.rms(buffer, 2), .00001f)
        assertTrue(MicLevel.rms(buffer, 4) > .7f)
    }
    @Test fun fullScaleNegativePcmDoesNotOverflow() {
        assertEquals(1f, MicLevel.rms(shortArrayOf(Short.MIN_VALUE), 1), 0f)
        assertEquals(1f, MicLevel.rms(shortArrayOf(Short.MAX_VALUE), 1), .0001f)
    }
}
