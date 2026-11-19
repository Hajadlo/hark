package cz.hajadlo.hark

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Typeface
import android.net.Uri
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.provider.Settings as SystemSettings
import android.text.InputType
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.ArrayAdapter
import android.widget.AutoCompleteTextView
import android.widget.Button
import android.widget.CheckBox
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import java.io.File
import java.text.DateFormat
import java.util.concurrent.Executors

class SettingsActivity : Activity() {
    private lateinit var settings: Settings
    private lateinit var pending: Pending
    private lateinit var content: LinearLayout
    private lateinit var url: EditText
    private lateinit var token: EditText
    private lateinit var mode: AutoCompleteTextView
    private lateinit var status: TextView
    private lateinit var clips: LinearLayout
    private lateinit var pendingTitle: TextView
    private val worker = Executors.newSingleThreadExecutor()
    private val main = Handler(Looper.getMainLooper())
    private var shownClips: List<String>? = null
    private val poll = object : Runnable {
        override fun run() { refreshPending(); main.postDelayed(this, 1000) }
    }
    private val density get() = resources.displayMetrics.density
    private fun dp(value: Int) = (value * density).toInt()
    private fun themeColor(attr: Int): Int = TypedValue().also { theme.resolveAttribute(attr, it, true) }.data

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        settings = Settings(this)
        pending = Pending(File(cacheDir, "recordings"))
        pending.prune()
        content = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(20), dp(16), dp(20), dp(32))
        }
        val scroll = ScrollView(this).apply { addView(content); isFillViewport = true }
        // Target 35+ uses edge-to-edge; keep setup controls out of the status/navigation bars.
        scroll.setOnApplyWindowInsetsListener { view, insets ->
            val bars = insets.getInsets(android.view.WindowInsets.Type.systemBars())
            view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
            insets
        }
        setContentView(scroll)

        // Title
        text("hark", 26f, Typeface.BOLD, android.R.attr.textColorPrimary)
        text("Dictation to your own server", 14f, Typeface.NORMAL, android.R.attr.textColorSecondary, top = 2)

        // Server
        section("Server")
        caption("Server URL")
        url = EditText(this).apply {
            setSingleLine()
            hint = "https://host.tailb9a5ba.ts.net:8766"
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI
            setText(settings.base)
        }
        content.addView(url, field())
        caption("Token (stored only on this device)", top = 12)
        token = EditText(this).apply {
            setSingleLine()
            hint = "Bearer token from the server's api-token file"
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD
            importantForAutofill = View.IMPORTANT_FOR_AUTOFILL_NO
            setText(settings.token)
        }
        content.addView(token, field())
        buttonRow("Save" to { save() }, "Test" to { test() })
        status = TextView(this).apply {
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 14f)
            setTextColor(themeColor(android.R.attr.textColorSecondary))
            minHeight = dp(20)
        }
        content.addView(status, margins(top = 8))

        // Mode
        section("Mode")
        caption("After Test, tap to choose a mode; or type a name")
        mode = AutoCompleteTextView(this).apply {
            setSingleLine()
            hint = "clean"
            threshold = 0
            setText(settings.mode)
            setOnClickListener { showDropDown() }
        }
        content.addView(mode, field())

        // Phone setup
        section("Phone setup")
        caption("Keyboard microphone: in SwiftKey → Rich input, turn off Multi-modal voice typing. " +
            "Tap SwiftKey’s microphone and choose hark voice input if Android offers a chooser. " +
            "This uses your server.")
        content.addView(CheckBox(this).apply {
            text = "Insert keyboard dictation directly"
            isChecked = settings.directKeyboardInsertion
            setOnCheckedChangeListener { _, checked -> settings.directKeyboardInsertion = checked }
        }, margins(top = 8))
        caption("Enable hark in Accessibility settings for direct insertion if a keyboard loses dictated text.")
        content.addView(CheckBox(this).apply {
            text = "Show floating microphone"
            isChecked = settings.showBubble
            setOnCheckedChangeListener { _, checked -> settings.showBubble = checked }
        }, margins(top = 8))
        linkButton("Keyboard settings", "Enable hark voice input alongside SwiftKey; keep SwiftKey as your typing keyboard") {
            startActivity(Intent(SystemSettings.ACTION_INPUT_METHOD_SETTINGS))
        }
        linkButton("Accessibility settings", "Required for direct insertion and the optional floating microphone") {
            startActivity(Intent(SystemSettings.ACTION_ACCESSIBILITY_SETTINGS))
        }
        linkButton("App info", "Allow restricted settings if accessibility is blocked") {
            startActivity(Intent(SystemSettings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.parse("package:$packageName")))
        }
        linkButton("Permissions", "Microphone and notifications") {
            val missing = arrayOf(Manifest.permission.RECORD_AUDIO, Manifest.permission.POST_NOTIFICATIONS)
                .filter { checkSelfPermission(it) != PackageManager.PERMISSION_GRANTED }
            if (missing.isEmpty()) status.text = "Permissions granted"
            else requestPermissions(missing.toTypedArray(), 1)
        }
        caption("Tap the bubble to toggle recording; hold at least 300 ms to stop on release. " +
            "Drag to move it; dragging cancels. Recordings stop after 10 minutes.", top = 8)

        // Pending recordings
        pendingTitle = section("Pending recordings")
        clips = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        content.addView(clips)

        // History
        section("History")
        linkButton("Open History", "Transcripts, corrections and the glossary, on the server") {
            if (save()) startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(settings.base)))
        }
    }

    // ---- Layout helpers ------------------------------------------------------------------------

    private fun margins(top: Int = 0, bottom: Int = 0) =
        LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
            .apply { topMargin = dp(top); bottomMargin = dp(bottom) }

    private fun field() = margins(top = 2)

    private fun text(value: String, sp: Float, style: Int, colorAttr: Int, top: Int = 0): TextView =
        TextView(this).apply {
            text = value
            setTextSize(TypedValue.COMPLEX_UNIT_SP, sp)
            setTypeface(Typeface.DEFAULT, style)
            setTextColor(themeColor(colorAttr))
            content.addView(this, margins(top = top))
        }

    /** A section heading in the accent colour with a hairline above it. */
    private fun section(title: String): TextView {
        content.addView(View(this).apply { setBackgroundColor(themeColor(android.R.attr.colorControlHighlight)) },
            LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, dp(1)).apply { topMargin = dp(24) })
        return text(title, 13f, Typeface.BOLD, android.R.attr.colorAccent, top = 14).apply {
            letterSpacing = 0.06f
            isAllCaps = true
        }
    }

    private fun caption(value: String, top: Int = 0): TextView =
        text(value, 13f, Typeface.NORMAL, android.R.attr.textColorSecondary, top = top)

    private fun buttonRow(vararg actions: Pair<String, () -> Unit>) {
        val row = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
        actions.forEachIndexed { index, (label, action) ->
            row.addView(Button(this).apply {
                text = label
                isAllCaps = false
                setOnClickListener { action() }
            }, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply {
                if (index > 0) marginStart = dp(8)
            })
        }
        content.addView(row, margins(top = 12))
    }

    /** A full-width row: title plus a one-line explanation, tappable as a whole. */
    private fun linkButton(title: String, detail: String, action: () -> Unit) {
        val row = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            isClickable = true
            isFocusable = true
            setBackgroundResource(TypedValue().also {
                theme.resolveAttribute(android.R.attr.selectableItemBackground, it, true)
            }.resourceId)
            setPadding(dp(12), dp(10), dp(12), dp(10))
            contentDescription = "$title. $detail"
            setOnClickListener { action() }
        }
        row.addView(TextView(this).apply {
            text = title
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 16f)
            setTextColor(themeColor(android.R.attr.textColorPrimary))
        })
        row.addView(TextView(this).apply {
            text = detail
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 13f)
            setTextColor(themeColor(android.R.attr.textColorSecondary))
        })
        content.addView(row, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
            .apply { marginStart = -dp(12); marginEnd = -dp(12); topMargin = dp(2) })
    }

    // ---- Behaviour (unchanged) ------------------------------------------------------------------

    private fun save(): Boolean = try {
        val base = BaseUrl.normalize(url.text.toString())
        val name = mode.text.toString().trim()
        require(name.matches(Regex("[a-z0-9-]{1,32}"))) { "Invalid mode name" }
        settings.save(base, token.text.toString(), name)
        url.setText(base)
        status.text = "Saved"
        true
    } catch (_: Exception) { status.text = "Use an HTTPS base URL and a mode of 1–32 lowercase letters, numbers or hyphens"; false }
    private fun test() {
        if (!save()) return
        val uploader = settings.uploader()
        val secret = settings.token
        status.text = "Testing…"
        worker.execute {
            try {
                val models = uploader.models()
                runOnUiThread {
                    mode.setAdapter(ArrayAdapter(this, android.R.layout.simple_dropdown_item_1line, models))
                    status.text = "OK — ${models.joinToString()}"
                }
            } catch (e: Exception) {
                val message = if (e is ApiFailure) e.message.orEmpty() else "Network error or invalid server response"
                runOnUiThread { status.text = message.replace(secret.takeIf { it.isNotEmpty() } ?: "\u0000", "[redacted]") }
            }
        }
    }
    private fun refreshPending() {
        val items = pending.all().filter { it.file.name != RecordingService.activeFileName }
        val names = items.map { it.file.name }
        if (shownClips == names) return
        shownClips = names
        clips.removeAllViews()
        pendingTitle.text = if (items.isEmpty()) "Pending recordings" else "Pending recordings · ${items.size}"
        if (items.isEmpty()) {
            clips.addView(TextView(this).apply {
                text = "No failed recordings waiting."
                setTextSize(TypedValue.COMPLEX_UNIT_SP, 14f)
                setTextColor(themeColor(android.R.attr.textColorSecondary))
            }, margins(top = 4))
        }
        val dates = DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT)
        items.forEach { clip ->
            val row = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL }
            row.addView(LinearLayout(this).apply {
                orientation = LinearLayout.VERTICAL
                addView(TextView(this@SettingsActivity).apply {
                    text = clip.mode
                    setTextSize(TypedValue.COMPLEX_UNIT_SP, 16f)
                    setTextColor(themeColor(android.R.attr.textColorPrimary))
                })
                addView(TextView(this@SettingsActivity).apply {
                    text = dates.format(java.util.Date(clip.file.lastModified()))
                    setTextSize(TypedValue.COMPLEX_UNIT_SP, 13f)
                    setTextColor(themeColor(android.R.attr.textColorSecondary))
                })
            }, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
            listOf("Retry" to RecordingService.RETRY, "Discard" to RecordingService.DISCARD).forEach { (label, command) ->
                row.addView(Button(this).apply {
                    text = label
                    isAllCaps = false
                    setOnClickListener {
                        if (RecordingService.currentState in listOf(Gesture.State.RECORDING, Gesture.State.PROCESSING)) {
                            status.text = "Busy"; return@setOnClickListener
                        }
                        if (save()) {
                            RecordingService.command(this@SettingsActivity, command, clip.file.name)
                            status.text = "$label requested; successful retries are available in History"
                        }
                    }
                }, LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT)
                    .apply { marginStart = dp(8) })
            }
            clips.addView(row, margins(top = 6))
        }
    }
    override fun onResume() { super.onResume(); if (::clips.isInitialized) main.post(poll) }
    override fun onPause() { main.removeCallbacks(poll); super.onPause() }
    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, results: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, results)
        status.text = permissions.zip(results.toList()).joinToString("\n") { (name, result) ->
            "${name.substringAfterLast('.')}: ${if (result == PackageManager.PERMISSION_GRANTED) "granted" else "denied — grant in App info"}"
        }
    }
    override fun onDestroy() { worker.shutdown(); super.onDestroy() }
}
