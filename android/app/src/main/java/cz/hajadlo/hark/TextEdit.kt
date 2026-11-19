package cz.hajadlo.hark

/** Replace only a valid selection; never replace a whole draft because its cursor is unavailable. */
data class TextEdit(val text: String, val cursor: Int) {
    companion object {
        fun replace(old: String, start: Int, end: Int, transcript: String): TextEdit? {
            // Android can expose a focused, empty hint-only editor with no selection offsets.
            if (old.isEmpty() && start == -1 && end == -1) return TextEdit(transcript, transcript.length)
            if (start !in 0..old.length || end !in 0..old.length) return null
            val left = minOf(start, end)
            val right = maxOf(start, end)
            return TextEdit(old.substring(0, left) + transcript + old.substring(right), left + transcript.length)
        }
    }
}
