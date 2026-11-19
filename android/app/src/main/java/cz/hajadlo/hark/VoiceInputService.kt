package cz.hajadlo.hark

import android.inputmethodservice.InputMethodService
import android.view.View
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputMethodManager
import android.widget.FrameLayout

/** A voice-only input method; SwiftKey remains the user's typing keyboard. */
class VoiceInputService : InputMethodService() {
    private var container: FrameLayout? = null
    private var panel: VoicePanel? = null
    private var generation = 0
    override fun onCreateInputView(): View = FrameLayout(this).also { container = it }
    override fun onEvaluateFullscreenMode() = false
    override fun onStartInput(attribute: EditorInfo?, restarting: Boolean) {
        closePanel()
        super.onStartInput(attribute, restarting)
    }
    override fun onStartInputView(info: EditorInfo?, restarting: Boolean) {
        super.onStartInputView(info, restarting)
        closePanel()
        val request = generation
        val fresh = VoicePanel(this, { text ->
            if (request == generation && isInputViewShown) {
                if (currentInputConnection?.commitText(text, 1) == true) returnToKeyboard()
                else panel?.showError("Could not insert the transcript. Open hark History to read it.")
            }
        }, { returnToKeyboard() })
        panel = fresh
        container?.addView(fresh)
        fresh.start()
    }
    private fun returnToKeyboard() {
        closePanel()
        if (!switchToPreviousInputMethod()) {
            requestHideSelf(0)
            getSystemService(InputMethodManager::class.java).showInputMethodPicker()
        }
    }
    private fun closePanel() {
        generation++
        panel?.close()
        panel = null
        container?.removeAllViews()
    }
    override fun onFinishInputView(finishingInput: Boolean) {
        closePanel()
        super.onFinishInputView(finishingInput)
    }
    override fun onFinishInput() {
        closePanel()
        super.onFinishInput()
    }
    override fun onWindowHidden() {
        closePanel()
        super.onWindowHidden()
    }
    override fun onDestroy() {
        closePanel()
        super.onDestroy()
    }
}
