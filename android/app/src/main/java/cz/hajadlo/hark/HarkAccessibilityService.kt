package cz.hajadlo.hark

import android.accessibilityservice.AccessibilityService
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.graphics.PixelFormat
import android.graphics.Rect
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.os.SystemClock
import android.view.Gravity
import android.view.MotionEvent
import android.view.WindowManager
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo
import android.view.accessibility.AccessibilityWindowInfo
import android.widget.Toast
import kotlin.math.hypot

class HarkAccessibilityService : AccessibilityService() {
    private val main = Handler(Looper.getMainLooper())
    private val gesture = Gesture { SystemClock.elapsedRealtime() }
    private lateinit var settings: Settings
    private lateinit var wm: WindowManager
    private lateinit var bubble: BubbleView
    private lateinit var layout: WindowManager.LayoutParams
    private var attached = false
    private var positioned = false
    private var dragging = false
    private var dragEligible = false
    private var touchX = 0f
    private var touchY = 0f
    private var originX = 0
    private var originY = 0
    private val screen = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) { refreshVisibility() }
    }
    private val tick = object : Runnable {
        override fun run() { effects(gesture.tick()); main.postDelayed(this, 200) }
    }
    override fun onServiceConnected() {
        instance = this
        settings = Settings(this)
        wm = getSystemService(WindowManager::class.java)
        bubble = BubbleView(this).apply {
            // TalkBack reads the per-state contentDescription that BubbleView sets.
            importantForAccessibility = android.view.View.IMPORTANT_FOR_ACCESSIBILITY_YES
            setOnTouchListener { _, event -> touch(event) }
        }
        layout = WindowManager.LayoutParams(dp(64), dp(64), WindowManager.LayoutParams.TYPE_ACCESSIBILITY_OVERLAY,
            WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE or WindowManager.LayoutParams.FLAG_NOT_TOUCH_MODAL,
            PixelFormat.TRANSLUCENT).apply {
            gravity = Gravity.TOP or Gravity.LEFT
            x = settings.x
            y = settings.y.takeIf { it >= 0 } ?: (wm.currentWindowMetrics.bounds.height() - dp(260))
        }
        clamp()
        registerReceiver(screen, IntentFilter().apply { addAction(Intent.ACTION_SCREEN_OFF); addAction(Intent.ACTION_SCREEN_ON) }, RECEIVER_NOT_EXPORTED)
        gesture.setState(RecordingService.currentState)
        Pending(java.io.File(cacheDir, "recordings")).prune()
        paint()
        refreshVisibility()
        main.post(tick)
    }
    override fun onAccessibilityEvent(event: AccessibilityEvent?) {
        if (::bubble.isInitialized) { clamp(); if (attached) wm.updateViewLayout(bubble, layout); refreshVisibility() }
    }
    override fun onInterrupt() { effects(gesture.cancel()) }
    private fun dp(value: Int) = (value * resources.displayMetrics.density).toInt()
    private fun focused(): AccessibilityNodeInfo? {
        val node = rootInActiveWindow?.findFocus(AccessibilityNodeInfo.FOCUS_INPUT)
        return node?.takeIf { it.isEditable && it.packageName?.toString() != packageName }
    }
    private fun refreshVisibility() {
        val awake = getSystemService(PowerManager::class.java).isInteractive
        val keyboard = windows.firstOrNull { it.type == AccessibilityWindowInfo.TYPE_INPUT_METHOD }
        if (settings.y < 0 && !positioned && keyboard != null) {
            val bounds = Rect().also { keyboard.getBoundsInScreen(it) }
            layout.x = wm.currentWindowMetrics.bounds.width() - dp(80)
            layout.y = bounds.top + dp(16)
            positioned = true
            clamp()
            if (attached) wm.updateViewLayout(bubble, layout)
        }
        val wanted = settings.showBubble && awake && (gesture.state == Gesture.State.RECORDING || gesture.state == Gesture.State.PROCESSING ||
            keyboard != null || focused() != null)
        if (wanted && !attached) { wm.addView(bubble, layout); attached = true }
        if (!wanted && attached) { wm.removeView(bubble); attached = false }
    }
    private fun clamp() {
        val bounds = wm.currentWindowMetrics.bounds
        layout.x = layout.x.coerceIn(0, maxOf(0, bounds.width() - dp(64)))
        layout.y = layout.y.coerceIn(0, maxOf(0, bounds.height() - dp(64)))
    }
    private fun touch(event: MotionEvent): Boolean {
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                touchX = event.rawX; touchY = event.rawY; originX = layout.x; originY = layout.y
                dragging = false
                dragEligible = gesture.state == Gesture.State.IDLE || gesture.state == Gesture.State.ERROR
                effects(gesture.press())
            }
            MotionEvent.ACTION_MOVE -> {
                val distance = hypot(event.rawX - touchX, event.rawY - touchY) / resources.displayMetrics.density
                if (dragEligible && distance > 24 && !dragging) {
                    dragging = true
                    effects(gesture.move(distance))
                }
                if (dragging) {
                    layout.x = originX + (event.rawX - touchX).toInt()
                    layout.y = originY + (event.rawY - touchY).toInt()
                    clamp()
                    if (attached) wm.updateViewLayout(bubble, layout)
                }
            }
            MotionEvent.ACTION_UP -> {
                if (dragging) { settings.x = layout.x; settings.y = layout.y } else effects(gesture.release())
                dragEligible = false
            }
            MotionEvent.ACTION_CANCEL -> { effects(gesture.cancel()); dragEligible = false }
        }
        return true
    }
    private fun effects(effects: List<Gesture.Effect>) {
        for (effect in effects) {
            val command = when (effect) {
                Gesture.Effect.START -> RecordingService.START
                Gesture.Effect.STOP -> RecordingService.STOP
                Gesture.Effect.CANCEL -> RecordingService.CANCEL
                Gesture.Effect.BUSY -> { signal("Busy"); continue }
            }
            try { RecordingService.command(this, command) }
            catch (_: RuntimeException) { stateChanged(Gesture.State.ERROR, "Could not start microphone service") }
        }
        paint()
        refreshVisibility()
    }
    fun stateChanged(state: Gesture.State, message: String) {
        gesture.setState(state)
        paint()
        refreshVisibility()
        if (message == "Busy") signal(message)
        if (state == Gesture.State.ERROR) main.postDelayed({
            if (gesture.state == Gesture.State.ERROR) { gesture.setState(Gesture.State.IDLE); paint(); refreshVisibility() }
        }, 2000)
    }
    private fun paint() { bubble.state = gesture.state }
    fun signal(message: String) { Toast.makeText(this, message, Toast.LENGTH_SHORT).show() }
    /** Pin the visible editor behind SwiftKey's helper, without saving or logging its text. */
    fun keyboardTarget(caller: String?): KeyboardTarget? {
        val node = windows.filter { it.type == AccessibilityWindowInfo.TYPE_APPLICATION }
            .sortedByDescending { it.layer }
            .firstNotNullOfOrNull { it.root?.findFocus(AccessibilityNodeInfo.FOCUS_INPUT)?.takeIf { node ->
                node.isEditable && node.packageName?.toString() != packageName && node.packageName?.toString() != caller
            } } ?: return null
        return KeyboardTarget(node, caller)
    }
    inner class KeyboardTarget internal constructor(private val node: AccessibilityNodeInfo, private val caller: String?) {
        private val editorPackage = node.packageName?.toString()
        private val editorWindow = node.windowId
        private var delivered = false
        fun deliver(text: String) {
            if (delivered) return
            delivered = true
            val deadline = SystemClock.elapsedRealtime() + 2500
            fun failed() = signal("Transcribed; open hark History to read the text")
            val attempt = object : Runnable {
                override fun run() {
                    if (instance !== this@HarkAccessibilityService) { failed(); return }
                    val root = rootInActiveWindow
                    val activePackage = root?.packageName?.toString()
                    if (root != null && activePackage == editorPackage && root.windowId == editorWindow) {
                        if (!node.refresh() || !node.isEditable) { failed(); return }
                        val focused = root.findFocus(AccessibilityNodeInfo.FOCUS_INPUT)
                        if (focused != null && focused != node) { failed(); return }
                        if (focused == node && node.isFocused) {
                            val old = if (node.isShowingHintText) "" else node.text?.toString().orEmpty()
                            val edit = TextEdit.replace(old, node.textSelectionStart, node.textSelectionEnd, text)
                            if (edit == null || !node.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, Bundle().apply {
                                    putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, edit.text)
                                })) { failed(); return }
                            node.performAction(AccessibilityNodeInfo.ACTION_SET_SELECTION, Bundle().apply {
                                putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_START_INT, edit.cursor)
                                putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_END_INT, edit.cursor)
                            })
                            return
                        }
                        // Some editors drop focus while SwiftKey's helper is on screen.
                        if (focused == null) node.performAction(AccessibilityNodeInfo.ACTION_FOCUS)
                    } else if (activePackage != null && activePackage != packageName && activePackage != caller) {
                        failed(); return
                    }
                    if (SystemClock.elapsedRealtime() >= deadline) { failed(); return }
                    main.postDelayed(this, 50)
                }
            }
            // Run after RecognizeActivity finishes and SwiftKey dismisses its helper.
            main.post(attempt)
        }
    }
    fun insert(text: String): Boolean {
        if (text.isEmpty()) return true
        val connection = inputMethod?.currentInputConnection
        if (connection != null) {
            try { connection.commitText(text, 1, null); return true } catch (_: RuntimeException) { }
        }
        val node = focused() ?: return false
        val old = node.text?.toString().orEmpty()
        val start = node.textSelectionStart
        val end = node.textSelectionEnd
        if (start !in 0..old.length || end !in 0..old.length) return false
        val left = minOf(start, end)
        val right = maxOf(start, end)
        val replacement = old.substring(0, left) + text + old.substring(right)
        if (!node.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, Bundle().apply {
                putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, replacement)
            })) return false
        return node.performAction(AccessibilityNodeInfo.ACTION_SET_SELECTION, Bundle().apply {
            putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_START_INT, left + text.length)
            putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_END_INT, left + text.length)
        })
    }
    override fun onDestroy() {
        instance = null
        main.removeCallbacksAndMessages(null)
        if (::bubble.isInitialized) {
            unregisterReceiver(screen)
            if (attached) wm.removeView(bubble)
        }
        super.onDestroy()
    }
    companion object { var instance: HarkAccessibilityService? = null; private set }
}
