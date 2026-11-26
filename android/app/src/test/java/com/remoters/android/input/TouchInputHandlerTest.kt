package com.remoters.android.input

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class TouchInputHandlerTest {

    private sealed class RecordedEvent {
        data class Move(val x: Int, val y: Int) : RecordedEvent()
        data class Button(val x: Int, val y: Int, val button: Int, val down: Boolean) : RecordedEvent()
        data class Scroll(val x: Int, val y: Int, val vertical: Int, val horizontal: Int) : RecordedEvent()
        data class Key(val vk: Int, val down: Boolean) : RecordedEvent()
    }

    private class RecordingSink : InputSink {
        val events = mutableListOf<RecordedEvent>()

        override fun sendPointerMove(x: Int, y: Int) {
            events.add(RecordedEvent.Move(x, y))
        }

        override fun sendPointerButton(x: Int, y: Int, button: Int, down: Boolean) {
            events.add(RecordedEvent.Button(x, y, button, down))
        }

        override fun sendScroll(x: Int, y: Int, vertical: Int, horizontal: Int) {
            events.add(RecordedEvent.Scroll(x, y, vertical, horizontal))
        }

        override fun sendKey(vkCode: Int, down: Boolean) {
            events.add(RecordedEvent.Key(vkCode, down))
        }
    }

    @Test
    fun mapsNon16By9ViewAndFrameCoordinatesWithoutCropping() {
        val sink = RecordingSink()
        val handler = TouchInputHandler(sink, touchSlopPx = 10f)

        // Non-16:9 phone (2656x1220) displaying 16:9 (1920x1080) with horizontal pillarbox
        handler.updateViewSize(2656, 1220)
        handler.updateFrameSize(1920, 1080)
        val rect = computeFittedVideoRect(2656, 1220, 1920, 1080)
        assertEquals(1220f, rect.height, 0.5f)
        assertEquals(1920f * (1220f / 1080f), rect.width, 0.5f)
        assertTrue(rect.offsetX > 200f)
        assertEquals(0f, rect.offsetY, 0.5f)

        // Touch in left pillarbox clamps cleanly to x=0
        assertEquals(0 to 32768, handler.mapToHost(10f, 610f))
        // Touch at exact video center maps to (32768, 32768)
        val (cx, cy) = handler.mapToHost(1328f, 610f)
        assertTrue(cx in 32766..32769)
        assertTrue(cy in 32766..32769)
        // Touch in right pillarbox clamps cleanly to x=65535
        assertEquals(65535 to 65535, handler.mapToHost(2650f, 1220f))

        // Native non-16:9 stream (2656x1220) fills 2656x1220 edge-to-edge with zero offset
        handler.updateFrameSize(2656, 1220)
        val nativeRect = computeFittedVideoRect(2656, 1220, 2656, 1220)
        assertEquals(0f, nativeRect.offsetX, 0.01f)
        assertEquals(0f, nativeRect.offsetY, 0.01f)
        assertEquals(2656f, nativeRect.width, 0.01f)
        assertEquals(1220f, nativeRect.height, 0.01f)
        assertEquals(0 to 0, handler.mapToHost(0f, 0f))
        assertEquals(65535 to 65535, handler.mapToHost(2656f, 1220f))
    }

    @Test
    fun touchModeSupportsTapDragRightClickAndScroll() {
        val sink = RecordingSink()
        val handler = TouchInputHandler(sink, touchSlopPx = 10f)
        handler.updateViewSize(2000, 1000)
        handler.updateFrameSize(2000, 1000)

        // Single tap -> move + left click down/up
        handler.onPointerEvent(PointerAction.DOWN, 100L, 1000f, 500f)
        handler.onPointerEvent(PointerAction.UP, 150L, 1002f, 501f)
        assertEquals(3, sink.events.size)
        assertTrue(sink.events[0] is RecordedEvent.Move)
        assertEquals(
            RecordedEvent.Button(32833, 32833, TouchInputHandler.BUTTON_LEFT, true),
            sink.events[1]
        )
        assertEquals(
            RecordedEvent.Button(32833, 32833, TouchInputHandler.BUTTON_LEFT, false),
            sink.events[2]
        )

        // Single drag -> left down at start, move to target, left up on release
        sink.events.clear()
        handler.onPointerEvent(PointerAction.DOWN, 200L, 200f, 200f)
        handler.onPointerEvent(PointerAction.MOVE, 220L, 300f, 250f)
        handler.onPointerEvent(PointerAction.UP, 240L, 300f, 250f)
        assertTrue(sink.events.any { it is RecordedEvent.Button && it.button == TouchInputHandler.BUTTON_LEFT && it.down })
        assertTrue(sink.events.any { it is RecordedEvent.Button && it.button == TouchInputHandler.BUTTON_LEFT && !it.down })

        // Two-finger tap -> right click
        sink.events.clear()
        handler.onPointerEvent(PointerAction.DOWN, 300L, 500f, 500f)
        handler.onPointerEvent(PointerAction.POINTER_DOWN, 310L, 500f, 500f, pointerCount = 2, x1 = 700f, y1 = 500f)
        handler.onPointerEvent(PointerAction.POINTER_UP, 340L, 500f, 500f, pointerCount = 2, x1 = 700f, y1 = 500f)
        handler.onPointerEvent(PointerAction.UP, 360L, 500f, 500f, pointerCount = 1)
        val rightClicks = sink.events.filterIsInstance<RecordedEvent.Button>()
            .filter { it.button == TouchInputHandler.BUTTON_RIGHT }
        assertEquals(2, rightClicks.size)
        assertTrue(rightClicks[0].down)
        assertFalse(rightClicks[1].down)
    }

    @Test
    fun mouseModeMovesRelativeCursorWithoutClickingAndTapsLeftClick() {
        val sink = RecordingSink()
        val cursorUpdates = mutableListOf<Pair<Int, Int>>()
        val handler = TouchInputHandler(
            sink = sink,
            touchSlopPx = 10f,
            onCursorChanged = { x, y -> cursorUpdates.add(x to y) }
        )
        handler.updateViewSize(2656, 1220)
        handler.updateFrameSize(2656, 1220)

        handler.setInputMode(InputMode.MOUSE)
        assertEquals(InputMode.MOUSE, handler.inputMode)
        assertEquals(TouchInputHandler.CENTER_COORD, handler.cursorX)
        assertEquals(TouchInputHandler.CENTER_COORD, handler.cursorY)
        assertEquals(listOf(32767 to 32767), cursorUpdates)

        // 1-finger drag moves cursor relatively WITHOUT pressing any button
        sink.events.clear()
        handler.onPointerEvent(PointerAction.DOWN, 100L, 400f, 400f)
        handler.onPointerEvent(PointerAction.MOVE, 116L, 500f, 460f)
        handler.onPointerEvent(PointerAction.MOVE, 132L, 560f, 490f)
        handler.onPointerEvent(PointerAction.UP, 150L, 560f, 490f)

        assertTrue(sink.events.filterIsInstance<RecordedEvent.Button>().isEmpty())
        assertEquals(2, sink.events.filterIsInstance<RecordedEvent.Move>().size)
        assertTrue(handler.cursorX > TouchInputHandler.CENTER_COORD)
        assertTrue(handler.cursorY > TouchInputHandler.CENTER_COORD)

        val movedX = handler.cursorX
        val movedY = handler.cursorY

        // 1-finger tap anywhere on screen left-clicks at the virtual cursor position
        sink.events.clear()
        handler.onPointerEvent(PointerAction.DOWN, 500L, 100f, 100f)
        handler.onPointerEvent(PointerAction.UP, 540L, 102f, 101f)
        assertEquals(movedX, handler.cursorX)
        assertEquals(movedY, handler.cursorY)
        val buttons = sink.events.filterIsInstance<RecordedEvent.Button>()
        assertEquals(2, buttons.size)
        assertEquals(RecordedEvent.Button(movedX, movedY, TouchInputHandler.BUTTON_LEFT, true), buttons[0])
        assertEquals(RecordedEvent.Button(movedX, movedY, TouchInputHandler.BUTTON_LEFT, false), buttons[1])
    }

    @Test
    fun mouseModeDoubleTapAndDragHoldsLeftButtonUntilFingerUp() {
        val sink = RecordingSink()
        val handler = TouchInputHandler(sink, touchSlopPx = 10f)
        handler.updateViewSize(2000, 1000)
        handler.updateFrameSize(2000, 1000)
        handler.setInputMode(InputMode.MOUSE)
        sink.events.clear()

        // First tap
        handler.onPointerEvent(PointerAction.DOWN, 1000L, 500f, 500f)
        handler.onPointerEvent(PointerAction.UP, 1040L, 500f, 500f)
        sink.events.clear()

        val startX = handler.cursorX
        val startY = handler.cursorY

        // Second tap within DOUBLE_TAP_TIMEOUT_MS, then drag
        handler.onPointerEvent(PointerAction.DOWN, 1180L, 502f, 501f)
        handler.onPointerEvent(PointerAction.MOVE, 1210L, 560f, 540f)
        handler.onPointerEvent(PointerAction.MOVE, 1240L, 620f, 580f)
        handler.onPointerEvent(PointerAction.UP, 1270L, 620f, 580f)

        val buttons = sink.events.filterIsInstance<RecordedEvent.Button>()
        assertEquals(2, buttons.size)
        assertEquals(RecordedEvent.Button(startX, startY, TouchInputHandler.BUTTON_LEFT, true), buttons[0])
        assertEquals(
            RecordedEvent.Button(handler.cursorX, handler.cursorY, TouchInputHandler.BUTTON_LEFT, false),
            buttons[1]
        )
        assertTrue(handler.cursorX > startX)
        assertTrue(handler.cursorY > startY)
    }

    @Test
    fun mouseModeTwoFingerTapRightClicksAndTwoFingerDragScrollsAtCursor() {
        val sink = RecordingSink()
        val handler = TouchInputHandler(sink, touchSlopPx = 10f)
        handler.updateViewSize(2000, 1000)
        handler.updateFrameSize(2000, 1000)
        handler.setInputMode(InputMode.MOUSE)

        // Move cursor off-center first
        handler.onPointerEvent(PointerAction.DOWN, 100L, 500f, 500f)
        handler.onPointerEvent(PointerAction.MOVE, 120L, 600f, 420f)
        handler.onPointerEvent(PointerAction.UP, 140L, 600f, 420f)
        val cx = handler.cursorX
        val cy = handler.cursorY

        // Two-finger tap -> right click at (cx, cy)
        sink.events.clear()
        handler.onPointerEvent(PointerAction.DOWN, 500L, 300f, 300f)
        handler.onPointerEvent(PointerAction.POINTER_DOWN, 510L, 300f, 300f, pointerCount = 2, x1 = 420f, y1 = 300f)
        handler.onPointerEvent(PointerAction.POINTER_UP, 540L, 300f, 300f, pointerCount = 2, x1 = 420f, y1 = 300f)
        handler.onPointerEvent(PointerAction.UP, 560L, 300f, 300f, pointerCount = 1)
        val rightButtons = sink.events.filterIsInstance<RecordedEvent.Button>()
        assertEquals(2, rightButtons.size)
        assertEquals(RecordedEvent.Button(cx, cy, TouchInputHandler.BUTTON_RIGHT, true), rightButtons[0])
        assertEquals(RecordedEvent.Button(cx, cy, TouchInputHandler.BUTTON_RIGHT, false), rightButtons[1])

        // Two-finger drag -> vertical & horizontal scroll at (cx, cy)
        sink.events.clear()
        handler.onPointerEvent(PointerAction.DOWN, 700L, 400f, 400f)
        handler.onPointerEvent(PointerAction.POINTER_DOWN, 710L, 400f, 400f, pointerCount = 2, x1 = 500f, y1 = 400f)
        handler.onPointerEvent(PointerAction.MOVE, 730L, 430f, 450f, pointerCount = 2, x1 = 530f, y1 = 450f)
        handler.onPointerEvent(PointerAction.POINTER_UP, 760L, 430f, 450f, pointerCount = 2, x1 = 530f, y1 = 450f)
        handler.onPointerEvent(PointerAction.UP, 780L, 430f, 450f, pointerCount = 1)
        val scrolls = sink.events.filterIsInstance<RecordedEvent.Scroll>()
        assertEquals(1, scrolls.size)
        assertEquals(cx, scrolls[0].x)
        assertEquals(cy, scrolls[0].y)
        assertTrue(scrolls[0].vertical > 0)
        assertTrue(scrolls[0].horizontal < 0)
    }

    @Test
    fun mouseActionBarButtonsAndDragLockWorkAndCleanUpOnModeSwitch() {
        val sink = RecordingSink()
        val dragLockStates = mutableListOf<Boolean>()
        val handler = TouchInputHandler(
            sink = sink,
            touchSlopPx = 10f,
            onDragLockChanged = { dragLockStates.add(it) }
        )
        handler.updateViewSize(2000, 1000)
        handler.updateFrameSize(2000, 1000)
        handler.setInputMode(InputMode.MOUSE)
        sink.events.clear()

        // Middle click via action bar helper
        handler.clickButton(TouchInputHandler.BUTTON_MIDDLE)
        val midButtons = sink.events.filterIsInstance<RecordedEvent.Button>()
        assertEquals(2, midButtons.size)
        assertEquals(TouchInputHandler.BUTTON_MIDDLE, midButtons[0].button)
        assertTrue(midButtons[0].down)
        assertFalse(midButtons[1].down)

        // Enable drag lock -> presses left down, drag moves cursor while held
        sink.events.clear()
        handler.setDragLocked(true)
        assertTrue(handler.dragLocked)
        handler.onPointerEvent(PointerAction.DOWN, 100L, 200f, 200f)
        handler.onPointerEvent(PointerAction.MOVE, 120L, 300f, 220f)
        handler.onPointerEvent(PointerAction.UP, 140L, 300f, 220f)
        assertTrue(handler.dragLocked)
        val leftDowns = sink.events.filterIsInstance<RecordedEvent.Button>()
            .filter { it.button == TouchInputHandler.BUTTON_LEFT }
        assertEquals(1, leftDowns.size)
        assertTrue(leftDowns[0].down)

        // Switching back to TOUCH automatically releases drag-lock
        handler.setInputMode(InputMode.TOUCH)
        assertFalse(handler.dragLocked)
        assertEquals(listOf(true, false), dragLockStates)
        val finalButton = sink.events.filterIsInstance<RecordedEvent.Button>().last()
        assertEquals(TouchInputHandler.BUTTON_LEFT, finalButton.button)
        assertFalse(finalButton.down)
    }
}
