package cz.hajadlo.hark

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.ResultReceiver
import android.widget.Button
import android.widget.LinearLayout
import android.widget.TextView
import java.util.UUID

/** One visible voice request; closing it permanently rejects late upload results. */
class VoicePanel(context: Context, private val result: (String) -> Unit, cancel: () -> Unit) : LinearLayout(context) {
    private val session = UUID.randomUUID().toString()
    private var started = false
    private var closed = false
    private var state = Gesture.State.IDLE
    private val status = TextView(context)
    private val record = Button(context)
    private val meter = MicLevelView(context)
    private val receiver = object : ResultReceiver(Handler(Looper.getMainLooper())) {
        override fun onReceiveResult(resultCode: Int, resultData: Bundle?) {
            if (closed) return
            when (resultCode) {
                RecordingService.VOICE_LEVEL -> meter.sample(resultData?.getFloat("rms") ?: 0f)
                RecordingService.VOICE_RESULT -> {
                    val text = resultData?.getString("text").orEmpty()
                    if (text.isEmpty()) showError("Nothing heard. Try again.")
                    else { closed = true; result(text) }
                }
                RecordingService.VOICE_STATE -> when (resultData?.getString("state")) {
                    Gesture.State.RECORDING.name -> showState(Gesture.State.RECORDING, "Listening…")
                    Gesture.State.PROCESSING.name -> showState(Gesture.State.PROCESSING, "Transcribing…")
                    Gesture.State.ERROR.name -> showError(resultData.getString("message").orEmpty())
                    Gesture.State.IDLE.name -> if (state != Gesture.State.ERROR) showState(Gesture.State.IDLE, "Ready")
                }
            }
        }
    }
    init {
        orientation = VERTICAL
        val pad = (20 * resources.displayMetrics.density).toInt()
        setPadding(pad, pad, pad, pad)
        val background = android.util.TypedValue().also { context.theme.resolveAttribute(android.R.attr.colorBackground, it, true) }.data
        setBackgroundColor(background)
        addView(TextView(context).apply { text = "hark"; textSize = 22f })
        addView(status.apply {
            text = "Ready"; textSize = 17f
            accessibilityLiveRegion = ACCESSIBILITY_LIVE_REGION_POLITE
            setPadding(0, pad / 2, 0, pad / 2)
        })
        addView(meter, LayoutParams(LayoutParams.MATCH_PARENT, (56 * resources.displayMetrics.density).toInt()))
        addView(record.apply {
            tag = "record-button"
            text = "Record"; isAllCaps = false
            setOnClickListener {
                if (state == Gesture.State.RECORDING) {
                    showState(Gesture.State.PROCESSING, "Transcribing…")
                    RecordingService.command(context, RecordingService.STOP, session = session)
                } else start()
            }
        })
        addView(Button(context).apply { text = "Cancel"; isAllCaps = false; setOnClickListener { close(); cancel() } })
    }
    fun start() {
        if (closed) return
        if (context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            showError("Open hark and grant microphone permission first.")
            return
        }
        try {
            val settings = Settings(context)
            BaseUrl.normalize(settings.base)
            require(settings.token.isNotBlank())
            showState(Gesture.State.PROCESSING, "Starting microphone…")
            RecordingService.command(context, RecordingService.START, session = session, receiver = receiver)
            started = true
        } catch (e: RuntimeException) {
            showError(if (e is IllegalArgumentException) "Open hark and check the server URL and token first" else "Could not start the microphone; check permissions")
        }
    }
    fun showError(message: String) = showState(Gesture.State.ERROR, message)
    fun showUnavailable(message: String) { showError(message); record.isEnabled = false }
    private fun showState(value: Gesture.State, message: String) {
        state = value
        if (meter.recording != (value == Gesture.State.RECORDING)) meter.recording = value == Gesture.State.RECORDING
        status.text = message
        record.isEnabled = value != Gesture.State.PROCESSING
        record.text = if (value == Gesture.State.RECORDING) "Stop and transcribe" else "Record again"
    }
    fun close() {
        closed = true
        meter.recording = false
        if (started) {
            started = false
            try { RecordingService.command(context, RecordingService.CANCEL, session = session) }
            catch (_: RuntimeException) { }
        }
    }
}
