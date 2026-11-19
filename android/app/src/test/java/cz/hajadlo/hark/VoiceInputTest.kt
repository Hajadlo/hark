package cz.hajadlo.hark

import android.Manifest
import android.app.Activity
import android.app.Application
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Bundle
import android.os.Looper
import android.os.ResultReceiver
import android.speech.RecognizerIntent
import android.view.inputmethod.InputMethodInfo
import android.widget.Button
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class VoiceInputTest {
    private lateinit var app: Application
    @Before fun setup() {
        app = RuntimeEnvironment.getApplication()
        shadowOf(app).grantPermissions(Manifest.permission.RECORD_AUDIO)
        Settings(app).save("https://hark.example", "synthetic-test-token", "clean")
    }
    private fun nextCommand(): Intent = checkNotNull(shadowOf(app).nextStartedService)
    private fun receiver(command: Intent) = checkNotNull(command.getParcelableExtra("receiver", ResultReceiver::class.java))
    private fun result(receiver: ResultReceiver, text: String) {
        receiver.send(RecordingService.VOICE_RESULT, Bundle().apply { putString("text", text) })
        shadowOf(Looper.getMainLooper()).idle()
    }
    @Test fun keyboardRequestReturnsAndroidResultWithoutAccessibility() {
        val intent = Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH).setClass(app, RecognizeActivity::class.java)
        val controller = Robolectric.buildActivity(RecognizeActivity::class.java, intent).setup()
        val command = nextCommand()
        assertEquals(RecordingService.START, command.action)
        result(receiver(command), "A synthetic sentence.")
        assertEquals(Activity.RESULT_OK, shadowOf(controller.get()).resultCode)
        assertEquals(listOf("A synthetic sentence."), shadowOf(controller.get()).resultIntent
            .getStringArrayListExtra(RecognizerIntent.EXTRA_RESULTS))
        assertTrue(controller.get().isFinishing)
        controller.pause().stop().destroy()
    }
    @Test fun leavingCallerCancelsOnlyItsSessionAndIgnoresLateResult() {
        val intent = Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH).setClass(app, RecognizeActivity::class.java)
        val controller = Robolectric.buildActivity(RecognizeActivity::class.java, intent).setup()
        val command = nextCommand()
        controller.pause().stop()
        val cancel = nextCommand()
        assertEquals(RecordingService.CANCEL, cancel.action)
        assertEquals(command.getStringExtra("session"), cancel.getStringExtra("session"))
        result(receiver(command), "Must not be inserted.")
        assertEquals(Activity.RESULT_CANCELED, shadowOf(controller.get()).resultCode)
        controller.destroy()
    }
    @Test fun directInsertionWithoutAccessibilityDoesNotRecordOrReturnToBrokenHandoff() {
        Settings(app).directKeyboardInsertion = true
        val intent = Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH).setClass(app, RecognizeActivity::class.java)
        val controller = Robolectric.buildActivity(RecognizeActivity::class.java, intent).setup()
        assertNull(shadowOf(app).nextStartedService)
        assertFalse(controller.get().findViewById<android.view.View>(android.R.id.content)
            .findViewWithTag<Button>("record-button").isEnabled)
        assertEquals(Activity.RESULT_CANCELED, shadowOf(controller.get()).resultCode)
        controller.pause().stop().destroy()
    }
    @Test fun panelStopsItsRecordingAndDeliversResultOnlyOnce() {
        val results = mutableListOf<String>()
        val panel = VoicePanel(app, { results.add(it) }, {})
        panel.start()
        val command = nextCommand()
        val callback = receiver(command)
        callback.send(RecordingService.VOICE_STATE, Bundle().apply { putString("state", Gesture.State.RECORDING.name) })
        shadowOf(Looper.getMainLooper()).idle()
        val stop = panel.findViewWithTag<Button>("record-button")
        assertEquals("Stop and transcribe", stop.text.toString())
        stop.performClick()
        assertFalse(stop.isEnabled)
        assertEquals(RecordingService.STOP, nextCommand().action)
        result(callback, "First result.")
        result(callback, "Duplicate result.")
        assertEquals(listOf("First result."), results)
        panel.close()
    }
    @Test fun liveMeterUsesCaptureEventsAndStopsUpdatingAfterStop() {
        val panel = VoicePanel(app, {}, {})
        panel.start()
        val callback = receiver(nextCommand())
        val meter = panel.findViewWithTag<MicLevelView>("microphone-meter")
        callback.send(RecordingService.VOICE_STATE, Bundle().apply { putString("state", Gesture.State.RECORDING.name) })
        callback.send(RecordingService.VOICE_LEVEL, Bundle().apply { putFloat("rms", .25f) })
        shadowOf(Looper.getMainLooper()).idle()
        assertTrue(meter.recording)
        assertEquals(.25f, meter.level, 0f)
        panel.findViewWithTag<Button>("record-button").performClick()
        callback.send(RecordingService.VOICE_LEVEL, Bundle().apply { putFloat("rms", .75f) })
        shadowOf(Looper.getMainLooper()).idle()
        assertFalse(meter.recording)
        assertEquals(0f, meter.level, 0f)
        panel.close()
    }
    @Test fun cancelledPanelCannotDeliverIntoANewRequest() {
        val results = mutableListOf<String>()
        val first = VoicePanel(app, { results.add(it) }, {})
        first.start()
        val old = nextCommand()
        first.close()
        assertEquals(RecordingService.CANCEL, nextCommand().action)
        val second = VoicePanel(app, { results.add(it) }, {})
        second.start()
        val fresh = nextCommand()
        assertNotEquals(old.getStringExtra("session"), fresh.getStringExtra("session"))
        result(receiver(old), "Old request.")
        result(receiver(fresh), "Current request.")
        assertEquals(listOf("Current request."), results)
        second.close()
    }
    @Test fun missingPermissionDoesNotStartMicrophone() {
        shadowOf(app).denyPermissions(Manifest.permission.RECORD_AUDIO)
        val panel = VoicePanel(app, { fail("Unexpected transcript") }, {})
        panel.start()
        assertNull(shadowOf(app).nextStartedService)
        panel.close()
    }
    @Test fun voiceKeyboardIsDiscoverableAndAuxiliary() {
        val resolved = app.packageManager.queryIntentServices(Intent("android.view.InputMethod"), PackageManager.GET_META_DATA)
            .single { it.serviceInfo.name == VoiceInputService::class.java.name }
        assertEquals("android.permission.BIND_INPUT_METHOD", resolved.serviceInfo.permission)
        val info = InputMethodInfo(app, resolved)
        assertEquals(1, info.subtypeCount)
        assertEquals("voice", info.getSubtypeAt(0).mode)
        assertTrue(info.getSubtypeAt(0).isAuxiliary)
    }
}
