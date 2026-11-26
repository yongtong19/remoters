package com.remoters.android.input

import android.view.KeyEvent
import android.view.MotionEvent
import kotlin.math.abs
import kotlin.math.hypot
import kotlin.math.min
import kotlin.math.roundToInt

enum class InputMode {
    TOUCH,
    MOUSE;

    companion object {
        fun fromStoredName(name: String?): InputMode {
            return entries.firstOrNull { it.name == name } ?: TOUCH
        }
    }
}

enum class PointerAction {
    DOWN,
    POINTER_DOWN,
    MOVE,
    POINTER_UP,
    UP,
    CANCEL
}

data class FittedVideoRect(
    val offsetX: Float,
    val offsetY: Float,
    val width: Float,
    val height: Float
)

fun computeFittedVideoRect(
    viewWidth: Int,
    viewHeight: Int,
    frameWidth: Int,
    frameHeight: Int
): FittedVideoRect {
    val vw = viewWidth.toFloat().coerceAtLeast(1f)
    val vh = viewHeight.toFloat().coerceAtLeast(1f)
    val fw = (if (frameWidth > 0) frameWidth else 1920).toFloat()
    val fh = (if (frameHeight > 0) frameHeight else 1080).toFloat()
    val factor = min(vw / fw, vh / fh)
    val fittedW = (fw * factor).coerceAtLeast(1f)
    val fittedH = (fh * factor).coerceAtLeast(1f)
    val offsetX = (vw - fittedW) / 2f
    val offsetY = (vh - fittedH) / 2f
    return FittedVideoRect(
        offsetX = offsetX,
        offsetY = offsetY,
        width = fittedW,
        height = fittedH
    )
}

interface InputSink {
    fun sendPointerMove(x: Int, y: Int)
    fun sendPointerButton(x: Int, y: Int, button: Int, down: Boolean)
    fun sendScroll(x: Int, y: Int, vertical: Int, horizontal: Int)
    fun sendKey(vkCode: Int, down: Boolean)
}

class TouchInputHandler(
    private val sink: InputSink,
    private val touchSlopPx: Float = 16f,
    private val onCursorChanged: ((x: Int, y: Int) -> Unit)? = null,
    private val onDragLockChanged: ((locked: Boolean) -> Unit)? = null
) {
    private var viewWidth: Int = 1
    private var viewHeight: Int = 1
    private var frameWidth: Int = 1920
    private var frameHeight: Int = 1080

    var inputMode: InputMode = InputMode.TOUCH
        private set

    var cursorX: Int = CENTER_COORD
        private set
    var cursorY: Int = CENTER_COORD
        private set

    private var cursorXf: Float = CENTER_COORD.toFloat()
    private var cursorYf: Float = CENTER_COORD.toFloat()

    var dragLocked: Boolean = false
        private set

    private enum class GestureMode {
        NONE,
        SINGLE_PENDING,
        SINGLE_DRAG,
        MOUSE_SINGLE_PENDING,
        MOUSE_MOVE,
        MOUSE_DOUBLE_TAP_PENDING,
        MOUSE_DOUBLE_TAP_DRAG,
        TWO_FINGER_PENDING,
        TWO_FINGER_SCROLL
    }

    private var mode = GestureMode.NONE
    private var downX = 0f
    private var downY = 0f
    private var lastX = 0f
    private var lastY = 0f
    private var lastTwoFingerX = 0f
    private var lastTwoFingerY = 0f
    private var scrollAnchorX = CENTER_COORD
    private var scrollAnchorY = CENTER_COORD
    private var lastTapTimeMs = 0L
    private var lastTapX = 0f
    private var lastTapY = 0f

    fun setInputMode(newMode: InputMode) {
        if (inputMode == newMode) return
        cancelActiveGestures()
        if (newMode == InputMode.TOUCH && dragLocked) {
            setDragLocked(false)
        }
        inputMode = newMode
        if (newMode == InputMode.MOUSE) {
            sink.sendPointerMove(cursorX, cursorY)
            onCursorChanged?.invoke(cursorX, cursorY)
        }
    }

    fun setDragLocked(locked: Boolean) {
        if (dragLocked == locked) return
        dragLocked = locked
        sink.sendPointerMove(cursorX, cursorY)
        sink.sendPointerButton(cursorX, cursorY, BUTTON_LEFT, locked)
        onDragLockChanged?.invoke(locked)
    }

    fun clickButton(button: Int) {
        if (button == BUTTON_LEFT && dragLocked) {
            setDragLocked(false)
            return
        }
        sink.sendPointerMove(cursorX, cursorY)
        sink.sendPointerButton(cursorX, cursorY, button, true)
        sink.sendPointerButton(cursorX, cursorY, button, false)
    }

    fun cancelActiveGestures() {
        when (mode) {
            GestureMode.SINGLE_DRAG -> {
                val (hx, hy) = mapToHost(lastX, lastY)
                sink.sendPointerButton(hx, hy, BUTTON_LEFT, false)
            }
            GestureMode.MOUSE_DOUBLE_TAP_DRAG -> {
                if (!dragLocked) {
                    sink.sendPointerButton(cursorX, cursorY, BUTTON_LEFT, false)
                }
            }
            else -> Unit
        }
        mode = GestureMode.NONE
    }

    fun updateViewSize(width: Int, height: Int) {
        if (width > 0 && height > 0) {
            viewWidth = width
            viewHeight = height
        }
    }

    fun updateFrameSize(width: Int, height: Int) {
        if (width > 0 && height > 0) {
            frameWidth = width
            frameHeight = height
        }
    }

    fun mapToHost(viewX: Float, viewY: Float): Pair<Int, Int> {
        val rect = computeFittedVideoRect(viewWidth, viewHeight, frameWidth, frameHeight)
        val normX = ((viewX - rect.offsetX) / rect.width).coerceIn(0f, 1f)
        val normY = ((viewY - rect.offsetY) / rect.height).coerceIn(0f, 1f)
        return (normX * MAX_COORD).roundToInt().coerceIn(0, MAX_COORD) to
            (normY * MAX_COORD).roundToInt().coerceIn(0, MAX_COORD)
    }

    fun onTouchEvent(event: MotionEvent): Boolean {
        val action = when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> PointerAction.DOWN
            MotionEvent.ACTION_POINTER_DOWN -> PointerAction.POINTER_DOWN
            MotionEvent.ACTION_MOVE -> PointerAction.MOVE
            MotionEvent.ACTION_POINTER_UP -> PointerAction.POINTER_UP
            MotionEvent.ACTION_UP -> PointerAction.UP
            MotionEvent.ACTION_CANCEL -> PointerAction.CANCEL
            else -> return false
        }
        val count = event.pointerCount.coerceAtLeast(1)
        val x0 = event.getX(0)
        val y0 = event.getY(0)
        val x1 = if (count >= 2) event.getX(1) else 0f
        val y1 = if (count >= 2) event.getY(1) else 0f
        return onPointerEvent(
            action = action,
            eventTimeMs = event.eventTime,
            x0 = x0,
            y0 = y0,
            pointerCount = count,
            x1 = x1,
            y1 = y1
        )
    }

    fun onPointerEvent(
        action: PointerAction,
        eventTimeMs: Long,
        x0: Float,
        y0: Float,
        pointerCount: Int = 1,
        x1: Float = 0f,
        y1: Float = 0f
    ): Boolean {
        return when (inputMode) {
            InputMode.TOUCH -> handleTouchModeEvent(action, x0, y0, pointerCount, x1, y1)
            InputMode.MOUSE -> handleMouseModeEvent(action, eventTimeMs, x0, y0, pointerCount, x1, y1)
        }
    }

    private fun handleTouchModeEvent(
        action: PointerAction,
        x0: Float,
        y0: Float,
        pointerCount: Int,
        x1: Float,
        y1: Float
    ): Boolean {
        when (action) {
            PointerAction.DOWN -> {
                downX = x0
                downY = y0
                lastX = x0
                lastY = y0
                mode = GestureMode.SINGLE_PENDING
                val (hx, hy) = mapToHost(downX, downY)
                sink.sendPointerMove(hx, hy)
                return true
            }

            PointerAction.POINTER_DOWN -> {
                if (pointerCount == 2 && mode == GestureMode.SINGLE_PENDING) {
                    val midX = (x0 + x1) / 2f
                    val midY = (y0 + y1) / 2f
                    lastTwoFingerX = midX
                    lastTwoFingerY = midY
                    val (hx, hy) = mapToHost(midX, midY)
                    scrollAnchorX = hx
                    scrollAnchorY = hy
                    mode = GestureMode.TWO_FINGER_PENDING
                }
                return true
            }

            PointerAction.MOVE -> {
                lastX = x0
                lastY = y0
                when (mode) {
                    GestureMode.SINGLE_PENDING -> {
                        val dx = x0 - downX
                        val dy = y0 - downY
                        if (dx * dx + dy * dy > touchSlopPx * touchSlopPx) {
                            val (startX, startY) = mapToHost(downX, downY)
                            sink.sendPointerButton(startX, startY, BUTTON_LEFT, true)
                            val (hx, hy) = mapToHost(x0, y0)
                            sink.sendPointerMove(hx, hy)
                            mode = GestureMode.SINGLE_DRAG
                        }
                    }

                    GestureMode.SINGLE_DRAG -> {
                        val (hx, hy) = mapToHost(x0, y0)
                        sink.sendPointerMove(hx, hy)
                    }

                    GestureMode.TWO_FINGER_PENDING -> {
                        if (pointerCount >= 2) {
                            val midX = (x0 + x1) / 2f
                            val midY = (y0 + y1) / 2f
                            val dx = midX - lastTwoFingerX
                            val dy = midY - lastTwoFingerY
                            if (dx * dx + dy * dy > touchSlopPx * touchSlopPx) {
                                mode = GestureMode.TWO_FINGER_SCROLL
                                sendScrollDelta(dx, dy, scrollAnchorX, scrollAnchorY)
                                lastTwoFingerX = midX
                                lastTwoFingerY = midY
                            }
                        }
                    }

                    GestureMode.TWO_FINGER_SCROLL -> {
                        if (pointerCount >= 2) {
                            val midX = (x0 + x1) / 2f
                            val midY = (y0 + y1) / 2f
                            val dx = midX - lastTwoFingerX
                            val dy = midY - lastTwoFingerY
                            sendScrollDelta(dx, dy, scrollAnchorX, scrollAnchorY)
                            lastTwoFingerX = midX
                            lastTwoFingerY = midY
                        }
                    }

                    else -> Unit
                }
                return true
            }

            PointerAction.POINTER_UP -> {
                if (mode == GestureMode.TWO_FINGER_PENDING && pointerCount == 2) {
                    sink.sendPointerButton(scrollAnchorX, scrollAnchorY, BUTTON_RIGHT, true)
                    sink.sendPointerButton(scrollAnchorX, scrollAnchorY, BUTTON_RIGHT, false)
                    mode = GestureMode.NONE
                } else if (mode == GestureMode.TWO_FINGER_SCROLL && pointerCount <= 2) {
                    mode = GestureMode.NONE
                }
                return true
            }

            PointerAction.UP -> {
                when (mode) {
                    GestureMode.SINGLE_PENDING -> {
                        val (hx, hy) = mapToHost(x0, y0)
                        sink.sendPointerButton(hx, hy, BUTTON_LEFT, true)
                        sink.sendPointerButton(hx, hy, BUTTON_LEFT, false)
                    }

                    GestureMode.SINGLE_DRAG -> {
                        val (hx, hy) = mapToHost(x0, y0)
                        sink.sendPointerButton(hx, hy, BUTTON_LEFT, false)
                    }

                    else -> Unit
                }
                mode = GestureMode.NONE
                return true
            }

            PointerAction.CANCEL -> {
                if (mode == GestureMode.SINGLE_DRAG) {
                    val (hx, hy) = mapToHost(x0, y0)
                    sink.sendPointerButton(hx, hy, BUTTON_LEFT, false)
                }
                mode = GestureMode.NONE
                return true
            }
        }
    }

    private fun handleMouseModeEvent(
        action: PointerAction,
        eventTimeMs: Long,
        x0: Float,
        y0: Float,
        pointerCount: Int,
        x1: Float,
        y1: Float
    ): Boolean {
        when (action) {
            PointerAction.DOWN -> {
                downX = x0
                downY = y0
                lastX = x0
                lastY = y0
                val isDoubleTapCandidate = lastTapTimeMs > 0L &&
                    (eventTimeMs - lastTapTimeMs) in 0L..DOUBLE_TAP_TIMEOUT_MS &&
                    hypot(x0 - lastTapX, y0 - lastTapY) <= touchSlopPx * DOUBLE_TAP_SLOP_MULTIPLIER
                mode = if (isDoubleTapCandidate) {
                    GestureMode.MOUSE_DOUBLE_TAP_PENDING
                } else {
                    GestureMode.MOUSE_SINGLE_PENDING
                }
                return true
            }

            PointerAction.POINTER_DOWN -> {
                if (pointerCount == 2 &&
                    (mode == GestureMode.MOUSE_SINGLE_PENDING || mode == GestureMode.MOUSE_DOUBLE_TAP_PENDING)
                ) {
                    lastTwoFingerX = (x0 + x1) / 2f
                    lastTwoFingerY = (y0 + y1) / 2f
                    scrollAnchorX = cursorX
                    scrollAnchorY = cursorY
                    mode = GestureMode.TWO_FINGER_PENDING
                }
                return true
            }

            PointerAction.MOVE -> {
                when (mode) {
                    GestureMode.MOUSE_SINGLE_PENDING -> {
                        val totalDx = x0 - downX
                        val totalDy = y0 - downY
                        if (totalDx * totalDx + totalDy * totalDy > touchSlopPx * touchSlopPx) {
                            mode = GestureMode.MOUSE_MOVE
                            applyRelativeMouseMove(totalDx, totalDy)
                            lastX = x0
                            lastY = y0
                        }
                    }

                    GestureMode.MOUSE_MOVE -> {
                        val dx = x0 - lastX
                        val dy = y0 - lastY
                        applyRelativeMouseMove(dx, dy)
                        lastX = x0
                        lastY = y0
                    }

                    GestureMode.MOUSE_DOUBLE_TAP_PENDING -> {
                        val totalDx = x0 - downX
                        val totalDy = y0 - downY
                        if (totalDx * totalDx + totalDy * totalDy > touchSlopPx * touchSlopPx) {
                            mode = GestureMode.MOUSE_DOUBLE_TAP_DRAG
                            if (!dragLocked) {
                                sink.sendPointerMove(cursorX, cursorY)
                                sink.sendPointerButton(cursorX, cursorY, BUTTON_LEFT, true)
                            }
                            applyRelativeMouseMove(totalDx, totalDy)
                            lastX = x0
                            lastY = y0
                        }
                    }

                    GestureMode.MOUSE_DOUBLE_TAP_DRAG -> {
                        val dx = x0 - lastX
                        val dy = y0 - lastY
                        applyRelativeMouseMove(dx, dy)
                        lastX = x0
                        lastY = y0
                    }

                    GestureMode.TWO_FINGER_PENDING -> {
                        if (pointerCount >= 2) {
                            val midX = (x0 + x1) / 2f
                            val midY = (y0 + y1) / 2f
                            val dx = midX - lastTwoFingerX
                            val dy = midY - lastTwoFingerY
                            if (dx * dx + dy * dy > touchSlopPx * touchSlopPx) {
                                mode = GestureMode.TWO_FINGER_SCROLL
                                sendScrollDelta(dx, dy, cursorX, cursorY)
                                lastTwoFingerX = midX
                                lastTwoFingerY = midY
                            }
                        }
                    }

                    GestureMode.TWO_FINGER_SCROLL -> {
                        if (pointerCount >= 2) {
                            val midX = (x0 + x1) / 2f
                            val midY = (y0 + y1) / 2f
                            val dx = midX - lastTwoFingerX
                            val dy = midY - lastTwoFingerY
                            sendScrollDelta(dx, dy, cursorX, cursorY)
                            lastTwoFingerX = midX
                            lastTwoFingerY = midY
                        }
                    }

                    else -> Unit
                }
                return true
            }

            PointerAction.POINTER_UP -> {
                if (mode == GestureMode.TWO_FINGER_PENDING && pointerCount == 2) {
                    sink.sendPointerMove(cursorX, cursorY)
                    sink.sendPointerButton(cursorX, cursorY, BUTTON_RIGHT, true)
                    sink.sendPointerButton(cursorX, cursorY, BUTTON_RIGHT, false)
                    lastTapTimeMs = 0L
                    mode = GestureMode.NONE
                } else if (mode == GestureMode.TWO_FINGER_SCROLL && pointerCount <= 2) {
                    lastTapTimeMs = 0L
                    mode = GestureMode.NONE
                }
                return true
            }

            PointerAction.UP -> {
                when (mode) {
                    GestureMode.MOUSE_SINGLE_PENDING,
                    GestureMode.MOUSE_DOUBLE_TAP_PENDING -> {
                        if (dragLocked) {
                            setDragLocked(false)
                        } else {
                            sink.sendPointerMove(cursorX, cursorY)
                            sink.sendPointerButton(cursorX, cursorY, BUTTON_LEFT, true)
                            sink.sendPointerButton(cursorX, cursorY, BUTTON_LEFT, false)
                        }
                        lastTapTimeMs = eventTimeMs
                        lastTapX = x0
                        lastTapY = y0
                    }

                    GestureMode.MOUSE_DOUBLE_TAP_DRAG -> {
                        if (!dragLocked) {
                            sink.sendPointerButton(cursorX, cursorY, BUTTON_LEFT, false)
                        }
                        lastTapTimeMs = 0L
                    }

                    GestureMode.MOUSE_MOVE -> {
                        lastTapTimeMs = 0L
                    }

                    else -> Unit
                }
                mode = GestureMode.NONE
                return true
            }

            PointerAction.CANCEL -> {
                if (mode == GestureMode.MOUSE_DOUBLE_TAP_DRAG && !dragLocked) {
                    sink.sendPointerButton(cursorX, cursorY, BUTTON_LEFT, false)
                }
                mode = GestureMode.NONE
                return true
            }
        }
    }

    private fun applyRelativeMouseMove(dxPx: Float, dyPx: Float) {
        if (dxPx == 0f && dyPx == 0f) return
        val rect = computeFittedVideoRect(viewWidth, viewHeight, frameWidth, frameHeight)
        val distPx = hypot(dxPx, dyPx)
        val accel = 1.0f + (distPx / ACCEL_DIVISOR_PX).coerceAtMost(MAX_ACCEL_BONUS)
        val scale = MOUSE_SENSITIVITY * accel
        val normDx = (dxPx / rect.width) * scale * MAX_COORD
        val normDy = (dyPx / rect.height) * scale * MAX_COORD
        cursorXf = (cursorXf + normDx).coerceIn(0f, MAX_COORD.toFloat())
        cursorYf = (cursorYf + normDy).coerceIn(0f, MAX_COORD.toFloat())
        val nextX = cursorXf.roundToInt().coerceIn(0, MAX_COORD)
        val nextY = cursorYf.roundToInt().coerceIn(0, MAX_COORD)
        cursorX = nextX
        cursorY = nextY
        sink.sendPointerMove(cursorX, cursorY)
        onCursorChanged?.invoke(cursorX, cursorY)
    }

    private fun sendScrollDelta(dxPx: Float, dyPx: Float, anchorX: Int, anchorY: Int) {
        val vertical = (dyPx * 2.5f).roundToInt().coerceIn(-1200, 1200)
        val horizontal = (-dxPx * 2.5f).roundToInt().coerceIn(-1200, 1200)
        if (abs(vertical) > 0 || abs(horizontal) > 0) {
            sink.sendScroll(anchorX, anchorY, vertical, horizontal)
        }
    }

    @Suppress("DEPRECATION")
    fun handleKeyEvent(event: KeyEvent): Boolean {
        if (GamepadHandler.isGamepadEventSource(event.source)) return false
        if (event.action == KeyEvent.ACTION_MULTIPLE) {
            val chars = event.characters ?: return false
            for (ch in chars) {
                sendTypedChar(ch)
            }
            return true
        }
        if (event.action != KeyEvent.ACTION_DOWN && event.action != KeyEvent.ACTION_UP) {
            return false
        }
        val down = event.action == KeyEvent.ACTION_DOWN
        val vk = mapAndroidKeyCodeToVk(event.keyCode)
        if (vk != null) {
            val needsShift = event.isShiftPressed && event.keyCode !in SHIFT_KEYCODES
            if (down && needsShift) sink.sendKey(VK_LSHIFT, true)
            sink.sendKey(vk, down)
            if (!down && needsShift) sink.sendKey(VK_LSHIFT, false)
            return true
        }
        if (down && event.unicodeChar != 0) {
            sendTypedChar(event.unicodeChar.toChar())
            return true
        }
        return false
    }

    fun sendTypedText(text: CharSequence) {
        for (ch in text) {
            sendTypedChar(ch)
        }
    }

    fun sendTypedChar(ch: Char) {
        if (ch == '\n') {
            sink.sendKey(0x0D, true)
            sink.sendKey(0x0D, false)
            return
        }
        val (vk, shift) = charToVk(ch) ?: return
        if (shift) sink.sendKey(VK_LSHIFT, true)
        sink.sendKey(vk, true)
        sink.sendKey(vk, false)
        if (shift) sink.sendKey(VK_LSHIFT, false)
    }

    companion object {
        const val BUTTON_LEFT = 0
        const val BUTTON_RIGHT = 1
        const val BUTTON_MIDDLE = 2
        const val MAX_COORD = 65535
        const val CENTER_COORD = 32767
        const val DOUBLE_TAP_TIMEOUT_MS = 280L
        private const val DOUBLE_TAP_SLOP_MULTIPLIER = 4f
        private const val MOUSE_SENSITIVITY = 1.35f
        private const val ACCEL_DIVISOR_PX = 48f
        private const val MAX_ACCEL_BONUS = 0.75f
        private const val VK_LSHIFT = 0xA0

        private val SHIFT_KEYCODES = setOf(
            KeyEvent.KEYCODE_SHIFT_LEFT,
            KeyEvent.KEYCODE_SHIFT_RIGHT
        )

        fun mapAndroidKeyCodeToVk(keyCode: Int): Int? {
            if (keyCode in KeyEvent.KEYCODE_A..KeyEvent.KEYCODE_Z) {
                return 0x41 + (keyCode - KeyEvent.KEYCODE_A)
            }
            if (keyCode in KeyEvent.KEYCODE_0..KeyEvent.KEYCODE_9) {
                return 0x30 + (keyCode - KeyEvent.KEYCODE_0)
            }
            if (keyCode in KeyEvent.KEYCODE_F1..KeyEvent.KEYCODE_F12) {
                return 0x70 + (keyCode - KeyEvent.KEYCODE_F1)
            }
            return when (keyCode) {
                KeyEvent.KEYCODE_DEL -> 0x08
                KeyEvent.KEYCODE_TAB -> 0x09
                KeyEvent.KEYCODE_ENTER, KeyEvent.KEYCODE_NUMPAD_ENTER -> 0x0D
                KeyEvent.KEYCODE_SHIFT_LEFT -> 0xA0
                KeyEvent.KEYCODE_SHIFT_RIGHT -> 0xA1
                KeyEvent.KEYCODE_CTRL_LEFT -> 0xA2
                KeyEvent.KEYCODE_CTRL_RIGHT -> 0xA3
                KeyEvent.KEYCODE_ALT_LEFT -> 0xA4
                KeyEvent.KEYCODE_ALT_RIGHT -> 0xA5
                KeyEvent.KEYCODE_CAPS_LOCK -> 0x14
                KeyEvent.KEYCODE_ESCAPE -> 0x1B
                KeyEvent.KEYCODE_SPACE -> 0x20
                KeyEvent.KEYCODE_PAGE_UP -> 0x21
                KeyEvent.KEYCODE_PAGE_DOWN -> 0x22
                KeyEvent.KEYCODE_MOVE_END -> 0x23
                KeyEvent.KEYCODE_MOVE_HOME -> 0x24
                KeyEvent.KEYCODE_DPAD_LEFT -> 0x25
                KeyEvent.KEYCODE_DPAD_UP -> 0x26
                KeyEvent.KEYCODE_DPAD_RIGHT -> 0x27
                KeyEvent.KEYCODE_DPAD_DOWN -> 0x28
                KeyEvent.KEYCODE_INSERT -> 0x2D
                KeyEvent.KEYCODE_FORWARD_DEL -> 0x2E
                KeyEvent.KEYCODE_META_LEFT -> 0x5B
                KeyEvent.KEYCODE_META_RIGHT -> 0x5C
                KeyEvent.KEYCODE_SEMICOLON -> 0xBA
                KeyEvent.KEYCODE_EQUALS -> 0xBB
                KeyEvent.KEYCODE_COMMA -> 0xBC
                KeyEvent.KEYCODE_MINUS -> 0xBD
                KeyEvent.KEYCODE_PERIOD -> 0xBE
                KeyEvent.KEYCODE_SLASH -> 0xBF
                KeyEvent.KEYCODE_GRAVE -> 0xC0
                KeyEvent.KEYCODE_LEFT_BRACKET -> 0xDB
                KeyEvent.KEYCODE_BACKSLASH -> 0xDC
                KeyEvent.KEYCODE_RIGHT_BRACKET -> 0xDD
                KeyEvent.KEYCODE_APOSTROPHE -> 0xDE
                else -> null
            }
        }

        private fun charToVk(ch: Char): Pair<Int, Boolean>? = when (ch) {
            in 'a'..'z' -> (0x41 + (ch - 'a')) to false
            in 'A'..'Z' -> (0x41 + (ch - 'A')) to true
            in '0'..'9' -> (0x30 + (ch - '0')) to false
            ' ' -> 0x20 to false
            '\t' -> 0x09 to false
            ';' -> 0xBA to false
            ':' -> 0xBA to true
            '=' -> 0xBB to false
            '+' -> 0xBB to true
            ',' -> 0xBC to false
            '<' -> 0xBC to true
            '-' -> 0xBD to false
            '_' -> 0xBD to true
            '.' -> 0xBE to false
            '>' -> 0xBE to true
            '/' -> 0xBF to false
            '?' -> 0xBF to true
            '`' -> 0xC0 to false
            '~' -> 0xC0 to true
            '[' -> 0xDB to false
            '{' -> 0xDB to true
            '\\' -> 0xDC to false
            '|' -> 0xDC to true
            ']' -> 0xDD to false
            '}' -> 0xDD to true
            '\'' -> 0xDE to false
            '"' -> 0xDE to true
            '!' -> 0x31 to true
            '@' -> 0x32 to true
            '#' -> 0x33 to true
            '$' -> 0x34 to true
            '%' -> 0x35 to true
            '^' -> 0x36 to true
            '&' -> 0x37 to true
            '*' -> 0x38 to true
            '(' -> 0x39 to true
            ')' -> 0x30 to true
            else -> null
        }
    }
}
