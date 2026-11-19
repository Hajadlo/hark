package cz.hajadlo.hark

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Handler
import android.os.Bundle
import android.os.IBinder
import android.os.Looper
import android.os.PowerManager
import android.os.ResultReceiver
import android.widget.Toast
import androidx.core.app.NotificationCompat
import java.io.File
import java.util.concurrent.Executors

/** One serial worker owns clips and uploads; only AudioRecord's read loop has its own thread. */
class RecordingService : Service() {
    private val worker = Executors.newSingleThreadExecutor()
    private val main = Handler(Looper.getMainLooper())
    private lateinit var pending: Pending
    private lateinit var settings: Settings
    private var recorder: MicRecorder? = null
    private var clip: Pending.Clip? = null
    private var wake: PowerManager.WakeLock? = null
    private var foreground = false
    private var active = false
    private var operation = 0
    private var workerOperation = 0
    @Volatile private var voiceClient: VoiceClient? = null
    private data class VoiceClient(val session: String, val receiver: ResultReceiver)
    override fun onBind(intent: Intent): IBinder? = null
    override fun onCreate() {
        super.onCreate()
        settings = Settings(this)
        pending = Pending(File(cacheDir, "recordings"))
        val manager = getSystemService(NotificationManager::class.java)
        // Channel importance is immutable after creation, so retire the old indicator channel.
        manager.deleteNotificationChannel("dictation")
        manager.createNotificationChannel(NotificationChannel(QUIET_CHANNEL, "Recording indicator", NotificationManager.IMPORTANCE_MIN).apply {
            setSound(null, null)
            enableVibration(false)
            setShowBadge(false)
        })
        manager.createNotificationChannel(NotificationChannel(FAILURE_CHANNEL, "Failed dictations", NotificationManager.IMPORTANCE_DEFAULT))
        worker.execute { pending.prune() }
    }
    private fun action(action: String, name: String = ""): PendingIntent = PendingIntent.getService(
        this, (action + name).hashCode(), Intent(this, RecordingService::class.java)
            .setAction(action).putExtra("clip", name), PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
    private fun notification(message: String, channel: String): NotificationCompat.Builder = NotificationCompat.Builder(this, channel)
        .setSmallIcon(android.R.drawable.ic_btn_speak_now).setContentTitle("hark").setContentText(message)
        .setContentIntent(PendingIntent.getActivity(this, 0, Intent(this, SettingsActivity::class.java), PendingIntent.FLAG_IMMUTABLE))
    private fun foreground(message: String, type: Int) {
        val notice = notification(message, QUIET_CHANNEL).setOngoing(true).setSilent(true)
            .setForegroundServiceBehavior(Notification.FOREGROUND_SERVICE_DEFERRED)
        startForeground(ACTIVE, notice.build(), type)
        foreground = true
    }
    private fun state(state: Gesture.State, message: String = "") {
        currentState = state
        val client = voiceClient
        main.post {
            client?.receiver?.send(VOICE_STATE, Bundle().apply {
                putString("state", state.name); putString("message", message)
            })
            HarkAccessibilityService.instance?.stateChanged(state, message)
        }
    }
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val command = intent?.action ?: return START_NOT_STICKY
        val receiver = intent.getParcelableExtra("receiver", ResultReceiver::class.java)
        val session = intent.getStringExtra("session")
        if ((command == START || command == RETRY || command == DISCARD) && active) {
            if (receiver != null) receiver.send(VOICE_STATE, Bundle().apply {
                putString("state", Gesture.State.ERROR.name); putString("message", "Another dictation is already running")
            }) else state(currentState, "Busy")
            return START_NOT_STICKY
        }
        if ((command == STOP || command == CANCEL) && !active) return START_NOT_STICKY
        // A closing keyboard panel must never cancel a later recording or a bubble recording.
        if ((command == STOP || command == CANCEL) && session != voiceClient?.session) return START_NOT_STICKY
        if (command == START || command == RETRY || command == DISCARD) {
            active = true
            operation++
            voiceClient = if (command == START && receiver != null && session != null) VoiceClient(session, receiver) else null
        }
        val generation = operation
        // Promotion happens synchronously, before the worker can construct AudioRecord.
        try {
            if (command == START) {
                foreground("Recording", ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE)
                state(Gesture.State.RECORDING)
            } else if (command == RETRY && currentState != Gesture.State.RECORDING && currentState != Gesture.State.PROCESSING) {
                foreground("Retrying upload", ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
                state(Gesture.State.PROCESSING)
            }
            if (command == STOP || command == CANCEL) state(Gesture.State.PROCESSING)
        } catch (_: RuntimeException) {
            active = false
            state(Gesture.State.ERROR, "Microphone service could not start; check permissions")
            stopSelf(startId)
            return START_NOT_STICKY
        }
        val name = intent.getStringExtra("clip").orEmpty()
        val startingBase = intent.getStringExtra("base") ?: settings.base
        val startingMode = intent.getStringExtra("mode") ?: settings.mode
        worker.execute {
            when (command) {
                START -> if (recorder == null && clip == null) { workerOperation = generation; begin(startingBase, startingMode) }
                STOP -> finish(false)
                CANCEL -> finish(true)
                RETRY -> if (recorder == null) { workerOperation = generation; retry(name) }
                DISCARD -> if (recorder == null && clip == null) {
                    workerOperation = generation
                    pending.find(name)?.let { pending.discard(it); clearFailure(it) }
                    done()
                }
            }
        }
        return START_NOT_STICKY
    }
    private fun begin(startingBase: String, startingMode: String) {
        try {
            val base = BaseUrl.normalize(startingBase)
            require(settings.token.isNotBlank()) { "Set the server token first" }
            val created = pending.create(base, startingMode)
            clip = created
            activeFileName = created.file.name
            wake = getSystemService(PowerManager::class.java).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "hark:recording").apply {
                acquire(1_200_000)
            }
            val client = voiceClient
            recorder = MicRecorder(this, created.file, onLevel = { rms ->
                client?.receiver?.send(VOICE_LEVEL, Bundle().apply { putFloat("rms", rms) })
            }) {
                if (!worker.isShutdown) worker.execute {
                    if (clip != created) return@execute
                    finish(false)
                }
            }
        } catch (_: Exception) {
            clip?.let { pending.discard(it) }; clip = null
            state(Gesture.State.ERROR, "Recording could not start; check microphone permission and settings")
            done(preserveError = true)
        }
    }
    private fun finish(cancel: Boolean) {
        val current = clip ?: run { done(); return }
        val error = recorder?.stop()
        recorder = null
        if (cancel || error != null || current.file.length() <= 44) {
            pending.discard(current)
            clip = null
            if (error != null) state(Gesture.State.ERROR, error)
            done(preserveError = error != null)
            return
        }
        main.post {
            if (foreground && currentState == Gesture.State.PROCESSING)
                foreground("Processing", ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
        }
        state(Gesture.State.PROCESSING)
        send(current)
    }
    private fun retry(name: String) {
        val current = pending.find(name)
        if (current == null) { done(); return }
        clip = current
        activeFileName = current.file.name
        send(current)
    }
    private fun send(current: Pending.Clip) {
        val client = voiceClient
        try {
            val text = Uploader({
                val config = settings.snapshot()
                // Validate the same snapshot whose token is sent; repeat this for every attempt.
                require(BaseUrl.normalize(config.base) == current.base) { "Restore this recording's server URL before retrying" }
                config
            }, BuildConfig.VERSION_NAME).upload(current.file, current.mode)
            pending.discard(current)
            clearFailure(current)
            main.post {
                if (client != null) {
                    client.receiver.send(VOICE_RESULT, Bundle().apply { putString("text", text) })
                    return@post
                }
                val message = when {
                    text.isEmpty() -> "Nothing heard"
                    HarkAccessibilityService.instance?.insert(text) == true -> null
                    else -> "Transcribed; open History to read the text"
                }
                if (message != null) Toast.makeText(this, message, Toast.LENGTH_SHORT).show()
            }
        } catch (e: Exception) {
            val message = if (e is ApiFailure || e is IllegalArgumentException) e.message.orEmpty() else "Upload failed"
            val safe = message.replace(settings.token.takeIf { it.isNotEmpty() } ?: "\u0000", "[redacted]").take(240)
            getSystemService(NotificationManager::class.java).notify(current.file.name.hashCode(), notification(safe, FAILURE_CHANNEL)
                .addAction(0, "Retry", action(RETRY, current.file.name))
                .addAction(0, "Discard", action(DISCARD, current.file.name)).build())
            state(Gesture.State.ERROR, safe)
        } finally { clip = null; done(preserveError = true) }
    }
    private fun clearFailure(current: Pending.Clip) {
        getSystemService(NotificationManager::class.java).cancel(current.file.name.hashCode())
    }
    private fun done(preserveError: Boolean = false) {
        activeFileName = null
        wake?.let { if (it.isHeld) it.release() }; wake = null
        val completedOperation = workerOperation
        main.post {
            if (operation != completedOperation) return@post
            if (!preserveError || currentState != Gesture.State.ERROR) state(Gesture.State.IDLE)
            active = false
            voiceClient = null
            if (foreground) { stopForeground(STOP_FOREGROUND_REMOVE); foreground = false }
            stopSelf()
        }
    }
    override fun onDestroy() {
        worker.execute {
            recorder?.stop(); recorder = null
            wake?.let { if (it.isHeld) it.release() }; wake = null
        }
        worker.shutdown()
        currentState = Gesture.State.IDLE
        activeFileName = null
        super.onDestroy()
    }
    override fun onTimeout(startId: Int, fgsType: Int) { state(Gesture.State.ERROR, "Service timed out"); stopSelf() }
    companion object {
        const val QUIET_CHANNEL = "dictation-quiet"
        const val FAILURE_CHANNEL = "dictation-failures"
        const val ACTIVE = 1
        const val START = "start"
        const val STOP = "stop"
        const val CANCEL = "cancel"
        const val RETRY = "retry"
        const val DISCARD = "discard"
        const val VOICE_STATE = 1
        const val VOICE_RESULT = 2
        const val VOICE_LEVEL = 3
        @Volatile var currentState = Gesture.State.IDLE
            private set
        @Volatile var activeFileName: String? = null
            private set
        fun command(context: Context, action: String, name: String = "", session: String? = null, receiver: ResultReceiver? = null) {
            val intent = Intent(context, RecordingService::class.java).setAction(action).putExtra("clip", name)
                .putExtra("session", session).putExtra("receiver", receiver)
            if (action == START) {
                val settings = Settings(context)
                intent.putExtra("base", settings.base).putExtra("mode", settings.mode)
            }
            if (action == START || action == RETRY) context.startForegroundService(intent) else context.startService(intent)
        }
    }
}
