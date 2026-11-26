package com.remoters.android.input

import kotlin.math.abs
import kotlin.math.roundToInt

/**
 * Pure Kotlin Android KeyEvent / MotionEvent -> XInput gamepad mapper.
 * Zero android.* imports so all mapping rules are directly JVM unit-testable.
 */
object XInputButtons {
    const val DPAD_UP = 0x0001
    const val DPAD_DOWN = 0x0002
    const val DPAD_LEFT = 0x0004
    const val DPAD_RIGHT = 0x0008
    const val START = 0x0010
    const val BACK = 0x0020
    const val LTHUMB = 0x0040
    const val RTHUMB = 0x0080
    const val LB = 0x0100
    const val RB = 0x0200
    const val GUIDE = 0x0400
    const val A = 0x1000
    const val B = 0x2000
    const val X = 0x4000
    const val Y = 0x8000
}

object AndroidGamepadKeyCodes {
    const val DPAD_UP = 19
    const val DPAD_DOWN = 20
    const val DPAD_LEFT = 21
    const val DPAD_RIGHT = 22
    const val BUTTON_A = 96
    const val BUTTON_B = 97
    const val BUTTON_C = 98
    const val BUTTON_X = 99
    const val BUTTON_Y = 100
    const val BUTTON_Z = 101
    const val BUTTON_L1 = 102
    const val BUTTON_R1 = 103
    const val BUTTON_L2 = 104
    const val BUTTON_R2 = 105
    const val BUTTON_THUMBL = 106
    const val BUTTON_THUMBR = 107
    const val BUTTON_START = 108
    const val BUTTON_SELECT = 109
    const val BUTTON_MODE = 110
}

object AndroidMotionAxes {
    const val AXIS_X = 0
    const val AXIS_Y = 1
    const val AXIS_Z = 11
    const val AXIS_RX = 12
    const val AXIS_RY = 13
    const val AXIS_RZ = 14
    const val AXIS_HAT_X = 15
    const val AXIS_HAT_Y = 16
    const val AXIS_LTRIGGER = 17
    const val AXIS_RTRIGGER = 18
    const val AXIS_GAS = 22
    const val AXIS_BRAKE = 23
}

data class GamepadState(
    val slot: Int = 0,
    val connected: Boolean = true,
    val buttons: Int = 0,
    val lt: Int = 0,
    val rt: Int = 0,
    val lx: Int = 0,
    val ly: Int = 0,
    val rx: Int = 0,
    val ry: Int = 0
)

data class AxisCapabilities(
    val hasZ: Boolean = true,
    val hasRz: Boolean = true,
    val hasRx: Boolean = false,
    val hasRy: Boolean = false,
    val hasLTrigger: Boolean = true,
    val hasRTrigger: Boolean = true,
    val hasBrake: Boolean = false,
    val hasGas: Boolean = false
)

data class MotionSample(
    val x: Float = 0f,
    val y: Float = 0f,
    val z: Float = 0f,
    val rz: Float = 0f,
    val rx: Float = 0f,
    val ry: Float = 0f,
    val lTrigger: Float = 0f,
    val rTrigger: Float = 0f,
    val brake: Float = 0f,
    val gas: Float = 0f,
    val hatX: Float = 0f,
    val hatY: Float = 0f
)

class ControllerStateTracker(
    val slot: Int,
    private val stickDeadzone: Float = DEFAULT_STICK_DEADZONE,
    private val triggerDeadzone: Float = DEFAULT_TRIGGER_DEADZONE
) {
    private var digitalButtons: Int = 0
    private var hatDpadButtons: Int = 0
    private var digitalL2: Boolean = false
    private var digitalR2: Boolean = false
    private var lt: Int = 0
    private var rt: Int = 0
    private var lx: Int = 0
    private var ly: Int = 0
    private var rx: Int = 0
    private var ry: Int = 0

    fun onKey(keyCode: Int, down: Boolean): GamepadState? {
        var handled = true
        when (keyCode) {
            AndroidGamepadKeyCodes.BUTTON_L2 -> digitalL2 = down
            AndroidGamepadKeyCodes.BUTTON_R2 -> digitalR2 = down
            else -> {
                val mask = GamepadMapper.mapKeyCodeToMask(keyCode)
                if (mask != 0) {
                    digitalButtons = if (down) {
                        digitalButtons or mask
                    } else {
                        digitalButtons and mask.inv()
                    }
                } else {
                    handled = false
                }
            }
        }
        return if (handled) snapshot() else null
    }

    fun onMotion(sample: MotionSample, caps: AxisCapabilities = AxisCapabilities()): GamepadState {
        lx = GamepadMapper.normalizeStickAxis(sample.x, invert = false, deadzone = stickDeadzone)
        ly = GamepadMapper.normalizeStickAxis(sample.y, invert = true, deadzone = stickDeadzone)

        val (rawRx, rawRy) = GamepadMapper.selectRightStickAxes(sample, caps)
        rx = GamepadMapper.normalizeStickAxis(rawRx, invert = false, deadzone = stickDeadzone)
        ry = GamepadMapper.normalizeStickAxis(rawRy, invert = true, deadzone = stickDeadzone)

        val (rawLt, rawRt) = GamepadMapper.selectTriggerAxes(sample, caps)
        lt = GamepadMapper.normalizeTrigger(rawLt, triggerDeadzone)
        rt = GamepadMapper.normalizeTrigger(rawRt, triggerDeadzone)

        hatDpadButtons = GamepadMapper.mapHatToDpadMask(sample.hatX, sample.hatY)
        return snapshot()
    }

    fun snapshot(): GamepadState {
        val effectiveLt = if (lt > 0) lt else if (digitalL2) 255 else 0
        val effectiveRt = if (rt > 0) rt else if (digitalR2) 255 else 0
        return GamepadState(
            slot = slot,
            connected = true,
            buttons = digitalButtons or hatDpadButtons,
            lt = effectiveLt,
            rt = effectiveRt,
            lx = lx,
            ly = ly,
            rx = rx,
            ry = ry
        )
    }

    companion object {
        const val DEFAULT_STICK_DEADZONE = 0.08f
        const val DEFAULT_TRIGGER_DEADZONE = 0.02f
    }
}

object GamepadMapper {
    fun mapKeyCodeToMask(keyCode: Int): Int = when (keyCode) {
        AndroidGamepadKeyCodes.BUTTON_A -> XInputButtons.A
        AndroidGamepadKeyCodes.BUTTON_B -> XInputButtons.B
        AndroidGamepadKeyCodes.BUTTON_X -> XInputButtons.X
        AndroidGamepadKeyCodes.BUTTON_Y -> XInputButtons.Y
        AndroidGamepadKeyCodes.BUTTON_L1 -> XInputButtons.LB
        AndroidGamepadKeyCodes.BUTTON_R1 -> XInputButtons.RB
        AndroidGamepadKeyCodes.BUTTON_THUMBL -> XInputButtons.LTHUMB
        AndroidGamepadKeyCodes.BUTTON_THUMBR -> XInputButtons.RTHUMB
        AndroidGamepadKeyCodes.BUTTON_START -> XInputButtons.START
        AndroidGamepadKeyCodes.BUTTON_SELECT -> XInputButtons.BACK
        AndroidGamepadKeyCodes.BUTTON_MODE -> XInputButtons.GUIDE
        AndroidGamepadKeyCodes.DPAD_UP -> XInputButtons.DPAD_UP
        AndroidGamepadKeyCodes.DPAD_DOWN -> XInputButtons.DPAD_DOWN
        AndroidGamepadKeyCodes.DPAD_LEFT -> XInputButtons.DPAD_LEFT
        AndroidGamepadKeyCodes.DPAD_RIGHT -> XInputButtons.DPAD_RIGHT
        else -> 0
    }

    fun mapHatToDpadMask(hatX: Float, hatY: Float, threshold: Float = 0.5f): Int {
        var mask = 0
        if (hatX <= -threshold) mask = mask or XInputButtons.DPAD_LEFT
        if (hatX >= threshold) mask = mask or XInputButtons.DPAD_RIGHT
        if (hatY <= -threshold) mask = mask or XInputButtons.DPAD_UP
        if (hatY >= threshold) mask = mask or XInputButtons.DPAD_DOWN
        return mask
    }

    fun normalizeStickAxis(
        value: Float,
        invert: Boolean,
        deadzone: Float = ControllerStateTracker.DEFAULT_STICK_DEADZONE
    ): Int {
        val clamped = value.coerceIn(-1f, 1f)
        if (abs(clamped) < deadzone) return 0
        val rescaled = ((abs(clamped) - deadzone) / (1f - deadzone)).coerceIn(0f, 1f)
        val signed = if (clamped < 0f) -rescaled else rescaled
        val directed = if (invert) -signed else signed
        return (directed * 32767f).roundToInt().coerceIn(-32768, 32767)
    }

    fun normalizeTrigger(
        value: Float,
        deadzone: Float = ControllerStateTracker.DEFAULT_TRIGGER_DEADZONE
    ): Int {
        val clamped = value.coerceIn(0f, 1f)
        if (clamped < deadzone) return 0
        return (clamped * 255f).roundToInt().coerceIn(0, 255)
    }

    fun selectRightStickAxes(sample: MotionSample, caps: AxisCapabilities): Pair<Float, Float> {
        val rightX = when {
            caps.hasZ -> sample.z
            caps.hasRx -> sample.rx
            else -> if (abs(sample.z) >= abs(sample.rx)) sample.z else sample.rx
        }
        val rightY = when {
            caps.hasRz -> sample.rz
            caps.hasRy -> sample.ry
            else -> if (abs(sample.rz) >= abs(sample.ry)) sample.rz else sample.ry
        }
        return rightX to rightY
    }

    fun selectTriggerAxes(sample: MotionSample, caps: AxisCapabilities): Pair<Float, Float> {
        val left = when {
            caps.hasLTrigger && sample.lTrigger > 0f -> sample.lTrigger
            caps.hasBrake && sample.brake > 0f -> sample.brake
            !caps.hasLTrigger && caps.hasBrake -> sample.brake
            else -> maxOf(sample.lTrigger, sample.brake)
        }
        val right = when {
            caps.hasRTrigger && sample.rTrigger > 0f -> sample.rTrigger
            caps.hasGas && sample.gas > 0f -> sample.gas
            !caps.hasRTrigger && caps.hasGas -> sample.gas
            else -> maxOf(sample.rTrigger, sample.gas)
        }
        return left to right
    }
}

/**
 * Assigns connected Android controller deviceIds to XInput slots 0..3.
 */
class GamepadSlotManager(private val maxSlots: Int = 4) {
    private val deviceToSlot = LinkedHashMap<Int, ControllerStateTracker>()

    val connectedCount: Int
        get() = deviceToSlot.size

    fun ensureDevice(deviceId: Int): ControllerStateTracker? {
        deviceToSlot[deviceId]?.let { return it }
        val used = deviceToSlot.values.mapTo(HashSet()) { it.slot }
        val freeSlot = (0 until maxSlots).firstOrNull { it !in used } ?: return null
        val tracker = ControllerStateTracker(freeSlot)
        deviceToSlot[deviceId] = tracker
        return tracker
    }

    fun getTracker(deviceId: Int): ControllerStateTracker? = deviceToSlot[deviceId]

    fun removeDevice(deviceId: Int): GamepadState? {
        val removed = deviceToSlot.remove(deviceId) ?: return null
        return GamepadState(slot = removed.slot, connected = false)
    }

    fun activeSnapshots(): List<GamepadState> =
        deviceToSlot.values.map { it.snapshot() }.sortedBy { it.slot }

    fun clearAll(): List<GamepadState> {
        val disconnects = deviceToSlot.values
            .map { GamepadState(slot = it.slot, connected = false) }
            .sortedBy { it.slot }
        deviceToSlot.clear()
        return disconnects
    }
}
