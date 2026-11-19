package cz.hajadlo.hark

import org.junit.Assert.*
import org.junit.Test

class TextEditTest {
    @Test fun insertsAtCursorWithoutLosingSurroundingDraft() {
        assertEquals(TextEdit("before dictated after", 15), TextEdit.replace("before  after", 7, 7, "dictated"))
    }
    @Test fun replacesASelectionInEitherDirection() {
        assertEquals(TextEdit("one new three", 7), TextEdit.replace("one old three", 4, 7, "new"))
        assertEquals(TextEdit("one new three", 7), TextEdit.replace("one old three", 7, 4, "new"))
    }
    @Test fun rejectsMissingOrStaleSelectionInsteadOfOverwritingDraft() {
        assertNull(TextEdit.replace("draft", -1, -1, "dictated"))
        assertNull(TextEdit.replace("draft", 0, 99, "dictated"))
    }
    @Test fun emptyPlaceholderEditorWithoutReportedSelectionAcceptsDictation() {
        assertEquals(TextEdit("dictated", 8), TextEdit.replace("", -1, -1, "dictated"))
        assertNull(TextEdit.replace("", -1, 2, "dictated"))
    }
    @Test fun handlesEmptyEditorAndUnicodeCursorOffsets() {
        assertEquals(TextEdit("Příliš žluťoučký", 16), TextEdit.replace("", 0, 0, "Příliš žluťoučký"))
        assertEquals(TextEdit("🙂 word", 7), TextEdit.replace("🙂 ", 3, 3, "word"))
    }
}
