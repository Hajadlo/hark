package cz.hajadlo.hark

/** Clock supplied by the caller; Android scheduling stays outside this state machine. */
class Gesture(private val now: () -> Long) {
    enum class State { IDLE, RECORDING, PROCESSING, ERROR }
    enum class Effect { START, STOP, CANCEL, BUSY }
    var state = State.IDLE
        private set
    private var started = 0L
    private var pressed = 0L
    private var down = false
    private var toggle = false
    private var dragging = false

    fun press(): List<Effect> {
        dragging = false
        if (state == State.PROCESSING) return listOf(Effect.BUSY)
        if (state == State.RECORDING) {
            down = false
            state = State.PROCESSING
            return listOf(Effect.STOP)
        }
        started = now()
        pressed = started
        down = true
        toggle = false
        state = State.RECORDING
        return listOf(Effect.START)
    }

    fun release(): List<Effect> {
        if (!down || dragging || state != State.RECORDING) return emptyList()
        down = false
        if (now() - pressed < 300) { toggle = true; return emptyList() }
        state = State.PROCESSING
        return listOf(Effect.STOP)
    }

    fun move(distanceDp: Float): List<Effect> {
        if (distanceDp <= 24 || !down || state != State.RECORDING || dragging) return emptyList()
        dragging = true
        down = false
        state = State.IDLE
        return listOf(Effect.CANCEL)
    }

    fun cancel(): List<Effect> {
        if (!down || toggle || state != State.RECORDING) return emptyList()
        down = false
        state = State.IDLE
        return listOf(Effect.CANCEL)
    }

    fun tick(): List<Effect> {
        if (state != State.RECORDING || now() - started < 600_000) return emptyList()
        down = false
        state = State.PROCESSING
        return listOf(Effect.STOP)
    }

    fun setState(value: State) { state = value; if (value != State.RECORDING) down = false }
}
