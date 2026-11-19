package cz.hajadlo.hark

import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.util.TypedValue
import android.view.View
import kotlin.math.log10

/** Recent microphone levels. A flat line means quiet input, rather than a fake listening animation. */
class MicLevelView(context: Context) : View(context) {
    private val history = FloatArray(32)
    private val paint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = TypedValue().also { context.theme.resolveAttribute(android.R.attr.colorAccent, it, true) }.data
        strokeCap = Paint.Cap.ROUND
    }
    var level = 0f
        private set
    var recording = false
        set(value) {
            field = value
            level = 0f
            history.fill(0f)
            contentDescription = if (value) "Live microphone level" else "Microphone inactive"
            invalidate()
        }
    init {
        tag = "microphone-meter"
        importantForAccessibility = IMPORTANT_FOR_ACCESSIBILITY_YES
        contentDescription = "Microphone inactive"
    }
    fun sample(rms: Float) {
        if (!recording) return
        level = if (rms.isFinite()) rms.coerceIn(0f, 1f) else 0f
        history.copyInto(history, 0, 1)
        // Show quiet speech as well as loud input; the bottom of the display is -60 dBFS.
        history[history.lastIndex] = if (level > 0f) ((20f * log10(level) + 60f) / 60f).coerceIn(0f, 1f) else 0f
        invalidate()
    }
    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        setMeasuredDimension(resolveSize((240 * resources.displayMetrics.density).toInt(), widthMeasureSpec),
            resolveSize((56 * resources.displayMetrics.density).toInt(), heightMeasureSpec))
    }
    override fun onDraw(canvas: Canvas) {
        super.onDraw(canvas)
        val step = width.toFloat() / history.size
        paint.strokeWidth = minOf(step * .55f, 5f * resources.displayMetrics.density)
        paint.alpha = if (recording) 255 else 70
        val middle = height / 2f
        history.forEachIndexed { index, amplitude ->
            val half = maxOf(resources.displayMetrics.density, amplitude * height * .44f)
            val x = step * (index + .5f)
            canvas.drawLine(x, middle - half, x, middle + half, paint)
        }
    }
}
