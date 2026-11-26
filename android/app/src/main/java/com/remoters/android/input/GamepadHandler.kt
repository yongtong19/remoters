package com.remoters.android.input

import android.content.Context
import android.hardware.input.InputManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

class GamepadHandler(
    context: Context,
    private val onGamepadState: (GamepadState) -> Unit
) : InputManager.InputDeviceListener {

    private val inputManager = context.getSystemService(Context.INPUT_SERVICE) as InputManager
    private val slotManager = GamepadSlotManager(maxSlots = 4)
    private val capabilitiesByDevice = HashMap<Int, AxisCapabilities>()
    private val lastSentBySlot = HashMap<Int, GamepadState>()

    private val _connectedCount = MutableStateFlow(0)
    val connectedCount: StateFlow<Int> = _connectedCount.asStateFlow()

    private var registered = false

    fun start() {
        if (registered) return
        registered = true
        inputManager.registerInputDeviceListener(this, Handler(Looper.getMainLooper()))
        scanExistingDevices()
    }

    fun activeSnapshots(): List<GamepadState> = slotManager.activeSnapshots()

    fun stop() {
        if (registered) {
            inputManager.unregisterInputDeviceListener(this)
            registered = false
        }
        val disconnects = slotManager.clearAll()
        capabilitiesByDevice.clear()
        lastSentBySlot.clear()
        _connectedCount.value = 0
        disconnects.forEach { onGamepadState(it) }
    }

    private fun scanExistingDevices() {
        for (deviceId in InputDevice.getDeviceIds()) {
            val device = InputDevice.getDevice(deviceId) ?: continue
            if (isGamepadDevice(device)) {
                registerDevice(device)
            }
        }
    }

    private fun registerDevice(device: InputDevice) {
        val tracker = slotManager.ensureDevice(device.id) ?: return
        capabilitiesByDevice[device.id] = inspectCapabilities(device)
        _connectedCount.value = slotManager.connectedCount
        emitIfChanged(tracker.snapshot(), force = true)
    }

    override fun onInputDeviceAdded(deviceId: Int) {
        val device = InputDevice.getDevice(deviceId) ?: return
        if (isGamepadDevice(device)) {
            registerDevice(device)
        }
    }

    override fun onInputDeviceChanged(deviceId: Int) {
        val device = InputDevice.getDevice(deviceId)
        if (device == null || !isGamepadDevice(device)) {
            onInputDeviceRemoved(deviceId)
            return
        }
        if (slotManager.getTracker(deviceId) == null) {
            registerDevice(device)
        } else {
            capabilitiesByDevice[deviceId] = inspectCapabilities(device)
        }
    }

    override fun onInputDeviceRemoved(deviceId: Int) {
        capabilitiesByDevice.remove(deviceId)
        val disconnected = slotManager.removeDevice(deviceId) ?: return
        lastSentBySlot.remove(disconnected.slot)
        _connectedCount.value = slotManager.connectedCount
        onGamepadState(disconnected)
    }

    fun handleKeyEvent(event: KeyEvent): Boolean {
        val device = event.device ?: return false
        if (!isGamepadEventSource(event.source) && !isGamepadDevice(device)) return false
        if (event.repeatCount > 0) return true

        val tracker = slotManager.getTracker(device.id) ?: run {
            registerDevice(device)
            slotManager.getTracker(device.id)
        } ?: return false

        val down = event.action == KeyEvent.ACTION_DOWN
        if (event.action != KeyEvent.ACTION_DOWN && event.action != KeyEvent.ACTION_UP) {
            return false
        }
        val state = tracker.onKey(event.keyCode, down) ?: return false
        emitIfChanged(state)
        return true
    }

    fun handleMotionEvent(event: MotionEvent): Boolean {
        if ((event.source and InputDevice.SOURCE_JOYSTICK) != InputDevice.SOURCE_JOYSTICK &&
            (event.source and InputDevice.SOURCE_GAMEPAD) != InputDevice.SOURCE_GAMEPAD
        ) {
            return false
        }
        if (event.action != MotionEvent.ACTION_MOVE) return false
        val device = event.device ?: return false

        val tracker = slotManager.getTracker(device.id) ?: run {
            registerDevice(device)
            slotManager.getTracker(device.id)
        } ?: return false

        val caps = capabilitiesByDevice[device.id] ?: inspectCapabilities(device)
        val sample = MotionSample(
            x = getCenteredAxis(event, device, MotionEvent.AXIS_X),
            y = getCenteredAxis(event, device, MotionEvent.AXIS_Y),
            z = getCenteredAxis(event, device, MotionEvent.AXIS_Z),
            rz = getCenteredAxis(event, device, MotionEvent.AXIS_RZ),
            rx = getCenteredAxis(event, device, MotionEvent.AXIS_RX),
            ry = getCenteredAxis(event, device, MotionEvent.AXIS_RY),
            lTrigger = event.getAxisValue(MotionEvent.AXIS_LTRIGGER),
            rTrigger = event.getAxisValue(MotionEvent.AXIS_RTRIGGER),
            brake = event.getAxisValue(MotionEvent.AXIS_BRAKE),
            gas = event.getAxisValue(MotionEvent.AXIS_GAS),
            hatX = event.getAxisValue(MotionEvent.AXIS_HAT_X),
            hatY = event.getAxisValue(MotionEvent.AXIS_HAT_Y)
        )
        val state = tracker.onMotion(sample, caps)
        emitIfChanged(state)
        return true
    }

    private fun emitIfChanged(state: GamepadState, force: Boolean = false) {
        val prev = lastSentBySlot[state.slot]
        if (force || prev != state) {
            lastSentBySlot[state.slot] = state
            onGamepadState(state)
        }
    }

    private fun getCenteredAxis(event: MotionEvent, device: InputDevice, axis: Int): Float {
        val range = device.getMotionRange(axis, event.source) ?: return 0f
        val value = event.getAxisValue(axis)
        return if (kotlin.math.abs(value) > range.flat) value else 0f
    }

    private fun inspectCapabilities(device: InputDevice): AxisCapabilities {
        fun hasAxis(axis: Int): Boolean =
            device.getMotionRange(axis, InputDevice.SOURCE_JOYSTICK) != null ||
                device.getMotionRange(axis, InputDevice.SOURCE_GAMEPAD) != null

        return AxisCapabilities(
            hasZ = hasAxis(MotionEvent.AXIS_Z),
            hasRz = hasAxis(MotionEvent.AXIS_RZ),
            hasRx = hasAxis(MotionEvent.AXIS_RX),
            hasRy = hasAxis(MotionEvent.AXIS_RY),
            hasLTrigger = hasAxis(MotionEvent.AXIS_LTRIGGER),
            hasRTrigger = hasAxis(MotionEvent.AXIS_RTRIGGER),
            hasBrake = hasAxis(MotionEvent.AXIS_BRAKE),
            hasGas = hasAxis(MotionEvent.AXIS_GAS)
        )
    }

    companion object {
        fun isGamepadEventSource(source: Int): Boolean =
            (source and InputDevice.SOURCE_GAMEPAD) == InputDevice.SOURCE_GAMEPAD ||
                (source and InputDevice.SOURCE_JOYSTICK) == InputDevice.SOURCE_JOYSTICK

        fun isGamepadDevice(device: InputDevice): Boolean {
            if (device.isVirtual) return false
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q && !device.isExternal) return false
            if (device.name.startsWith("uinput-", ignoreCase = true)) return false
            val sources = device.sources
            val hasJoystick = (sources and InputDevice.SOURCE_JOYSTICK) == InputDevice.SOURCE_JOYSTICK
            val hasGamepad = (sources and InputDevice.SOURCE_GAMEPAD) == InputDevice.SOURCE_GAMEPAD
            return hasJoystick || (hasGamepad && device.controllerNumber > 0)
        }
    }
}
