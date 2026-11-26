package com.remoters.android.session

import android.view.Surface
import com.remoters.android.audio.AudioPlayer
import com.remoters.android.nativebridge.NativeBridge
import com.remoters.android.nativebridge.SessionCallback
import com.remoters.android.ui.JsonParsers
import com.remoters.android.ui.StreamStats
import com.remoters.android.video.VideoDecoder
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import java.util.concurrent.Executor
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger

enum class SessionConnectionState {
    CONNECTING,
    CONNECTED,
    CLOSED
}

data class SessionLaunchRequest(
    val title: String = "",
    val address: String,
    val code: String,
    val width: Int = 0,
    val height: Int = 0,
    val codecPref: Int = 0,
    val supportedCodecs: Int = -1,
    val hdr: Boolean = false,
    val maxFps: Int = 0,
    val bitrateMbps: Int = 0,
    val framePacingMode: VideoDecoder.FramePacingMode = VideoDecoder.FramePacingMode.LOWEST_LATENCY
)

data class ManagedSessionSnapshot(
    val nativeHandle: Long = 0L,
    val connectionState: SessionConnectionState = SessionConnectionState.CONNECTING,
    val stats: StreamStats = StreamStats(),
    val decodeMs: Double = 0.0,
    val requestedWidth: Int = 0,
    val requestedHeight: Int = 0,
    val requestedMaxFps: Int = 0,
    val codecPref: Int = 0,
    val supportedCodecs: Int = -1,
    val bitrateMbps: Int = 0,
    val framePacingMode: VideoDecoder.FramePacingMode = VideoDecoder.FramePacingMode.LOWEST_LATENCY,
    val audioMuted: Boolean = false,
    val backgroundMuted: Boolean = false,
    val canControl: Boolean = true,
    val hdr: Boolean = false,
    val lastError: String? = null,
    val frameWidth: Int = 0,
    val frameHeight: Int = 0
) {
    val effectiveAudioMuted: Boolean
        get() = audioMuted || backgroundMuted
}

interface SessionNativeAdapter {
    fun probeSupportedCodecsMask(): Int
    fun sessionStart(request: SessionLaunchRequest, supportedCodecs: Int, callback: SessionCallback): Long
    fun sessionStop(handle: Long)
    fun sessionSetBitrate(handle: Long, bitrateMbps: Int)
    fun sessionSetAudioMuted(handle: Long, muted: Boolean)
    fun sessionReleaseAll(handle: Long)
    fun sessionSendPointerMove(handle: Long, x: Int, y: Int)
    fun sessionSendPointerButton(handle: Long, x: Int, y: Int, button: Int, down: Boolean)
    fun sessionSendScroll(handle: Long, x: Int, y: Int, vertical: Int, horizontal: Int)
    fun sessionSendKey(handle: Long, vkCode: Int, down: Boolean)
    fun sessionSendGamepad(
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
    )
}

interface SessionVideoEngine {
    val averageDecodeMs: Double
    fun setHdr(enabled: Boolean)
    fun setFramePacingMode(mode: VideoDecoder.FramePacingMode)
    fun setOnOutputFormatChangedListener(listener: ((width: Int, height: Int) -> Unit)?)
    fun setSurface(surface: Surface?)
    fun decodeFrame(codecType: Int, keyframe: Boolean, width: Int, height: Int, data: ByteArray)
    fun release()
}

interface SessionAudioEngine {
    fun setMuted(muted: Boolean)
    fun isMuted(): Boolean
    fun pushPcm(pcm16Le: ByteArray, sampleRate: Int, channels: Int)
    fun release()
}

private class AndroidNativeAdapter : SessionNativeAdapter {
    override fun probeSupportedCodecsMask(): Int = VideoDecoder.probeSupportedCodecsMask()

    override fun sessionStart(
        request: SessionLaunchRequest,
        supportedCodecs: Int,
        callback: SessionCallback
    ): Long = NativeBridge.sessionStart(
        request.address,
        request.code,
        request.width,
        request.height,
        request.codecPref,
        supportedCodecs,
        request.hdr,
        request.maxFps,
        request.bitrateMbps,
        callback
    )

    override fun sessionStop(handle: Long) = NativeBridge.sessionStop(handle)
    override fun sessionSetBitrate(handle: Long, bitrateMbps: Int) =
        NativeBridge.sessionSetBitrate(handle, bitrateMbps)

    override fun sessionSetAudioMuted(handle: Long, muted: Boolean) =
        NativeBridge.sessionSetAudioMuted(handle, muted)

    override fun sessionReleaseAll(handle: Long) = NativeBridge.sessionReleaseAll(handle)

    override fun sessionSendPointerMove(handle: Long, x: Int, y: Int) =
        NativeBridge.sessionSendPointerMove(handle, x, y)

    override fun sessionSendPointerButton(handle: Long, x: Int, y: Int, button: Int, down: Boolean) =
        NativeBridge.sessionSendPointerButton(handle, x, y, button, down)

    override fun sessionSendScroll(handle: Long, x: Int, y: Int, vertical: Int, horizontal: Int) =
        NativeBridge.sessionSendScroll(handle, x, y, vertical, horizontal)

    override fun sessionSendKey(handle: Long, vkCode: Int, down: Boolean) =
        NativeBridge.sessionSendKey(handle, vkCode, down)

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
    ) = NativeBridge.sessionSendGamepad(handle, slot, connected, buttons, lt, rt, lx, ly, rx, ry)
}

private class AndroidVideoEngine(
    private val decoder: VideoDecoder = VideoDecoder()
) : SessionVideoEngine {
    override val averageDecodeMs: Double get() = decoder.averageDecodeMs
    override fun setHdr(enabled: Boolean) = decoder.setHdr(enabled)
    override fun setFramePacingMode(mode: VideoDecoder.FramePacingMode) =
        decoder.setFramePacingMode(mode)

    override fun setOnOutputFormatChangedListener(listener: ((width: Int, height: Int) -> Unit)?) =
        decoder.setOnOutputFormatChangedListener(listener)

    override fun setSurface(surface: Surface?) = decoder.setSurface(surface)
    override fun decodeFrame(codecType: Int, keyframe: Boolean, width: Int, height: Int, data: ByteArray) =
        decoder.decodeFrame(codecType, keyframe, width, height, data)

    override fun release() = decoder.release()
}

private class AndroidAudioEngine(
    private val player: AudioPlayer = AudioPlayer()
) : SessionAudioEngine {
    override fun setMuted(muted: Boolean) = player.setMuted(muted)
    override fun isMuted(): Boolean = player.isMuted()
    override fun pushPcm(pcm16Le: ByteArray, sampleRate: Int, channels: Int) =
        player.pushPcm(pcm16Le, sampleRate, channels)

    override fun release() = player.release()
}

class ManagedSession internal constructor(
    val id: String,
    val title: String,
    val address: String,
    val code: String,
    val videoDecoder: SessionVideoEngine,
    val audioPlayer: SessionAudioEngine,
    initialSnapshot: ManagedSessionSnapshot
) {
    private val _state = MutableStateFlow(initialSnapshot)
    val state: StateFlow<ManagedSessionSnapshot> = _state.asStateFlow()

    internal val closedLocally = AtomicBoolean(false)
    internal val sessionGeneration = AtomicInteger(0)
    @Volatile
    internal var lastContainerWidth: Int = 0
    @Volatile
    internal var lastContainerHeight: Int = 0

    val nativeHandle: Long get() = _state.value.nativeHandle
    val stats: StreamStats get() = _state.value.stats
    val decodeMs: Double get() = _state.value.decodeMs
    val requestedWidth: Int get() = _state.value.requestedWidth
    val requestedHeight: Int get() = _state.value.requestedHeight
    val requestedMaxFps: Int get() = _state.value.requestedMaxFps
    val codecPref: Int get() = _state.value.codecPref
    val supportedCodecs: Int get() = _state.value.supportedCodecs
    val bitrateMbps: Int get() = _state.value.bitrateMbps
    val framePacingMode: VideoDecoder.FramePacingMode get() = _state.value.framePacingMode
    val audioMuted: Boolean get() = _state.value.audioMuted
    val backgroundMuted: Boolean get() = _state.value.backgroundMuted
    val effectiveAudioMuted: Boolean get() = _state.value.effectiveAudioMuted
    val canControl: Boolean get() = _state.value.canControl
    val hdr: Boolean get() = _state.value.hdr
    val connectionState: SessionConnectionState get() = _state.value.connectionState
    val lastError: String? get() = _state.value.lastError
    val frameWidth: Int get() = _state.value.frameWidth
    val frameHeight: Int get() = _state.value.frameHeight

    internal fun updateState(transform: (ManagedSessionSnapshot) -> ManagedSessionSnapshot) {
        _state.update(transform)
    }
}

data class ConnectSessionResult(
    val session: ManagedSession,
    val reusedExisting: Boolean
)

class SessionCoordinator(
    private val nativeAdapter: SessionNativeAdapter,
    private val videoFactory: () -> SessionVideoEngine,
    private val audioFactory: () -> SessionAudioEngine,
    private val ioExecutor: Executor = Executors.newCachedThreadPool { r ->
        Thread(r, "remoters-session-io").apply { isDaemon = true }
    }
) {
    private val lock = Any()
    private val nextId = AtomicInteger(0)

    private val _sessions = MutableStateFlow<List<ManagedSession>>(emptyList())
    val sessions: StateFlow<List<ManagedSession>> = _sessions.asStateFlow()

    private val _activeSessionId = MutableStateFlow<String?>(null)
    val activeSessionId: StateFlow<String?> = _activeSessionId.asStateFlow()

    private var foregroundActive: Boolean = false
    private var foregroundSurface: Surface? = null
    private var onFrameSizeChanged: ((sessionId: String, width: Int, height: Int) -> Unit)? = null
    private var onSessionStarted: ((session: ManagedSession) -> Unit)? = null

    val activeSession: ManagedSession?
        get() {
            val activeId = _activeSessionId.value ?: return null
            return _sessions.value.firstOrNull { it.id == activeId }
        }

    fun getSession(sessionId: String): ManagedSession? =
        _sessions.value.firstOrNull { it.id == sessionId }

    fun setListeners(
        onFrameSizeChanged: ((sessionId: String, width: Int, height: Int) -> Unit)? = null,
        onSessionStarted: ((session: ManagedSession) -> Unit)? = null
    ) {
        synchronized(lock) {
            this.onFrameSizeChanged = onFrameSizeChanged
            this.onSessionStarted = onSessionStarted
        }
    }

    fun connectOrSwitch(request: SessionLaunchRequest): ConnectSessionResult {
        val normalizedAddress = request.address.trim()
        val normalizedCode = request.code.filterNot { it.isWhitespace() }
        val displayTitle = request.title.trim().ifBlank { deriveSessionTitle(normalizedAddress) }

        val sessionToStart: ManagedSession
        val normalizedRequest = request.copy(
            title = displayTitle,
            address = normalizedAddress,
            code = normalizedCode
        )

        synchronized(lock) {
            val existing = _sessions.value.firstOrNull {
                it.address == normalizedAddress && it.code == normalizedCode
            }
            if (existing != null && existing.connectionState != SessionConnectionState.CLOSED) {
                switchToLocked(existing.id)
                return ConnectSessionResult(existing, reusedExisting = true)
            }
            if (existing != null) {
                closeSessionLocked(existing.id)
            }

            val video = videoFactory().apply {
                setHdr(normalizedRequest.hdr)
                setFramePacingMode(normalizedRequest.framePacingMode)
            }
            val audio = audioFactory()
            val startInBackground = !foregroundActive
            audio.setMuted(startInBackground)

            val id = "session-${nextId.incrementAndGet()}"
            val created = ManagedSession(
                id = id,
                title = displayTitle,
                address = normalizedAddress,
                code = normalizedCode,
                videoDecoder = video,
                audioPlayer = audio,
                initialSnapshot = ManagedSessionSnapshot(
                    requestedWidth = normalizedRequest.width,
                    requestedHeight = normalizedRequest.height,
                    requestedMaxFps = normalizedRequest.maxFps,
                    codecPref = normalizedRequest.codecPref,
                    supportedCodecs = normalizedRequest.supportedCodecs,
                    bitrateMbps = normalizedRequest.bitrateMbps,
                    framePacingMode = normalizedRequest.framePacingMode,
                    audioMuted = false,
                    backgroundMuted = startInBackground,
                    canControl = true,
                    hdr = normalizedRequest.hdr,
                    connectionState = SessionConnectionState.CONNECTING
                )
            )
            video.setOnOutputFormatChangedListener { decodedW, decodedH ->
                if (created.closedLocally.get() || decodedW <= 0 || decodedH <= 0) return@setOnOutputFormatChangedListener
                if (decodedW != created.frameWidth || decodedH != created.frameHeight) {
                    created.updateState { it.copy(frameWidth = decodedW, frameHeight = decodedH) }
                    val listener = synchronized(lock) { onFrameSizeChanged }
                    listener?.invoke(created.id, decodedW, decodedH)
                }
            }

            // Background any previously active session without disconnecting it
            val previous = activeSessionLocked()
            if (previous != null) {
                demoteToBackgroundLocked(previous)
            }

            _sessions.value = _sessions.value + created
            _activeSessionId.value = created.id

            if (foregroundActive) {
                promoteToForegroundLocked(created, foregroundSurface)
            }
            sessionToStart = created
        }

        if (normalizedAddress.isBlank() || normalizedCode.isBlank()) {
            sessionToStart.updateState {
                it.copy(
                    connectionState = SessionConnectionState.CLOSED,
                    lastError = "Missing host address or pairing code."
                )
            }
            return ConnectSessionResult(sessionToStart, reusedExisting = false)
        }

        startNativeSessionAsync(
            session = sessionToStart,
            request = normalizedRequest,
            expectedGeneration = sessionToStart.sessionGeneration.get(),
            oldHandleToStop = 0L
        )

        return ConnectSessionResult(sessionToStart, reusedExisting = false)
    }

    fun reconfigureStream(
        sessionId: String,
        width: Int,
        height: Int,
        maxFps: Int
    ): Boolean {
        val target: ManagedSession
        val oldHandle: Long
        val newGeneration: Int
        val reconfigRequest: SessionLaunchRequest

        synchronized(lock) {
            val session = _sessions.value.firstOrNull { it.id == sessionId } ?: return false
            if (session.closedLocally.get()) return false
            if (session.requestedWidth == width &&
                session.requestedHeight == height &&
                session.requestedMaxFps == maxFps &&
                session.connectionState != SessionConnectionState.CLOSED
            ) {
                return false
            }

            newGeneration = session.sessionGeneration.incrementAndGet()
            oldHandle = session.nativeHandle
            session.lastContainerWidth = 0
            session.lastContainerHeight = 0

            val snapshot = session.state.value
            reconfigRequest = SessionLaunchRequest(
                title = session.title,
                address = session.address,
                code = session.code,
                width = width,
                height = height,
                codecPref = snapshot.codecPref,
                supportedCodecs = snapshot.supportedCodecs,
                hdr = snapshot.hdr,
                maxFps = maxFps,
                bitrateMbps = snapshot.bitrateMbps,
                framePacingMode = snapshot.framePacingMode
            )

            session.updateState {
                it.copy(
                    nativeHandle = 0L,
                    requestedWidth = width,
                    requestedHeight = height,
                    requestedMaxFps = maxFps,
                    connectionState = SessionConnectionState.CONNECTING,
                    lastError = null,
                    frameWidth = 0,
                    frameHeight = 0
                )
            }
            if (oldHandle != 0L) {
                nativeAdapter.sessionReleaseAll(oldHandle)
                nativeAdapter.sessionSetAudioMuted(oldHandle, true)
            }
            target = session
        }

        startNativeSessionAsync(
            session = target,
            request = reconfigRequest,
            expectedGeneration = newGeneration,
            oldHandleToStop = oldHandle
        )
        return true
    }

    private fun startNativeSessionAsync(
        session: ManagedSession,
        request: SessionLaunchRequest,
        expectedGeneration: Int,
        oldHandleToStop: Long
    ) {
        val callback = createSessionCallback(session, expectedGeneration)
        ioExecutor.execute {
            if (oldHandleToStop != 0L) {
                runCatching { nativeAdapter.sessionStop(oldHandleToStop) }
            }
            if (session.closedLocally.get() || session.sessionGeneration.get() != expectedGeneration) {
                return@execute
            }

            val supportedCodecs = if (request.supportedCodecs >= 0) {
                request.supportedCodecs
            } else {
                nativeAdapter.probeSupportedCodecsMask()
            }
            val handle = runCatching {
                nativeAdapter.sessionStart(request, supportedCodecs, callback)
            }.getOrElse { err ->
                if (!session.closedLocally.get() && session.sessionGeneration.get() == expectedGeneration) {
                    session.updateState {
                        it.copy(
                            connectionState = SessionConnectionState.CLOSED,
                            lastError = err.message ?: "Failed to start session."
                        )
                    }
                }
                0L
            }

            if (handle == 0L) return@execute

            var shouldStopImmediately = false
            var startedListener: ((ManagedSession) -> Unit)? = null
            synchronized(lock) {
                if (session.closedLocally.get() ||
                    session.sessionGeneration.get() != expectedGeneration ||
                    _sessions.value.none { it.id == session.id }
                ) {
                    shouldStopImmediately = true
                } else {
                    session.updateState { it.copy(nativeHandle = handle) }
                    nativeAdapter.sessionSetAudioMuted(handle, session.effectiveAudioMuted)
                    if (session.id == _activeSessionId.value) {
                        startedListener = onSessionStarted
                    }
                }
            }

            if (shouldStopImmediately) {
                nativeAdapter.sessionStop(handle)
            } else {
                startedListener?.invoke(session)
            }
        }
    }

    fun switchTo(sessionId: String): Boolean {
        synchronized(lock) {
            return switchToLocked(sessionId)
        }
    }

    private fun switchToLocked(sessionId: String): Boolean {
        val target = _sessions.value.firstOrNull { it.id == sessionId } ?: return false
        val previous = activeSessionLocked()
        if (previous != null && previous.id != target.id) {
            demoteToBackgroundLocked(previous)
        }
        _activeSessionId.value = target.id
        if (foregroundActive) {
            promoteToForegroundLocked(target, foregroundSurface)
        } else {
            demoteToBackgroundLocked(target)
        }
        return true
    }

    fun attachForeground(surface: Surface?) {
        synchronized(lock) {
            foregroundActive = true
            foregroundSurface = surface
            val active = activeSessionLocked()
            for (session in _sessions.value) {
                if (session.id == active?.id) {
                    promoteToForegroundLocked(session, surface)
                } else {
                    demoteToBackgroundLocked(session)
                }
            }
        }
    }

    fun updateForegroundSurface(surface: Surface?) {
        synchronized(lock) {
            foregroundSurface = surface
            if (!foregroundActive) return
            val active = activeSessionLocked() ?: return
            active.videoDecoder.setSurface(surface)
        }
    }

    fun detachForeground() {
        synchronized(lock) {
            foregroundActive = false
            foregroundSurface = null
            for (session in _sessions.value) {
                demoteToBackgroundLocked(session)
            }
        }
    }

    fun closeSession(sessionId: String): String? {
        synchronized(lock) {
            return closeSessionLocked(sessionId)
        }
    }

    private fun closeSessionLocked(sessionId: String): String? {
        val list = _sessions.value
        val index = list.indexOfFirst { it.id == sessionId }
        if (index < 0) return _activeSessionId.value

        val removed = list[index]
        removed.closedLocally.set(true)
        val remaining = list.filterNot { it.id == sessionId }
        _sessions.value = remaining

        val handle = removed.nativeHandle
        removed.updateState {
            it.copy(
                nativeHandle = 0L,
                connectionState = SessionConnectionState.CLOSED,
                backgroundMuted = true
            )
        }
        removed.videoDecoder.setSurface(null)
        removed.audioPlayer.setMuted(true)
        if (handle != 0L) {
            nativeAdapter.sessionReleaseAll(handle)
            nativeAdapter.sessionSetAudioMuted(handle, true)
            ioExecutor.execute { nativeAdapter.sessionStop(handle) }
        }
        removed.videoDecoder.release()
        removed.audioPlayer.release()

        val wasActive = _activeSessionId.value == sessionId
        val nextActiveId = if (!wasActive) {
            _activeSessionId.value
        } else if (remaining.isEmpty()) {
            null
        } else {
            remaining[index.coerceAtMost(remaining.lastIndex)].id
        }
        _activeSessionId.value = nextActiveId

        if (wasActive && nextActiveId != null) {
            val nextSession = remaining.firstOrNull { it.id == nextActiveId }
            if (nextSession != null) {
                if (foregroundActive) {
                    promoteToForegroundLocked(nextSession, foregroundSurface)
                } else {
                    demoteToBackgroundLocked(nextSession)
                }
            }
        }
        return nextActiveId
    }

    fun setAudioMuted(sessionId: String, muted: Boolean) {
        synchronized(lock) {
            val session = _sessions.value.firstOrNull { it.id == sessionId } ?: return
            session.updateState { it.copy(audioMuted = muted) }
            val effectiveMuted = session.effectiveAudioMuted
            session.audioPlayer.setMuted(effectiveMuted)
            val handle = session.nativeHandle
            if (handle != 0L) {
                nativeAdapter.sessionSetAudioMuted(handle, effectiveMuted)
            }
        }
    }

    fun setBitrate(sessionId: String, bitrateMbps: Int) {
        synchronized(lock) {
            val session = _sessions.value.firstOrNull { it.id == sessionId } ?: return
            session.updateState { it.copy(bitrateMbps = bitrateMbps) }
            val handle = session.nativeHandle
            if (handle != 0L) {
                nativeAdapter.sessionSetBitrate(handle, bitrateMbps)
            }
        }
    }

    fun setFramePacingMode(sessionId: String, mode: VideoDecoder.FramePacingMode) {
        synchronized(lock) {
            val session = _sessions.value.firstOrNull { it.id == sessionId } ?: return
            session.updateState { it.copy(framePacingMode = mode) }
            session.videoDecoder.setFramePacingMode(mode)
        }
    }

    fun releaseActiveInputs() {
        val handle = activeSession?.nativeHandle ?: 0L
        if (handle != 0L) {
            nativeAdapter.sessionReleaseAll(handle)
        }
    }

    fun sendPointerMove(x: Int, y: Int) {
        val session = activeSession ?: return
        val handle = session.nativeHandle
        if (handle != 0L && session.canControl) {
            nativeAdapter.sessionSendPointerMove(handle, x, y)
        }
    }

    fun sendPointerButton(x: Int, y: Int, button: Int, down: Boolean) {
        val session = activeSession ?: return
        val handle = session.nativeHandle
        if (handle != 0L && session.canControl) {
            nativeAdapter.sessionSendPointerButton(handle, x, y, button, down)
        }
    }

    fun sendScroll(x: Int, y: Int, vertical: Int, horizontal: Int) {
        val session = activeSession ?: return
        val handle = session.nativeHandle
        if (handle != 0L && session.canControl) {
            nativeAdapter.sessionSendScroll(handle, x, y, vertical, horizontal)
        }
    }

    fun sendKey(vkCode: Int, down: Boolean) {
        val session = activeSession ?: return
        val handle = session.nativeHandle
        if (handle != 0L && session.canControl) {
            nativeAdapter.sessionSendKey(handle, vkCode, down)
        }
    }

    fun sendGamepad(
        slot: Int,
        connected: Boolean,
        buttons: Int,
        lt: Int,
        rt: Int,
        lx: Int,
        ly: Int,
        rx: Int,
        ry: Int
    ) {
        val session = activeSession ?: return
        val handle = session.nativeHandle
        if (handle != 0L && session.canControl) {
            nativeAdapter.sessionSendGamepad(handle, slot, connected, buttons, lt, rt, lx, ly, rx, ry)
        }
    }

    private fun activeSessionLocked(): ManagedSession? {
        val activeId = _activeSessionId.value ?: return null
        return _sessions.value.firstOrNull { it.id == activeId }
    }

    private fun promoteToForegroundLocked(session: ManagedSession, surface: Surface?) {
        session.updateState { it.copy(backgroundMuted = false) }
        session.videoDecoder.setSurface(surface)
        val effectiveMuted = session.effectiveAudioMuted
        session.audioPlayer.setMuted(effectiveMuted)
        val handle = session.nativeHandle
        if (handle != 0L) {
            nativeAdapter.sessionSetAudioMuted(handle, effectiveMuted)
        }
    }

    private fun demoteToBackgroundLocked(session: ManagedSession) {
        val handle = session.nativeHandle
        if (handle != 0L) {
            nativeAdapter.sessionReleaseAll(handle)
            nativeAdapter.sessionSetAudioMuted(handle, true)
        }
        session.updateState { it.copy(backgroundMuted = true) }
        session.videoDecoder.setSurface(null)
        session.audioPlayer.setMuted(true)
    }

    private fun createSessionCallback(
        session: ManagedSession,
        expectedGeneration: Int
    ): SessionCallback {
        fun isStale(): Boolean =
            session.closedLocally.get() || session.sessionGeneration.get() != expectedGeneration

        return object : SessionCallback {
            override fun onReady(viewOnly: Boolean, hdr: Boolean) {
                if (isStale()) return
                session.videoDecoder.setHdr(hdr)
                session.updateState {
                    it.copy(
                        connectionState = SessionConnectionState.CONNECTED,
                        canControl = !viewOnly,
                        hdr = hdr,
                        lastError = null
                    )
                }
            }

            override fun onVideoFrame(
                codec: Int,
                keyframe: Boolean,
                width: Int,
                height: Int,
                data: ByteArray,
                hostLatencyUs: Long
            ) {
                if (isStale()) return
                val containerChanged = width > 0 && height > 0 &&
                    (width != session.lastContainerWidth || height != session.lastContainerHeight)
                if (containerChanged) {
                    session.lastContainerWidth = width
                    session.lastContainerHeight = height
                    if (width != session.frameWidth || height != session.frameHeight) {
                        session.updateState { it.copy(frameWidth = width, frameHeight = height) }
                        val listener = synchronized(lock) { onFrameSizeChanged }
                        listener?.invoke(session.id, width, height)
                    }
                }
                session.videoDecoder.decodeFrame(codec, keyframe, width, height, data)
            }

            override fun onAudioPcm(pcm16Le: ByteArray, sampleRate: Int, channels: Int) {
                if (isStale() || session.effectiveAudioMuted) return
                session.audioPlayer.pushPcm(pcm16Le, sampleRate, channels)
            }

            override fun onStatsJson(json: String) {
                if (isStale()) return
                val parsed = JsonParsers.parseStats(json) ?: return
                val decodeMs = session.videoDecoder.averageDecodeMs
                session.updateState { it.copy(stats = parsed, decodeMs = decodeMs) }
            }

            override fun onClosed(error: String?) {
                if (isStale()) return
                session.updateState {
                    it.copy(
                        connectionState = SessionConnectionState.CLOSED,
                        lastError = error?.takeIf { msg -> msg.isNotBlank() }
                    )
                }
            }
        }
    }

    companion object {
        internal fun deriveSessionTitle(address: String): String {
            val trimmed = address.trim()
            if (trimmed.isEmpty()) return "Host"
            val withoutScheme = trimmed.substringAfter("://")
            val hostOnly = if (withoutScheme.startsWith("[")) {
                withoutScheme.substringBefore("]") + "]"
            } else {
                withoutScheme.substringBefore(":")
            }
            return hostOnly.ifBlank { trimmed }
        }
    }
}

object SessionManager {
    private val coordinator = SessionCoordinator(
        nativeAdapter = AndroidNativeAdapter(),
        videoFactory = { AndroidVideoEngine() },
        audioFactory = { AndroidAudioEngine() }
    )

    val sessions: StateFlow<List<ManagedSession>> get() = coordinator.sessions
    val activeSessionId: StateFlow<String?> get() = coordinator.activeSessionId
    val activeSession: ManagedSession? get() = coordinator.activeSession

    fun getSession(sessionId: String): ManagedSession? = coordinator.getSession(sessionId)

    fun setListeners(
        onFrameSizeChanged: ((sessionId: String, width: Int, height: Int) -> Unit)? = null,
        onSessionStarted: ((session: ManagedSession) -> Unit)? = null
    ) = coordinator.setListeners(onFrameSizeChanged, onSessionStarted)

    fun connectOrSwitch(request: SessionLaunchRequest): ConnectSessionResult =
        coordinator.connectOrSwitch(request)

    fun switchTo(sessionId: String): Boolean = coordinator.switchTo(sessionId)

    fun attachForeground(surface: Surface?) = coordinator.attachForeground(surface)

    fun updateForegroundSurface(surface: Surface?) = coordinator.updateForegroundSurface(surface)

    fun detachForeground() = coordinator.detachForeground()

    fun closeSession(sessionId: String): String? = coordinator.closeSession(sessionId)

    fun setAudioMuted(sessionId: String, muted: Boolean) =
        coordinator.setAudioMuted(sessionId, muted)

    fun setBitrate(sessionId: String, bitrateMbps: Int) =
        coordinator.setBitrate(sessionId, bitrateMbps)

    fun setFramePacingMode(sessionId: String, mode: VideoDecoder.FramePacingMode) =
        coordinator.setFramePacingMode(sessionId, mode)

    fun reconfigureStream(sessionId: String, width: Int, height: Int, maxFps: Int): Boolean =
        coordinator.reconfigureStream(sessionId, width, height, maxFps)

    fun releaseActiveInputs() = coordinator.releaseActiveInputs()

    fun sendPointerMove(x: Int, y: Int) = coordinator.sendPointerMove(x, y)

    fun sendPointerButton(x: Int, y: Int, button: Int, down: Boolean) =
        coordinator.sendPointerButton(x, y, button, down)

    fun sendScroll(x: Int, y: Int, vertical: Int, horizontal: Int) =
        coordinator.sendScroll(x, y, vertical, horizontal)

    fun sendKey(vkCode: Int, down: Boolean) = coordinator.sendKey(vkCode, down)

    fun sendGamepad(
        slot: Int,
        connected: Boolean,
        buttons: Int,
        lt: Int,
        rt: Int,
        lx: Int,
        ly: Int,
        rx: Int,
        ry: Int
    ) = coordinator.sendGamepad(slot, connected, buttons, lt, rt, lx, ly, rx, ry)
}
