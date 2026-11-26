package com.remoters.android.session

import android.view.Surface
import com.remoters.android.nativebridge.SessionCallback
import com.remoters.android.video.VideoDecoder
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.concurrent.Executor

class SessionManagerTest {

    private class FakeNativeAdapter : SessionNativeAdapter {
        private var nextHandle = 100L
        val startedHandles = mutableListOf<Long>()
        val startedRequests = mutableListOf<SessionLaunchRequest>()
        val stoppedHandles = mutableListOf<Long>()
        val callbacksByHandle = mutableMapOf<Long, SessionCallback>()
        val mutedByHandle = mutableMapOf<Long, Boolean>()
        val bitrateByHandle = mutableMapOf<Long, Int>()
        val releaseAllCalls = mutableListOf<Long>()

        override fun probeSupportedCodecsMask(): Int = 7

        override fun sessionStart(
            request: SessionLaunchRequest,
            supportedCodecs: Int,
            callback: SessionCallback
        ): Long {
            val h = nextHandle++
            startedHandles.add(h)
            startedRequests.add(request)
            callbacksByHandle[h] = callback
            return h
        }

        override fun sessionStop(handle: Long) {
            stoppedHandles.add(handle)
        }

        override fun sessionSetBitrate(handle: Long, bitrateMbps: Int) {
            bitrateByHandle[handle] = bitrateMbps
        }

        override fun sessionSetAudioMuted(handle: Long, muted: Boolean) {
            mutedByHandle[handle] = muted
        }

        override fun sessionReleaseAll(handle: Long) {
            releaseAllCalls.add(handle)
        }

        override fun sessionSendPointerMove(handle: Long, x: Int, y: Int) = Unit
        override fun sessionSendPointerButton(handle: Long, x: Int, y: Int, button: Int, down: Boolean) = Unit
        override fun sessionSendScroll(handle: Long, x: Int, y: Int, vertical: Int, horizontal: Int) = Unit
        override fun sessionSendKey(handle: Long, vkCode: Int, down: Boolean) = Unit
        override fun sessionSendGamepad(
            handle: Long,
            slot: Int,
            connected: Boolean,
            buttons: Int,
            lt: Int,
            rt: Int,
            lx: Int,
            ly: Int,
            rx: Int,
            ry: Int
        ) = Unit
    }

    private class FakeVideoEngine : SessionVideoEngine {
        override var averageDecodeMs: Double = 2.4
        var hdrEnabled: Boolean = false
        var pacingMode: VideoDecoder.FramePacingMode = VideoDecoder.FramePacingMode.LOWEST_LATENCY
        var surfaceAttachedCount: Int = 0
        var surfaceDetachedCount: Int = 0
        var released: Boolean = false
        var outputFormatListener: ((width: Int, height: Int) -> Unit)? = null

        override fun setHdr(enabled: Boolean) {
            hdrEnabled = enabled
        }

        override fun setFramePacingMode(mode: VideoDecoder.FramePacingMode) {
            pacingMode = mode
        }

        override fun setOnOutputFormatChangedListener(listener: ((width: Int, height: Int) -> Unit)?) {
            outputFormatListener = listener
        }

        override fun setSurface(surface: Surface?) {
            if (surface == null) surfaceDetachedCount++ else surfaceAttachedCount++
        }

        override fun decodeFrame(
            codecType: Int,
            keyframe: Boolean,
            width: Int,
            height: Int,
            data: ByteArray
        ) = Unit

        override fun release() {
            released = true
        }
    }

    private class FakeAudioEngine : SessionAudioEngine {
        private var muted: Boolean = false
        var pcmPushedCount: Int = 0
        var released: Boolean = false

        override fun setMuted(muted: Boolean) {
            this.muted = muted
        }

        override fun isMuted(): Boolean = muted

        override fun pushPcm(pcm16Le: ByteArray, sampleRate: Int, channels: Int) {
            pcmPushedCount++
        }

        override fun release() {
            released = true
        }
    }

    private fun createCoordinator(): Triple<SessionCoordinator, FakeNativeAdapter, MutableList<Pair<FakeVideoEngine, FakeAudioEngine>>> {
        val native = FakeNativeAdapter()
        val engines = mutableListOf<Pair<FakeVideoEngine, FakeAudioEngine>>()
        var pendingVideo: FakeVideoEngine? = null
        val directExecutor = Executor { it.run() }
        val coordinator = SessionCoordinator(
            nativeAdapter = native,
            videoFactory = {
                val v = FakeVideoEngine()
                pendingVideo = v
                v
            },
            audioFactory = {
                val a = FakeAudioEngine()
                engines.add(pendingVideo!! to a)
                a
            },
            ioExecutor = directExecutor
        )
        return Triple(coordinator, native, engines)
    }

    @Test
    fun connectsMultipleSessionsWithoutDisconnectingExistingAndDeduplicatesExactMatch() {
        val (coordinator, native, engines) = createCoordinator()
        coordinator.attachForeground(null)

        val r1 = coordinator.connectOrSwitch(
            SessionLaunchRequest(
                title = "Win11",
                address = "192.168.1.10:47821",
                code = "AAAA-BBBB"
            )
        )
        assertFalse(r1.reusedExisting)
        assertEquals(1, coordinator.sessions.value.size)
        assertEquals(r1.session.id, coordinator.activeSessionId.value)
        assertEquals(100L, r1.session.nativeHandle)
        assertFalse(r1.session.effectiveAudioMuted)
        assertEquals(false, native.mutedByHandle[100L])

        // Connect a second distinct host: first stays connected (NOT stopped) and gets muted/backgrounded
        val r2 = coordinator.connectOrSwitch(
            SessionLaunchRequest(
                title = "Ubuntu",
                address = "192.168.1.20:47821",
                code = "CCCC-DDDD",
                framePacingMode = VideoDecoder.FramePacingMode.BALANCED
            )
        )
        assertFalse(r2.reusedExisting)
        assertEquals(2, coordinator.sessions.value.size)
        assertEquals(r2.session.id, coordinator.activeSessionId.value)
        assertTrue(native.stoppedHandles.isEmpty())
        assertTrue(r1.session.effectiveAudioMuted)
        assertEquals(true, native.mutedByHandle[100L])
        assertTrue(engines[0].second.isMuted())
        assertTrue(native.releaseAllCalls.contains(100L))
        assertEquals(VideoDecoder.FramePacingMode.BALANCED, engines[1].first.pacingMode)
        assertFalse(r2.session.effectiveAudioMuted)
        assertEquals(false, native.mutedByHandle[101L])

        // Re-connecting to exact same address + code switches instead of duplicating
        val dup = coordinator.connectOrSwitch(
            SessionLaunchRequest(
                title = "Win11 Again",
                address = " 192.168.1.10:47821 ",
                code = "AAAA-BBBB "
            )
        )
        assertTrue(dup.reusedExisting)
        assertEquals(r1.session.id, dup.session.id)
        assertEquals(2, coordinator.sessions.value.size)
        assertEquals(r1.session.id, coordinator.activeSessionId.value)
        // Session 1 is back in foreground (unmuted), Session 2 is background-muted
        assertEquals(false, native.mutedByHandle[100L])
        assertEquals(true, native.mutedByHandle[101L])
        assertTrue(native.releaseAllCalls.contains(101L))
    }

    @Test
    fun backgroundMutePreservesUserMutePreferenceAndBlocksPcm() {
        val (coordinator, native, engines) = createCoordinator()
        coordinator.attachForeground(null)

        val s1 = coordinator.connectOrSwitch(
            SessionLaunchRequest(address = "10.0.0.1:47821", code = "CODE1")
        ).session
        val s2 = coordinator.connectOrSwitch(
            SessionLaunchRequest(address = "10.0.0.2:47821", code = "CODE2")
        ).session

        // User explicitly mutes s2 while active
        coordinator.setAudioMuted(s2.id, true)
        assertTrue(s2.audioMuted)
        assertEquals(true, native.mutedByHandle[s2.nativeHandle])

        // Switch to s1 -> s1 unmutes, s2 stays muted
        coordinator.switchTo(s1.id)
        assertEquals(false, native.mutedByHandle[s1.nativeHandle])
        assertEquals(true, native.mutedByHandle[s2.nativeHandle])

        // Background s2 receives audio PCM -> ignored because effectiveAudioMuted is true
        native.callbacksByHandle[s2.nativeHandle]!!.onAudioPcm(byteArrayOf(1, 2, 3, 4), 48000, 2)
        assertEquals(0, engines[1].second.pcmPushedCount)

        // Foreground s1 receives audio PCM -> pushed
        native.callbacksByHandle[s1.nativeHandle]!!.onAudioPcm(byteArrayOf(1, 2, 3, 4), 48000, 2)
        assertEquals(1, engines[0].second.pcmPushedCount)

        // Switch back to s2 -> user mute preference (true) is still respected!
        coordinator.switchTo(s2.id)
        assertTrue(s2.audioMuted)
        assertTrue(s2.effectiveAudioMuted)
        assertEquals(true, native.mutedByHandle[s2.nativeHandle])

        // Detach foreground (e.g. return to Home) -> all sessions muted & surface detached
        coordinator.detachForeground()
        assertEquals(true, native.mutedByHandle[s1.nativeHandle])
        assertEquals(true, native.mutedByHandle[s2.nativeHandle])
        assertTrue(engines[0].first.surfaceDetachedCount > 0)
        assertTrue(engines[1].first.surfaceDetachedCount > 0)
    }

    @Test
    fun closingActiveSessionReleasesOnlyThatSessionAndSelectsNeighbor() {
        val (coordinator, native, engines) = createCoordinator()
        coordinator.attachForeground(null)

        val s1 = coordinator.connectOrSwitch(
            SessionLaunchRequest(title = "Host1", address = "10.0.0.1:47821", code = "C1")
        ).session
        val s2 = coordinator.connectOrSwitch(
            SessionLaunchRequest(title = "Host2", address = "10.0.0.2:47821", code = "C2")
        ).session
        val s3 = coordinator.connectOrSwitch(
            SessionLaunchRequest(title = "Host3", address = "10.0.0.3:47821", code = "C3")
        ).session

        // Focus middle session s2, then close it -> should switch to right neighbor s3
        coordinator.switchTo(s2.id)
        val afterCloseMiddle = coordinator.closeSession(s2.id)
        assertEquals(s3.id, afterCloseMiddle)
        assertEquals(s3.id, coordinator.activeSessionId.value)
        assertEquals(listOf(s1.id, s3.id), coordinator.sessions.value.map { it.id })
        assertEquals(listOf(101L), native.stoppedHandles)
        assertTrue(engines[1].first.released)
        assertTrue(engines[1].second.released)
        assertFalse(engines[0].first.released)
        assertFalse(engines[2].first.released)
        assertEquals(false, native.mutedByHandle[s3.nativeHandle])

        // Close last session s3 -> should switch to left neighbor s1
        val afterCloseLast = coordinator.closeSession(s3.id)
        assertEquals(s1.id, afterCloseLast)
        assertEquals(s1.id, coordinator.activeSessionId.value)
        assertEquals(false, native.mutedByHandle[s1.nativeHandle])

        // Close final session s1 -> active becomes null
        val afterCloseFinal = coordinator.closeSession(s1.id)
        assertNull(afterCloseFinal)
        assertNull(coordinator.activeSessionId.value)
        assertTrue(coordinator.sessions.value.isEmpty())
    }

    @Test
    fun updatesBitratePacingCallbacksAndReplacesClosedDuplicate() {
        val (coordinator, native, engines) = createCoordinator()
        coordinator.attachForeground(null)

        val s1 = coordinator.connectOrSwitch(
            SessionLaunchRequest(address = "192.168.1.5:47821", code = "XYZ")
        ).session
        val cb = native.callbacksByHandle[s1.nativeHandle]
        assertNotNull(cb)

        cb!!.onReady(viewOnly = false, hdr = true)
        assertEquals(SessionConnectionState.CONNECTED, s1.connectionState)
        assertTrue(s1.canControl)
        assertTrue(s1.hdr)
        assertTrue(engines[0].first.hdrEnabled)

        coordinator.setBitrate(s1.id, 80)
        assertEquals(80, s1.bitrateMbps)
        assertEquals(80, native.bitrateByHandle[s1.nativeHandle])

        coordinator.setFramePacingMode(s1.id, VideoDecoder.FramePacingMode.BALANCED)
        assertEquals(VideoDecoder.FramePacingMode.BALANCED, s1.framePacingMode)
        assertEquals(VideoDecoder.FramePacingMode.BALANCED, engines[0].first.pacingMode)

        cb.onClosed("Connection lost")
        assertEquals(SessionConnectionState.CLOSED, s1.connectionState)
        assertEquals("Connection lost", s1.lastError)

        // Connecting again to same address + code replaces the CLOSED session
        val reconnect = coordinator.connectOrSwitch(
            SessionLaunchRequest(address = "192.168.1.5:47821", code = "XYZ")
        )
        assertFalse(reconnect.reusedExisting)
        assertEquals(1, coordinator.sessions.value.size)
        assertEquals(SessionConnectionState.CONNECTING, reconnect.session.connectionState)
    }

    @Test
    fun reconfiguresResolutionAndFpsInPlaceAndPropagatesDecodedOutputFormat() {
        val (coordinator, native, engines) = createCoordinator()
        val observedSizes = mutableListOf<Pair<Int, Int>>()
        coordinator.setListeners(onFrameSizeChanged = { _, w, h ->
            observedSizes.add(w to h)
        })
        coordinator.attachForeground(null)

        val session = coordinator.connectOrSwitch(
            SessionLaunchRequest(
                title = "Win4K",
                address = "192.168.1.3:47821",
                code = "PAIR-CODE",
                width = 2656,
                height = 1220,
                codecPref = 2,
                hdr = true,
                maxFps = 60,
                bitrateMbps = 50,
                framePacingMode = VideoDecoder.FramePacingMode.BALANCED
            )
        ).session
        val originalId = session.id
        val firstHandle = session.nativeHandle
        val firstCb = native.callbacksByHandle[firstHandle]!!

        // Container reports 1920x1080, then decoder output format reports actual decoded 3840x2160
        firstCb.onVideoFrame(1, true, 1920, 1080, byteArrayOf(1), 1000L)
        assertEquals(1920, session.frameWidth)
        assertEquals(1080, session.frameHeight)

        engines[0].first.outputFormatListener!!.invoke(3840, 2160)
        assertEquals(3840, session.frameWidth)
        assertEquals(2160, session.frameHeight)

        // Subsequent frame with same 1920x1080 container must NOT overwrite decoded 3840x2160
        firstCb.onVideoFrame(1, false, 1920, 1080, byteArrayOf(2), 1000L)
        assertEquals(3840, session.frameWidth)
        assertEquals(2160, session.frameHeight)
        assertEquals(listOf(1920 to 1080, 3840 to 2160), observedSizes)

        // User mutes audio, then switches resolution to 2560x1440 @ 120fps in-session
        coordinator.setAudioMuted(originalId, true)
        assertTrue(coordinator.reconfigureStream(originalId, 2560, 1440, 120))

        // Same session ID, old handle stopped, new handle started with preserved preferences
        assertEquals(1, coordinator.sessions.value.size)
        assertEquals(originalId, coordinator.activeSessionId.value)
        assertEquals(listOf(firstHandle), native.stoppedHandles)
        val secondHandle = session.nativeHandle
        assertTrue(secondHandle != firstHandle && secondHandle > 0L)
        assertTrue(session.audioMuted)
        assertEquals(true, native.mutedByHandle[secondHandle])
        assertEquals(2560, session.requestedWidth)
        assertEquals(1440, session.requestedHeight)
        assertEquals(120, session.requestedMaxFps)
        val secondReq = native.startedRequests.last()
        assertEquals(2560, secondReq.width)
        assertEquals(1440, secondReq.height)
        assertEquals(120, secondReq.maxFps)
        assertEquals(2, secondReq.codecPref)
        assertTrue(secondReq.hdr)
        assertEquals(50, secondReq.bitrateMbps)
        assertEquals(VideoDecoder.FramePacingMode.BALANCED, secondReq.framePacingMode)

        // Stale onClosed from old handle is ignored; new handle's onReady transitions to CONNECTED
        firstCb.onClosed("Old socket closed")
        assertEquals(SessionConnectionState.CONNECTING, session.connectionState)
        assertNull(session.lastError)

        native.callbacksByHandle[secondHandle]!!.onReady(viewOnly = false, hdr = true)
        assertEquals(SessionConnectionState.CONNECTED, session.connectionState)
    }
}
