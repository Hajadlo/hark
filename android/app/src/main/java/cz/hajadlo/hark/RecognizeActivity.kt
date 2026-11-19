package cz.hajadlo.hark

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Bundle
import android.speech.RecognizerIntent
import android.view.Gravity
import android.view.ViewGroup
import android.view.WindowManager

/** SwiftKey owns insertion: return a recognizer result rather than using accessibility. */
class RecognizeActivity : Activity() {
    private lateinit var panel: VoicePanel
    private var permissionRequested = false
    private var resumed = false
    private var unavailable = false
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setResult(RESULT_CANCELED)
        if (intent.action != RecognizerIntent.ACTION_RECOGNIZE_SPEECH || savedInstanceState != null) {
            finish()
            return
        }
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        window.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_STATE_ALWAYS_HIDDEN)
        val direct = Settings(this).directKeyboardInsertion
        val target = if (direct) HarkAccessibilityService.instance?.keyboardTarget(callingPackage) else null
        panel = VoicePanel(this, { text ->
            if (target != null) target.deliver(text)
            else setResult(RESULT_OK, Intent().putStringArrayListExtra(RecognizerIntent.EXTRA_RESULTS, arrayListOf(text)))
            finish()
        }, { finish() })
        if (direct && target == null) {
            unavailable = true
            panel.showUnavailable(if (HarkAccessibilityService.instance == null)
                "Enable hark in Accessibility settings for direct keyboard insertion."
                else "Could not find the original text field. Cancel, tap the field, and try again.")
        }
        setContentView(panel)
        window.setLayout(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
        window.setGravity(Gravity.BOTTOM)
        setFinishOnTouchOutside(true)
    }
    override fun onResume() {
        super.onResume()
        if (!::panel.isInitialized || resumed || unavailable) return
        resumed = true
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) panel.start()
        else {
            permissionRequested = true
            requestPermissions(arrayOf(Manifest.permission.RECORD_AUDIO), 1)
        }
    }
    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (!::panel.isInitialized || isFinishing || requestCode != 1 || !permissionRequested) return
        if (grantResults.firstOrNull() == PackageManager.PERMISSION_GRANTED) panel.start()
        else panel.showError("Microphone permission is required. Enable it in hark’s App info → Permissions.")
    }
    override fun onStop() {
        // Backgrounding dismisses this request. A late result must not type into another app.
        if (::panel.isInitialized) panel.close()
        if (!isFinishing) finish()
        super.onStop()
    }
}
