package cz.hajadlo.hark

import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.RadialGradient
import android.graphics.RectF
import android.graphics.Shader
import android.os.SystemClock
import android.view.View
import kotlin.math.min
import kotlin.math.sin

/**
 * The floating microphone: a 56 dp disc drawn on Canvas inside the 64 dp window, with a soft
 * shadow and a glyph per [Gesture.State]. Look only; touch handling stays in the service.
 */
class BubbleView(context: Context) : View(context) {
    var state: Gesture.State = Gesture.State.IDLE
        set(value) {
            if (field == value) return
            field = value
            phaseStart = SystemClock.uptimeMillis()
            contentDescription = when (value) {
                Gesture.State.IDLE -> "hark microphone. Tap to start recording."
                Gesture.State.RECORDING -> "Recording. Tap to stop."
                Gesture.State.PROCESSING -> "Processing the recording."
                Gesture.State.ERROR -> "Dictation failed."
            }
            invalidate()
        }
    private val density = resources.displayMetrics.density
    private val fill = Paint(Paint.ANTI_ALIAS_FLAG)
    private val glyph = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = Color.WHITE; strokeCap = Paint.Cap.ROUND }
    private val ring = Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.STROKE; strokeCap = Paint.Cap.ROUND }
    private val shadow = Paint(Paint.ANTI_ALIAS_FLAG)
    private val box = RectF()
    private var phaseStart = SystemClock.uptimeMillis()

    init { contentDescription = "hark microphone. Tap to start recording." }

    private fun dp(value: Float) = value * density

    override fun onDraw(canvas: Canvas) {
        val cx = width / 2f
        val cy = height / 2f
        val radius = min(width, height) / 2f - dp(4f)
        val elapsed = (SystemClock.uptimeMillis() - phaseStart).toFloat()

        // Soft shadow: a radial falloff below the disc, no software layer needed.
        val shadowRadius = radius + dp(5f)
        shadow.shader = RadialGradient(cx, cy + dp(2f), shadowRadius,
            intArrayOf(0x55000000, 0x22000000, 0x00000000), floatArrayOf(0.70f, 0.85f, 1f), Shader.TileMode.CLAMP)
        canvas.drawCircle(cx, cy + dp(2f), shadowRadius, shadow)

        when (state) {
            Gesture.State.IDLE -> {
                fill.color = 0xB83D5A80.toInt()   // calm blue-grey, slightly translucent over the keyboard
                canvas.drawCircle(cx, cy, radius, fill)
                glyph.alpha = 230
                drawMic(canvas, cx, cy)
            }
            Gesture.State.RECORDING -> {
                // A gentle pulse: the disc breathes and a ring drifts outward and fades, 1.4 s per cycle.
                val cycle = (elapsed % 1400f) / 1400f
                val breathe = 1f + 0.03f * sin(cycle * 2f * Math.PI).toFloat()
                ring.color = 0xFFE53935.toInt()
                ring.alpha = ((1f - cycle) * 110f).toInt()
                ring.strokeWidth = dp(3f)
                canvas.drawCircle(cx, cy, radius + dp(2f) + cycle * dp(8f), ring)
                fill.color = 0xFFD32F2F.toInt()
                canvas.drawCircle(cx, cy, radius * breathe, fill)
                glyph.alpha = 255
                drawMic(canvas, cx, cy)
                postInvalidateOnAnimation()
            }
            Gesture.State.PROCESSING -> {
                fill.color = 0xFF455A64.toInt()
                canvas.drawCircle(cx, cy, radius, fill)
                glyph.alpha = 110
                drawMic(canvas, cx, cy)
                // A spinner ring: a 270 degree arc turning once per 1.1 s.
                val inset = dp(5f)
                box.set(cx - radius + inset, cy - radius + inset, cx + radius - inset, cy + radius - inset)
                ring.color = Color.WHITE
                ring.alpha = 235
                ring.strokeWidth = dp(2.5f)
                canvas.drawArc(box, (elapsed % 1100f) / 1100f * 360f, 270f, false, ring)
                postInvalidateOnAnimation()
            }
            Gesture.State.ERROR -> {
                fill.color = 0xFF7B1FA2.toInt()   // purple: not the recording red, not the idle blue
                canvas.drawCircle(cx, cy, radius, fill)
                glyph.alpha = 255
                drawBang(canvas, cx, cy)
            }
        }
    }

    /** A microphone: capsule, U-shaped holder, stem and base, about 26 dp tall. */
    private fun drawMic(canvas: Canvas, cx: Float, cy: Float) {
        val top = cy - dp(13f)
        glyph.style = Paint.Style.FILL
        box.set(cx - dp(5f), top, cx + dp(5f), top + dp(16f))
        canvas.drawRoundRect(box, dp(5f), dp(5f), glyph)
        glyph.style = Paint.Style.STROKE
        glyph.strokeWidth = dp(2.5f)
        val holder = dp(9f)
        box.set(cx - holder, top + dp(8f) - holder, cx + holder, top + dp(8f) + holder)
        canvas.drawArc(box, 0f, 180f, false, glyph)
        canvas.drawLine(cx, top + dp(8f) + holder, cx, top + dp(23f), glyph)
        canvas.drawLine(cx - dp(5.5f), top + dp(24f), cx + dp(5.5f), top + dp(24f), glyph)
    }

    /** An exclamation mark for the error state. */
    private fun drawBang(canvas: Canvas, cx: Float, cy: Float) {
        glyph.style = Paint.Style.STROKE
        glyph.strokeWidth = dp(4f)
        canvas.drawLine(cx, cy - dp(11f), cx, cy + dp(3f), glyph)
        glyph.style = Paint.Style.FILL
        canvas.drawCircle(cx, cy + dp(10f), dp(2.4f), glyph)
    }
}
