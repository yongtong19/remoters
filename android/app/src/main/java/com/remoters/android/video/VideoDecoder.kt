package com.remoters.android.video

import android.media.MediaCodec
import android.media.MediaCodecList
import android.media.MediaFormat
import android.os.Build
import android.util.Log
import android.view.Surface
import java.util.ArrayDeque
import java.util.LinkedHashMap
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock

class VideoDecoder {

    enum class FramePacingMode {
        LOWEST_LATENCY,
        BALANCED
    }

    private val stateLock = Any()
    private var surface: Surface? = null
    private var requestedHdr: Boolean = false
    private var released: Boolean = false
    private var pendingKeyframe: EncodedFrame? = null
    private var activePipeline: DecoderPipeline? = null

    @Volatile
    var framePacingMode: FramePacingMode = FramePacingMode.LOWEST_LATENCY
        private set

    @Volatile
    var averageDecodeMs: Double = 0.0
        private set

    @Volatile
    private var onOutputFormatChanged: ((width: Int, height: Int) -> Unit)? = null

    fun setOnOutputFormatChangedListener(listener: ((width: Int, height: Int) -> Unit)?) {
        onOutputFormatChanged = listener
    }

    fun setFramePacingMode(mode: FramePacingMode) {
        framePacingMode = mode
    }

    fun setHdr(enabled: Boolean) {
        synchronized(stateLock) {
            requestedHdr = enabled
        }
    }

    fun setSurface(newSurface: Surface?) {
        val oldPipeline: DecoderPipeline?
        val newPipeline: DecoderPipeline?
        val replay: EncodedFrame?
        synchronized(stateLock) {
            if (released) return
            if (surface === newSurface) return
            surface = newSurface
            oldPipeline = activePipeline
            activePipeline = null
            if (newSurface != null && newSurface.isValid) {
                newPipeline = DecoderPipeline(newSurface)
                activePipeline = newPipeline
                replay = pendingKeyframe?.copyWithClonedData()
            } else {
                newPipeline = null
                replay = null
            }
        }

        // Shut down the previous pipeline outside stateLock so workers cannot deadlock on surface switches.
        oldPipeline?.shutdown()

        if (newPipeline != null) {
            newPipeline.start()
            if (replay != null) {
                newPipeline.enqueue(replay)
            }
        }
    }

    fun decodeFrame(
        codecType: Int,
        keyframe: Boolean,
        width: Int,
        height: Int,
        data: ByteArray,
        hdr: Boolean = requestedHdr
    ) {
        if (width <= 0 || height <= 0 || data.isEmpty()) return

        val nowNs = System.nanoTime()
        val pipeline: DecoderPipeline?
        val frameCopy = data.copyOf()
        val frame = EncodedFrame(
            codecType = codecType,
            keyframe = keyframe,
            width = width,
            height = height,
            hdr = hdr,
            data = frameCopy,
            receivedNs = nowNs
        )

        synchronized(stateLock) {
            if (released) return
            if (keyframe) {
                pendingKeyframe = frame.copyWithClonedData()
            }
            val activeSurface = surface
            pipeline = if (activeSurface != null && activeSurface.isValid) activePipeline else null
        }

        // Non-blocking enqueue so MediaCodec never stalls the Rust QUIC/JNI thread.
        pipeline?.enqueue(frame)
    }

    fun release() {
        val oldPipeline: DecoderPipeline?
        synchronized(stateLock) {
            released = true
            surface = null
            pendingKeyframe = null
            requestedHdr = false
            oldPipeline = activePipeline
            activePipeline = null
        }
        oldPipeline?.shutdown()
    }

    private fun recordDecodeLatencyMs(elapsedMs: Double) {
        val prev = averageDecodeMs
        averageDecodeMs = if (prev <= 0.0) elapsedMs else prev * 0.8 + elapsedMs * 0.2
    }

    private inner class DecoderPipeline(private val targetSurface: Surface) {
        private val inputQueue = FrameInputQueue(INPUT_QUEUE_CAPACITY)
        private val ptsClock = MonotonicPtsClock()
        private val timingTracker = DecodeTimingTracker(MAX_TRACKED_SUBMIT_TIMES)
        private val stopping = AtomicBoolean(false)

        private var codec: MediaCodec? = null
        private var currentCodecType: Int = -1
        private var currentWidth: Int = 0
        private var currentHeight: Int = 0
        private var currentHdr: Boolean = false
        private var awaitingKeyframe: Boolean = true

        @Volatile
        private var outputRunning: AtomicBoolean? = null
        private var outputThread: Thread? = null
        @Volatile
        private var inputThread: Thread? = null

        fun start() {
            val thread = Thread({ runInputLoop() }, "VideoDecodeInput").apply {
                isDaemon = true
                priority = Thread.MAX_PRIORITY - 1
            }
            inputThread = thread
            thread.start()
        }

        fun enqueue(frame: EncodedFrame) {
            if (stopping.get()) return
            inputQueue.enqueue(frame)
        }

        fun shutdown() {
            if (!stopping.compareAndSet(false, true)) {
                return
            }
            inputQueue.close()
            outputRunning?.set(false)
            val thread = inputThread
            if (thread != null && thread !== Thread.currentThread()) {
                try {
                    thread.join(SHUTDOWN_JOIN_TIMEOUT_MS)
                } catch (_: InterruptedException) {
                    Thread.currentThread().interrupt()
                }
                if (thread.isAlive) {
                    thread.interrupt()
                }
            }
        }

        private fun runInputLoop() {
            try {
                while (!stopping.get()) {
                    val frame = inputQueue.pollBlocking() ?: break
                    if (stopping.get() || !targetSurface.isValid) break
                    feedFrame(frame)
                }
            } finally {
                releaseCodec()
            }
        }

        private fun feedFrame(frame: EncodedFrame) {
            val needsReconfigure =
                codec == null ||
                    currentCodecType != frame.codecType ||
                    currentWidth != frame.width ||
                    currentHeight != frame.height ||
                    currentHdr != frame.hdr

            if (needsReconfigure) {
                if (!frame.keyframe) {
                    awaitingKeyframe = true
                    return
                }
                if (!configureCodec(frame.codecType, frame.width, frame.height, frame.hdr)) {
                    awaitingKeyframe = true
                    return
                }
            }

            if (awaitingKeyframe && !frame.keyframe) {
                return
            }
            if (frame.keyframe) {
                awaitingKeyframe = false
            }

            val activeCodec = codec ?: return
            try {
                val inputIndex = activeCodec.dequeueInputBuffer(INPUT_TIMEOUT_US)
                if (inputIndex < 0) {
                    return
                }
                val inputBuffer = activeCodec.getInputBuffer(inputIndex) ?: return
                inputBuffer.clear()
                if (inputBuffer.capacity() < frame.data.size) {
                    activeCodec.queueInputBuffer(inputIndex, 0, 0, 0L, 0)
                    return
                }
                inputBuffer.put(frame.data)
                val ptsUs = ptsClock.nextPtsUs()
                timingTracker.recordSubmit(ptsUs, System.nanoTime())
                val flags = if (frame.keyframe) MediaCodec.BUFFER_FLAG_KEY_FRAME else 0
                activeCodec.queueInputBuffer(inputIndex, 0, frame.data.size, ptsUs, flags)
            } catch (e: Exception) {
                if (!stopping.get()) {
                    Log.w(TAG, "MediaCodec input error, resetting decoder", e)
                }
                releaseCodec()
                awaitingKeyframe = true
            }
        }

        private fun configureCodec(
            codecType: Int,
            width: Int,
            height: Int,
            hdr: Boolean
        ): Boolean {
            releaseCodec()
            val mime = mimeForCodec(codecType)
            return try {
                val attempt = configureWithLowLatencyFallback(
                    configureLowLatency = {
                        createAndStartCodec(
                            mime = mime,
                            width = width,
                            height = height,
                            hdr = hdr,
                            includeVendorAndOperatingRate = true
                        )
                    },
                    configureFallback = {
                        createAndStartCodec(
                            mime = mime,
                            width = width,
                            height = height,
                            hdr = hdr,
                            includeVendorAndOperatingRate = false
                        )
                    },
                    onLowLatencyFailure = { error ->
                        Log.w(
                            TAG,
                            "Low-latency vendor/operating-rate MediaCodec config rejected for $mime ${width}x$height, retrying standard config",
                            error
                        )
                    }
                )
                val instance = attempt.value
                codec = instance
                currentCodecType = codecType
                currentWidth = width
                currentHeight = height
                currentHdr = hdr
                awaitingKeyframe = false
                startOutputThread(instance)
                true
            } catch (e: Exception) {
                Log.e(TAG, "Failed to configure $mime decoder for ${width}x$height (hdr=$hdr)", e)
                releaseCodec()
                false
            }
        }

        private fun createAndStartCodec(
            mime: String,
            width: Int,
            height: Int,
            hdr: Boolean,
            includeVendorAndOperatingRate: Boolean
        ): MediaCodec {
            val format = buildMediaFormat(
                mime = mime,
                width = width,
                height = height,
                hdr = hdr,
                includeVendorAndOperatingRate = includeVendorAndOperatingRate
            )
            val instance = createDecoderForMime(mime)
            try {
                instance.configure(format, targetSurface, null, 0)
                instance.setVideoScalingMode(MediaCodec.VIDEO_SCALING_MODE_SCALE_TO_FIT)
                instance.start()
                return instance
            } catch (e: Exception) {
                safeReleaseCodec(instance)
                throw e
            }
        }

        private fun startOutputThread(activeCodec: MediaCodec) {
            val running = AtomicBoolean(true)
            outputRunning = running
            val thread = Thread({ runOutputLoop(activeCodec, running) }, "VideoDecodeOutput").apply {
                isDaemon = true
                priority = Thread.MAX_PRIORITY - 1
            }
            outputThread = thread
            thread.start()
        }

        private fun stopOutputThread() {
            val running = outputRunning
            val thread = outputThread
            outputRunning = null
            outputThread = null
            running?.set(false)
            if (thread != null && thread !== Thread.currentThread()) {
                try {
                    thread.join(SHUTDOWN_JOIN_TIMEOUT_MS)
                } catch (_: InterruptedException) {
                    Thread.currentThread().interrupt()
                }
                if (thread.isAlive) {
                    thread.interrupt()
                }
            }
        }

        private fun runOutputLoop(activeCodec: MediaCodec, running: AtomicBoolean) {
            val info = MediaCodec.BufferInfo()
            val readyBuffers = ArrayList<ReadyOutputBuffer>(4)
            while (running.get() && !stopping.get()) {
                try {
                    val firstIndex = activeCodec.dequeueOutputBuffer(info, OUTPUT_POLL_TIMEOUT_US)
                    if (firstIndex == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                        notifyOutputFormat(activeCodec)
                        continue
                    }
                    if (firstIndex < 0) {
                        continue
                    }
                    readyBuffers.clear()
                    readyBuffers.add(ReadyOutputBuffer(firstIndex, info.size, info.presentationTimeUs))

                    // Drain any additional immediately-ready frames so frame pacing can drop stale ones.
                    while (running.get() && !stopping.get()) {
                        val nextIndex = activeCodec.dequeueOutputBuffer(info, 0L)
                        if (nextIndex >= 0) {
                            readyBuffers.add(
                                ReadyOutputBuffer(nextIndex, info.size, info.presentationTimeUs)
                            )
                        } else if (nextIndex == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                            notifyOutputFormat(activeCodec)
                        } else {
                            break
                        }
                    }

                    val decisions = selectRenderDecisions(readyBuffers.size, framePacingMode)
                    for (i in readyBuffers.indices) {
                        val buf = readyBuffers[i]
                        val render = decisions[i] && buf.size > 0
                        val elapsedMs = timingTracker.complete(buf.ptsUs, System.nanoTime())
                        activeCodec.releaseOutputBuffer(buf.index, render)
                        if (render && elapsedMs != null) {
                            recordDecodeLatencyMs(elapsedMs)
                        }
                    }
                } catch (e: Exception) {
                    if (running.get() && !stopping.get()) {
                        Log.w(TAG, "MediaCodec output drain error", e)
                    }
                    break
                }
            }
        }

        private fun notifyOutputFormat(activeCodec: MediaCodec) {
            val outputFormat = runCatching { activeCodec.outputFormat }.getOrNull() ?: return
            val dims = extractOutputFormatDimensions(outputFormat) ?: return
            onOutputFormatChanged?.invoke(dims.first, dims.second)
        }

        private fun releaseCodec() {
            stopOutputThread()
            timingTracker.clear()
            val existing = codec
            codec = null
            currentCodecType = -1
            currentWidth = 0
            currentHeight = 0
            currentHdr = false
            if (existing != null) {
                safeReleaseCodec(existing)
            }
        }
    }

    private fun buildMediaFormat(
        mime: String,
        width: Int,
        height: Int,
        hdr: Boolean,
        includeVendorAndOperatingRate: Boolean
    ): MediaFormat {
        return MediaFormat.createVideoFormat(mime, width, height).apply {
            val integerKeys = buildLowLatencyIntegerKeys(
                apiLevel = Build.VERSION.SDK_INT,
                includeVendorAndOperatingRate = includeVendorAndOperatingRate
            )
            for ((key, value) in integerKeys) {
                setInteger(key, value)
            }
            setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, width * height)
            if (hdr) {
                setInteger(MediaFormat.KEY_COLOR_STANDARD, MediaFormat.COLOR_STANDARD_BT2020)
                setInteger(MediaFormat.KEY_COLOR_TRANSFER, MediaFormat.COLOR_TRANSFER_ST2084)
                setInteger(MediaFormat.KEY_COLOR_RANGE, MediaFormat.COLOR_RANGE_LIMITED)
            }
        }
    }

    private fun createDecoderForMime(mime: String): MediaCodec {
        val preferredName = selectPreferredDecoderName(mime, listDecoderCandidates())
        if (preferredName != null) {
            try {
                return MediaCodec.createByCodecName(preferredName)
            } catch (e: Exception) {
                Log.w(TAG, "Failed to open preferred decoder $preferredName for $mime, falling back", e)
            }
        }
        return MediaCodec.createDecoderByType(mime)
    }

    private fun safeReleaseCodec(instance: MediaCodec) {
        try {
            instance.stop()
        } catch (_: Exception) {
        }
        try {
            instance.release()
        } catch (_: Exception) {
        }
    }

    internal data class EncodedFrame(
        val codecType: Int,
        val keyframe: Boolean,
        val width: Int,
        val height: Int,
        val hdr: Boolean,
        val data: ByteArray,
        val receivedNs: Long = 0L
    ) {
        fun copyWithClonedData(): EncodedFrame = copy(data = data.copyOf())
    }

    private data class ReadyOutputBuffer(
        val index: Int,
        val size: Int,
        val ptsUs: Long
    )

    internal class FrameInputQueue(capacity: Int = INPUT_QUEUE_CAPACITY) {
        private val maxCapacity = capacity.coerceAtLeast(1)
        private val lock = ReentrantLock()
        private val available = lock.newCondition()
        private val deque = ArrayDeque<EncodedFrame>(maxCapacity)
        private var closed = false

        fun enqueue(frame: EncodedFrame): Int = lock.withLock {
            if (closed) return 0
            var dropped = 0
            if (frame.keyframe) {
                if (deque.isNotEmpty()) {
                    dropped = deque.size
                    deque.clear()
                }
                deque.addLast(frame)
            } else {
                if (deque.size >= maxCapacity) {
                    // Preserve a pending keyframe at head so decoder startup is not corrupted.
                    if (deque.peekFirst()?.keyframe == true) {
                        dropped = 1
                        return@withLock dropped
                    }
                    deque.removeFirst()
                    dropped = 1
                }
                deque.addLast(frame)
            }
            available.signal()
            dropped
        }

        fun pollBlocking(): EncodedFrame? = lock.withLock {
            while (deque.isEmpty() && !closed) {
                try {
                    available.await()
                } catch (_: InterruptedException) {
                    Thread.currentThread().interrupt()
                    return null
                }
            }
            if (closed && deque.isEmpty()) return null
            deque.pollFirst()
        }

        fun pollNow(): EncodedFrame? = lock.withLock {
            deque.pollFirst()
        }

        fun size(): Int = lock.withLock { deque.size }

        fun close() = lock.withLock {
            closed = true
            deque.clear()
            available.signalAll()
        }
    }

    internal class MonotonicPtsClock(
        private val nanoTimeProvider: () -> Long = System::nanoTime
    ) {
        private val lock = Any()
        private var lastPtsUs: Long = 0L

        fun nextPtsUs(): Long = synchronized(lock) {
            val candidate = (nanoTimeProvider() / 1_000L).coerceAtLeast(1L)
            val next = if (candidate > lastPtsUs) candidate else lastPtsUs + 1L
            lastPtsUs = next
            next
        }
    }

    internal class DecodeTimingTracker(private val maxEntries: Int = MAX_TRACKED_SUBMIT_TIMES) {
        private val lock = Any()
        private val submitTimesNs = LinkedHashMap<Long, Long>()

        fun recordSubmit(ptsUs: Long, nowNs: Long) = synchronized(lock) {
            submitTimesNs[ptsUs] = nowNs
            while (submitTimesNs.size > maxEntries) {
                val oldestKey = submitTimesNs.entries.iterator().next().key
                submitTimesNs.remove(oldestKey)
            }
        }

        fun complete(ptsUs: Long, nowNs: Long): Double? = synchronized(lock) {
            val submitted = submitTimesNs.remove(ptsUs) ?: return null
            val elapsedNs = (nowNs - submitted).coerceAtLeast(0L)
            elapsedNs / 1_000_000.0
        }

        fun size(): Int = synchronized(lock) { submitTimesNs.size }

        fun clear() = synchronized(lock) {
            submitTimesNs.clear()
        }
    }

    internal data class ConfigAttemptResult<T>(
        val value: T,
        val usedFallback: Boolean
    )

    internal data class DecoderCandidate(
        val name: String,
        val supportedTypes: List<String>,
        val isHardwareAccelerated: Boolean,
        val isSoftwareOnly: Boolean
    )

    companion object {
        private const val TAG = "VideoDecoder"
        const val CODEC_H264 = 0
        const val CODEC_HEVC = 1
        const val CODEC_AV1 = 2

        const val CODEC_BIT_H264 = 1
        const val CODEC_BIT_HEVC = 2
        const val CODEC_BIT_AV1 = 4

        const val MIME_H264 = MediaFormat.MIMETYPE_VIDEO_AVC
        const val MIME_HEVC = MediaFormat.MIMETYPE_VIDEO_HEVC
        const val MIME_AV1 = MediaFormat.MIMETYPE_VIDEO_AV1

        internal const val KEY_LOW_LATENCY_NAME = "low-latency"
        internal const val KEY_PRIORITY_NAME = "priority"
        internal const val KEY_OPERATING_RATE_NAME = "operating-rate"
        internal const val KEY_VENDOR_QTI_LOW_LATENCY = "vendor.qti-ext-dec-low-latency.enable"
        internal const val KEY_VENDOR_RTC_LOW_LATENCY = "vendor.rtc-ext-dec-low-latency.enable"
        internal const val KEY_VENDOR_GENERIC_LOW_LATENCY = "vendor.low-latency.enable"
        internal const val KEY_CROP_LEFT = "crop-left"
        internal const val KEY_CROP_TOP = "crop-top"
        internal const val KEY_CROP_RIGHT = "crop-right"
        internal const val KEY_CROP_BOTTOM = "crop-bottom"

        internal fun resolveOutputFormatDimensions(
            formatWidth: Int,
            formatHeight: Int,
            cropLeft: Int? = null,
            cropTop: Int? = null,
            cropRight: Int? = null,
            cropBottom: Int? = null
        ): Pair<Int, Int>? {
            val hasCrop = cropLeft != null &&
                cropTop != null &&
                cropRight != null &&
                cropBottom != null &&
                cropRight >= cropLeft &&
                cropBottom >= cropTop
            val width = if (hasCrop) cropRight!! - cropLeft!! + 1 else formatWidth
            val height = if (hasCrop) cropBottom!! - cropTop!! + 1 else formatHeight
            return if (width > 0 && height > 0) width to height else null
        }

        internal fun extractOutputFormatDimensions(format: MediaFormat): Pair<Int, Int>? {
            val formatW = if (format.containsKey(MediaFormat.KEY_WIDTH)) {
                format.getInteger(MediaFormat.KEY_WIDTH)
            } else {
                0
            }
            val formatH = if (format.containsKey(MediaFormat.KEY_HEIGHT)) {
                format.getInteger(MediaFormat.KEY_HEIGHT)
            } else {
                0
            }
            val cropLeft = if (format.containsKey(KEY_CROP_LEFT)) format.getInteger(KEY_CROP_LEFT) else null
            val cropTop = if (format.containsKey(KEY_CROP_TOP)) format.getInteger(KEY_CROP_TOP) else null
            val cropRight = if (format.containsKey(KEY_CROP_RIGHT)) format.getInteger(KEY_CROP_RIGHT) else null
            val cropBottom = if (format.containsKey(KEY_CROP_BOTTOM)) format.getInteger(KEY_CROP_BOTTOM) else null
            return resolveOutputFormatDimensions(
                formatWidth = formatW,
                formatHeight = formatH,
                cropLeft = cropLeft,
                cropTop = cropTop,
                cropRight = cropRight,
                cropBottom = cropBottom
            )
        }

        internal const val INPUT_QUEUE_CAPACITY = 2
        internal const val BALANCED_MAX_BACKLOG = 2
        internal const val MAX_TRACKED_SUBMIT_TIMES = 64

        private const val INPUT_TIMEOUT_US = 8_000L
        private const val OUTPUT_POLL_TIMEOUT_US = 5_000L
        private const val SHUTDOWN_JOIN_TIMEOUT_MS = 300L

        private val SOFTWARE_CODEC_PREFIXES = listOf(
            "c2.android.",
            "omx.google.",
            "omx.ffmpeg.",
            "c2.vpx."
        )

        fun probeSupportedCodecsMask(): Int = probeSupportedCodecsMask(listDecoderCandidates())

        internal fun buildLowLatencyIntegerKeys(
            apiLevel: Int,
            includeVendorAndOperatingRate: Boolean
        ): Map<String, Int> {
            val keys = LinkedHashMap<String, Int>()
            if (apiLevel >= Build.VERSION_CODES.R) {
                keys[KEY_LOW_LATENCY_NAME] = 1
            }
            keys[KEY_PRIORITY_NAME] = 0
            if (includeVendorAndOperatingRate) {
                keys[KEY_OPERATING_RATE_NAME] = Short.MAX_VALUE.toInt()
                keys[KEY_VENDOR_QTI_LOW_LATENCY] = 1
                keys[KEY_VENDOR_RTC_LOW_LATENCY] = 1
                keys[KEY_VENDOR_GENERIC_LOW_LATENCY] = 1
            }
            return keys
        }

        internal fun <T> configureWithLowLatencyFallback(
            configureLowLatency: () -> T,
            configureFallback: () -> T,
            onLowLatencyFailure: (Exception) -> Unit = {}
        ): ConfigAttemptResult<T> {
            return try {
                ConfigAttemptResult(value = configureLowLatency(), usedFallback = false)
            } catch (e: Exception) {
                onLowLatencyFailure(e)
                ConfigAttemptResult(value = configureFallback(), usedFallback = true)
            }
        }

        internal fun selectRenderDecisions(
            readyCount: Int,
            mode: FramePacingMode
        ): List<Boolean> {
            if (readyCount <= 0) return emptyList()
            return when (mode) {
                FramePacingMode.LOWEST_LATENCY -> {
                    List(readyCount) { index -> index == readyCount - 1 }
                }
                FramePacingMode.BALANCED -> {
                    val dropCount = (readyCount - BALANCED_MAX_BACKLOG).coerceAtLeast(0)
                    List(readyCount) { index -> index >= dropCount }
                }
            }
        }

        internal fun mimeForCodec(codecType: Int): String = when (codecType) {
            CODEC_AV1 -> MIME_AV1
            CODEC_HEVC -> MIME_HEVC
            else -> MIME_H264
        }

        internal fun codecBitForMime(mime: String): Int = when {
            mime.equals(MIME_H264, ignoreCase = true) -> CODEC_BIT_H264
            mime.equals(MIME_HEVC, ignoreCase = true) -> CODEC_BIT_HEVC
            mime.equals(MIME_AV1, ignoreCase = true) -> CODEC_BIT_AV1
            else -> 0
        }

        internal fun isKnownSoftwareCodecName(name: String): Boolean {
            val lower = name.lowercase()
            return SOFTWARE_CODEC_PREFIXES.any { lower.startsWith(it) }
        }

        internal fun probeSupportedCodecsMask(candidates: List<DecoderCandidate>): Int {
            var mask = 0
            for (candidate in candidates) {
                if (!candidate.isHardwareAccelerated || candidate.isSoftwareOnly) continue
                if (isKnownSoftwareCodecName(candidate.name)) continue
                for (mime in candidate.supportedTypes) {
                    mask = mask or codecBitForMime(mime)
                }
            }
            return mask
        }

        internal fun selectPreferredDecoderName(
            mime: String,
            candidates: List<DecoderCandidate>
        ): String? {
            val hardwareMatches = candidates.filter { candidate ->
                candidate.isHardwareAccelerated &&
                    !candidate.isSoftwareOnly &&
                    !isKnownSoftwareCodecName(candidate.name) &&
                    candidate.supportedTypes.any { it.equals(mime, ignoreCase = true) }
            }
            return hardwareMatches.firstOrNull {
                it.name.contains(".low_latency", ignoreCase = true)
            }?.name ?: hardwareMatches.firstOrNull()?.name
        }

        private fun listDecoderCandidates(): List<DecoderCandidate> {
            val infos = runCatching {
                MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos
            }.getOrNull() ?: return emptyList()

            return infos.mapNotNull { info ->
                if (info.isEncoder) return@mapNotNull null
                val isSoftwareOnly = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                    info.isSoftwareOnly
                } else {
                    isKnownSoftwareCodecName(info.name)
                }
                val isHardware = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                    info.isHardwareAccelerated && !isSoftwareOnly
                } else {
                    !isSoftwareOnly
                }
                DecoderCandidate(
                    name = info.name,
                    supportedTypes = info.supportedTypes.toList(),
                    isHardwareAccelerated = isHardware,
                    isSoftwareOnly = isSoftwareOnly
                )
            }
        }
    }
}
