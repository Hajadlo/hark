package cz.hajadlo.hark

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import java.io.File
import java.io.IOException

class MicRecorder(context: Context, file: File, private val onLevel: (Float) -> Unit = {}, private val finished: (String?) -> Unit) {
    private val audio: AudioRecord
    private val wav: Wav
    @Volatile private var running = true
    @Volatile private var failure: String? = null
    private val thread: Thread
    init {
        if (context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED)
            throw IOException("Microphone permission is missing")
        val minimum = AudioRecord.getMinBufferSize(Wav.RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
        require(minimum > 0) { "16 kHz microphone recording is unavailable" }
        audio = AudioRecord(MediaRecorder.AudioSource.MIC, Wav.RATE, AudioFormat.CHANNEL_IN_MONO,
            AudioFormat.ENCODING_PCM_16BIT, maxOf(minimum, 4096))
        try {
            check(audio.state == AudioRecord.STATE_INITIALIZED) { "Microphone could not be initialised" }
            wav = Wav(file)
            try {
                audio.startRecording()
                check(audio.recordingState == AudioRecord.RECORDSTATE_RECORDING) { "Microphone did not start" }
            } catch (e: Exception) { wav.close(); throw e }
        } catch (e: Exception) { audio.release(); throw e }
        thread = Thread({
            var samples = 0
            try {
                val buffer = ShortArray(2048)
                while (running && samples < Wav.MAX_SAMPLES) {
                    val count = audio.read(buffer, 0, minOf(buffer.size, Wav.MAX_SAMPLES - samples))
                    if (count < 0 && running) throw IOException("Microphone read failed")
                    if (count > 0) {
                        wav.write(buffer, count)
                        samples += count
                        onLevel(MicLevel.rms(buffer, count))
                    }
                }
            } catch (_: Exception) { failure = "Microphone recording failed" }
            finally {
                try { wav.close() } catch (_: Exception) { failure = "Could not save recording" }
                if (running) finished(failure) // The sample cap is enforced even if UI timers are delayed.
            }
        }, "hark-microphone").also { it.start() }
    }
    fun stop(): String? {
        running = false
        try { audio.stop() } catch (_: IllegalStateException) { }
        thread.join()
        audio.release()
        return failure
    }
}
