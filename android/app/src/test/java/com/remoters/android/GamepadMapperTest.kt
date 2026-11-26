package com.remoters.android.input

import com.remoters.android.ui.JsonParsers
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class GamepadMapperTest {

    @Test
    fun mapsAllFaceShoulderThumbAndMenuButtonsToXInputBitmasks() {
        val tracker = ControllerStateTracker(slot = 0)
        val keys = listOf(
            AndroidGamepadKeyCodes.BUTTON_A to XInputButtons.A,
            AndroidGamepadKeyCodes.BUTTON_B to XInputButtons.B,
            AndroidGamepadKeyCodes.BUTTON_X to XInputButtons.X,
            AndroidGamepadKeyCodes.BUTTON_Y to XInputButtons.Y,
            AndroidGamepadKeyCodes.BUTTON_L1 to XInputButtons.LB,
            AndroidGamepadKeyCodes.BUTTON_R1 to XInputButtons.RB,
            AndroidGamepadKeyCodes.BUTTON_THUMBL to XInputButtons.LTHUMB,
            AndroidGamepadKeyCodes.BUTTON_THUMBR to XInputButtons.RTHUMB,
            AndroidGamepadKeyCodes.BUTTON_START to XInputButtons.START,
            AndroidGamepadKeyCodes.BUTTON_SELECT to XInputButtons.BACK,
            AndroidGamepadKeyCodes.BUTTON_MODE to XInputButtons.GUIDE
        )
        var expected = 0
        for ((keyCode, bit) in keys) {
            val state = tracker.onKey(keyCode, down = true)
            assertNotNull(state)
            expected = expected or bit
            assertEquals(expected, state!!.buttons)
        }
        val releasedA = tracker.onKey(AndroidGamepadKeyCodes.BUTTON_A, down = false)!!
        assertEquals(expected and XInputButtons.A.inv(), releasedA.buttons)
    }

    @Test
    fun mapsDigitalAndHatDpadAndDigitalTriggerFallback() {
        val tracker = ControllerStateTracker(slot = 1)
        tracker.onKey(AndroidGamepadKeyCodes.DPAD_UP, down = true)
        tracker.onKey(AndroidGamepadKeyCodes.BUTTON_L2, down = true)
        tracker.onKey(AndroidGamepadKeyCodes.BUTTON_R2, down = true)
        var state = tracker.snapshot()
        assertEquals(XInputButtons.DPAD_UP, state.buttons)
        assertEquals(255, state.lt)
        assertEquals(255, state.rt)

        state = tracker.onMotion(MotionSample(hatX = -1.0f, hatY = 1.0f))
        assertEquals(
            XInputButtons.DPAD_UP or XInputButtons.DPAD_LEFT or XInputButtons.DPAD_DOWN,
            state.buttons
        )

        tracker.onKey(AndroidGamepadKeyCodes.DPAD_UP, down = false)
        state = tracker.onMotion(MotionSample(hatX = 0.0f, hatY = 0.0f))
        assertEquals(0, state.buttons)
    }

    @Test
    fun invertsYAxesAppliesDeadzoneAndNormalizesTriggers() {
        val tracker = ControllerStateTracker(slot = 0)
        val deadzoneState = tracker.onMotion(
            MotionSample(x = 0.05f, y = -0.05f, z = 0.04f, rz = -0.04f, lTrigger = 0.01f)
        )
        assertEquals(0, deadzoneState.lx)
        assertEquals(0, deadzoneState.ly)
        assertEquals(0, deadzoneState.rx)
        assertEquals(0, deadzoneState.ry)
        assertEquals(0, deadzoneState.lt)

        val fullState = tracker.onMotion(
            MotionSample(
                x = 1.0f,
                y = -1.0f, // Android up (-1.0) -> XInput positive (+32767)
                z = -1.0f,
                rz = 1.0f, // Android down (+1.0) -> XInput negative (-32767)
                lTrigger = 0.5f,
                rTrigger = 1.0f
            )
        )
        assertEquals(32767, fullState.lx)
        assertEquals(32767, fullState.ly)
        assertEquals(-32767, fullState.rx)
        assertEquals(-32767, fullState.ry)
        assertEquals(128, fullState.lt)
        assertEquals(255, fullState.rt)
    }

    @Test
    fun supportsRxRyFallbackAndGasBrakeTriggers() {
        val caps = AxisCapabilities(
            hasZ = false,
            hasRz = false,
            hasRx = true,
            hasRy = true,
            hasLTrigger = false,
            hasRTrigger = false,
            hasBrake = true,
            hasGas = true
        )
        val tracker = ControllerStateTracker(slot = 2)
        val state = tracker.onMotion(
            MotionSample(rx = 1.0f, ry = -1.0f, brake = 1.0f, gas = 0.5f),
            caps
        )
        assertEquals(32767, state.rx)
        assertEquals(32767, state.ry)
        assertEquals(255, state.lt)
        assertEquals(128, state.rt)
    }

    @Test
    fun managesFourSlotsAndEmitsDisconnectOnRemoval() {
        val manager = GamepadSlotManager(maxSlots = 4)
        val s0 = manager.ensureDevice(101)
        val s1 = manager.ensureDevice(102)
        val s2 = manager.ensureDevice(103)
        val s3 = manager.ensureDevice(104)
        assertEquals(0, s0?.slot)
        assertEquals(1, s1?.slot)
        assertEquals(2, s2?.slot)
        assertEquals(3, s3?.slot)
        assertNull(manager.ensureDevice(105))
        assertEquals(4, manager.connectedCount)

        val removed = manager.removeDevice(102)
        assertNotNull(removed)
        assertEquals(1, removed!!.slot)
        assertFalse(removed.connected)
        assertEquals(3, manager.connectedCount)

        // Reuses lowest free slot (1)
        val reused = manager.ensureDevice(105)
        assertEquals(1, reused?.slot)
        assertEquals(4, manager.connectedCount)

        val allDisconnected = manager.clearAll()
        assertEquals(4, allDisconnected.size)
        assertTrue(allDisconnected.all { !it.connected })
        assertEquals(0, manager.connectedCount)
    }

    @Test
    fun parsesNativeJsonResponsesSafely() {
        val auth = JsonParsers.parseAuthSession(
            """{"ok":true,"token":"tok-1","user":{"id":"u1","username":"alice","display_name":"Alice"}}""",
            "http://127.0.0.1:8787"
        ).getOrThrow()
        assertEquals("tok-1", auth.token)
        assertEquals("alice", auth.username)

        val devices = JsonParsers.parseDevices(
            """{"ok":true,"devices":[{"device_id":"d1","name":"Win11","platform":"Windows","host_address":"192.168.1.20:47821","pairing_code":"1234","sharing":true,"view_only":false,"display_label":"1080p","last_seen_unix_ms":1000,"online":true}]}"""
        ).getOrThrow()
        assertEquals(1, devices.size)
        assertTrue(devices[0].online)
        assertTrue(devices[0].sharing)

        val stats = JsonParsers.parseStats(
            """{"ok":true,"width":1920,"height":1080,"targetFps":60,"codec":"HEVC","fps":59.9,"videoBps":24000000.0,"rttMs":3.5,"encodeMs":2.1,"e2eMs":9.4,"lostPackets":0}"""
        )
        assertNotNull(stats)
        assertEquals("HEVC", stats!!.codec)
        assertEquals(1920, stats.width)
    }
}
