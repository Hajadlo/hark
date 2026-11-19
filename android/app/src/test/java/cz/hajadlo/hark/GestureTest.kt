package cz.hajadlo.hark

import org.junit.Assert.*
import org.junit.Test

class GestureTest {
    private var time = 1000L
    private val gesture = Gesture { time }
    @Test fun tapTogglesUntilNextPress() {
        assertEquals(listOf(Gesture.Effect.START), gesture.press())
        time += 299
        assertTrue(gesture.release().isEmpty())
        assertEquals(Gesture.State.RECORDING, gesture.state)
        time += 800
        assertEquals(listOf(Gesture.Effect.STOP), gesture.press())
        assertTrue(gesture.release().isEmpty())
        assertEquals(Gesture.State.PROCESSING, gesture.state)
    }
    @Test fun holdStopsAtBoundary() {
        gesture.press(); time += 300
        assertEquals(listOf(Gesture.Effect.STOP), gesture.release())
    }
    @Test fun dragCancelsWithoutUpload() {
        gesture.press()
        assertTrue(gesture.move(24f).isEmpty())
        assertEquals(listOf(Gesture.Effect.CANCEL), gesture.move(24.1f))
        assertEquals(Gesture.State.IDLE, gesture.state)
        assertTrue(gesture.release().isEmpty())
    }
    @Test fun busyPressIgnored() {
        gesture.setState(Gesture.State.PROCESSING)
        assertEquals(listOf(Gesture.Effect.BUSY), gesture.press())
        assertTrue(gesture.move(50f).isEmpty()); assertTrue(gesture.release().isEmpty())
        assertEquals(Gesture.State.PROCESSING, gesture.state)
    }
    @Test fun tenMinuteCapStopsExactlyOnce() {
        gesture.press(); time += 10; gesture.release()
        time += 599989
        assertTrue(gesture.tick().isEmpty())
        time++
        assertEquals(listOf(Gesture.Effect.STOP), gesture.tick())
        assertTrue(gesture.tick().isEmpty()); assertTrue(gesture.release().isEmpty())
    }
    @Test fun systemTouchCancellationDiscards() {
        gesture.press()
        assertEquals(listOf(Gesture.Effect.CANCEL), gesture.cancel())
        assertTrue(gesture.release().isEmpty())
    }
}
