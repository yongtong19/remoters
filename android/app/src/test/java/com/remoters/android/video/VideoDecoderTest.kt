package com.remoters.android.video

import com.remoters.android.ui.JsonParsers
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class VideoDecoderTest {

    @Test
    fun codecConstantsAndMimeMappingsMatchProtocol() {
        assertEquals(0, VideoDecoder.CODEC_H264)
        assertEquals(1, VideoDecoder.CODEC_HEVC)
        assertEquals(2, VideoDecoder.CODEC_AV1)

        assertEquals(1, VideoDecoder.CODEC_BIT_H264)
        assertEquals(2, VideoDecoder.CODEC_BIT_HEVC)
        assertEquals(4, VideoDecoder.CODEC_BIT_AV1)

        assertEquals("video/avc", VideoDecoder.MIME_H264)
        assertEquals("video/hevc", VideoDecoder.MIME_HEVC)
        assertEquals("video/av01", VideoDecoder.MIME_AV1)

        assertEquals(VideoDecoder.MIME_H264, VideoDecoder.mimeForCodec(VideoDecoder.CODEC_H264))
        assertEquals(VideoDecoder.MIME_HEVC, VideoDecoder.mimeForCodec(VideoDecoder.CODEC_HEVC))
        assertEquals(VideoDecoder.MIME_AV1, VideoDecoder.mimeForCodec(VideoDecoder.CODEC_AV1))

        assertEquals(VideoDecoder.CODEC_BIT_H264, VideoDecoder.codecBitForMime("video/avc"))
        assertEquals(VideoDecoder.CODEC_BIT_HEVC, VideoDecoder.codecBitForMime("video/hevc"))
        assertEquals(VideoDecoder.CODEC_BIT_AV1, VideoDecoder.codecBitForMime("video/av01"))
        assertEquals(0, VideoDecoder.codecBitForMime("video/x-vnd.on2.vp8"))
    }

    @Test
    fun probesHardwareCodecBitmaskAndExcludesSoftwareDecoders() {
        val xiaomi17Candidates = listOf(
            VideoDecoder.DecoderCandidate(
                name = "c2.qti.avc.decoder.low_latency",
                supportedTypes = listOf("video/avc"),
                isHardwareAccelerated = true,
                isSoftwareOnly = false
            ),
            VideoDecoder.DecoderCandidate(
                name = "c2.qti.hevc.decoder.low_latency",
                supportedTypes = listOf("video/hevc"),
                isHardwareAccelerated = true,
                isSoftwareOnly = false
            ),
            VideoDecoder.DecoderCandidate(
                name = "c2.qti.av1.decoder.low_latency",
                supportedTypes = listOf("video/av01"),
                isHardwareAccelerated = true,
                isSoftwareOnly = false
            ),
            VideoDecoder.DecoderCandidate(
                name = "c2.android.av1.decoder",
                supportedTypes = listOf("video/av01"),
                isHardwareAccelerated = false,
                isSoftwareOnly = true
            )
        )

        val fullMask = VideoDecoder.probeSupportedCodecsMask(xiaomi17Candidates)
        assertEquals(
            VideoDecoder.CODEC_BIT_H264 or VideoDecoder.CODEC_BIT_HEVC or VideoDecoder.CODEC_BIT_AV1,
            fullMask
        )

        val softwareAv1Only = listOf(
            VideoDecoder.DecoderCandidate(
                name = "c2.qti.avc.decoder",
                supportedTypes = listOf("video/avc"),
                isHardwareAccelerated = true,
                isSoftwareOnly = false
            ),
            VideoDecoder.DecoderCandidate(
                name = "c2.android.av1.decoder",
                supportedTypes = listOf("video/av01"),
                isHardwareAccelerated = true,
                isSoftwareOnly = false
            )
        )
        assertEquals(
            VideoDecoder.CODEC_BIT_H264,
            VideoDecoder.probeSupportedCodecsMask(softwareAv1Only)
        )
    }

    @Test
    fun prefersHardwareLowLatencyDecoderAndIgnoresSoftwareDecoders() {
        val candidates = listOf(
            VideoDecoder.DecoderCandidate(
                name = "c2.android.av1.decoder",
                supportedTypes = listOf("video/av01"),
                isHardwareAccelerated = false,
                isSoftwareOnly = true
            ),
            VideoDecoder.DecoderCandidate(
                name = "c2.qti.av1.decoder",
                supportedTypes = listOf("video/av01"),
                isHardwareAccelerated = true,
                isSoftwareOnly = false
            ),
            VideoDecoder.DecoderCandidate(
                name = "c2.qti.av1.decoder.low_latency",
                supportedTypes = listOf("video/av01"),
                isHardwareAccelerated = true,
                isSoftwareOnly = false
            )
        )

        assertEquals(
            "c2.qti.av1.decoder.low_latency",
            VideoDecoder.selectPreferredDecoderName("video/av01", candidates)
        )
        assertNull(VideoDecoder.selectPreferredDecoderName("video/hevc", candidates))
    }

    @Test
    fun parsesAv1AndHdrFromStatsJson() {
        val hdrStats = JsonParsers.parseStats(
            """{"ok":true,"width":2400,"height":1080,"targetFps":120,"codec":"AV1","hdr":true,"fps":119.5,"videoBps":45000000.0,"rttMs":2.4,"encodeMs":1.8,"e2eMs":7.1,"lostPackets":0}"""
        )
        assertNotNull(hdrStats)
        assertEquals("AV1", hdrStats!!.codec)
        assertTrue(hdrStats.hdr)
        assertEquals(2400, hdrStats.width)

        val sdrStats = JsonParsers.parseStats(
            """{"ok":true,"width":1920,"height":1080,"targetFps":60,"codec":"H.264","fps":60.0,"videoBps":12000000.0,"rttMs":4.0,"encodeMs":2.0,"e2eMs":8.0,"lostPackets":0}"""
        )
        assertNotNull(sdrStats)
        assertEquals("H.264", sdrStats!!.codec)
        assertFalse(sdrStats.hdr)
    }

    @Test
    fun frameInputQueueFlushesStaleFramesOnKeyframeAndCapsBacklog() {
        val queue = VideoDecoder.FrameInputQueue(capacity = 2)
        fun frame(id: Byte, keyframe: Boolean) = VideoDecoder.EncodedFrame(
            codecType = VideoDecoder.CODEC_H264,
            keyframe = keyframe,
            width = 1920,
            height = 1080,
            hdr = false,
            data = byteArrayOf(id)
        )

        // Fill with 2 P-frames; a 3rd P-frame evicts the oldest P-frame.
        assertEquals(0, queue.enqueue(frame(1, keyframe = false)))
        assertEquals(0, queue.enqueue(frame(2, keyframe = false)))
        assertEquals(1, queue.enqueue(frame(3, keyframe = false)))
        assertEquals(2, queue.size())

        // Keyframe arrival immediately flushes both obsolete queued P-frames.
        assertEquals(2, queue.enqueue(frame(4, keyframe = true)))
        assertEquals(1, queue.size())
        val kf = queue.pollNow()
        assertNotNull(kf)
        assertTrue(kf!!.keyframe)
        assertEquals(4.toByte(), kf.data[0])
        assertNull(queue.pollNow())

        // Pending keyframe at head is preserved when P-frames overflow capacity.
        assertEquals(0, queue.enqueue(frame(10, keyframe = true)))
        assertEquals(0, queue.enqueue(frame(11, keyframe = false)))
        assertEquals(1, queue.enqueue(frame(12, keyframe = false)))
        assertEquals(10.toByte(), queue.pollNow()!!.data[0])
        assertEquals(11.toByte(), queue.pollNow()!!.data[0])
        assertNull(queue.pollNow())
    }

    @Test
    fun monotonicPtsClockIsStrictlyIncreasingAcrossHighRefreshAndClockStalls() {
        val timestampsNs = longArrayOf(
            10_000_000L, // 10_000 us
            18_333_000L, // ~120Hz (+8.333ms)
            18_333_000L, // same timestamp -> must bump +1us
            15_000_000L, // backwards clock -> must still bump +1us
            26_666_000L  // next 120Hz tick
        )
        var idx = 0
        val clock = VideoDecoder.MonotonicPtsClock { timestampsNs[idx++] }

        val p0 = clock.nextPtsUs()
        val p1 = clock.nextPtsUs()
        val p2 = clock.nextPtsUs()
        val p3 = clock.nextPtsUs()
        val p4 = clock.nextPtsUs()

        assertEquals(10_000L, p0)
        assertEquals(18_333L, p1)
        assertEquals(18_334L, p2)
        assertEquals(18_335L, p3)
        assertEquals(26_666L, p4)
    }

    @Test
    fun decodeTimingTrackerCapsEntriesAndPreventsLeaksOnDroppedFrames() {
        val tracker = VideoDecoder.DecodeTimingTracker(maxEntries = 4)
        for (i in 1L..10L) {
            tracker.recordSubmit(ptsUs = i * 1_000L, nowNs = i * 1_000_000L)
        }
        assertEquals(4, tracker.size())
        // Earliest entries (1..6) were pruned.
        assertNull(tracker.complete(ptsUs = 1_000L, nowNs = 20_000_000L))
        assertNull(tracker.complete(ptsUs = 6_000L, nowNs = 20_000_000L))
        // Recent entry (10) is retained and computes accurate decode duration.
        val elapsedMs = tracker.complete(ptsUs = 10_000L, nowNs = 12_500_000L)
        assertNotNull(elapsedMs)
        assertEquals(2.5, elapsedMs!!, 0.001)
        assertEquals(3, tracker.size())
    }

    @Test
    fun vendorLowLatencyKeysAndFallbackHelperBehaveCorrectly() {
        val fullKeysApi30 = VideoDecoder.buildLowLatencyIntegerKeys(
            apiLevel = 30,
            includeVendorAndOperatingRate = true
        )
        assertEquals(1, fullKeysApi30[VideoDecoder.KEY_LOW_LATENCY_NAME])
        assertEquals(0, fullKeysApi30[VideoDecoder.KEY_PRIORITY_NAME])
        assertEquals(Short.MAX_VALUE.toInt(), fullKeysApi30[VideoDecoder.KEY_OPERATING_RATE_NAME])
        assertEquals(1, fullKeysApi30[VideoDecoder.KEY_VENDOR_QTI_LOW_LATENCY])
        assertEquals(1, fullKeysApi30[VideoDecoder.KEY_VENDOR_RTC_LOW_LATENCY])
        assertEquals(1, fullKeysApi30[VideoDecoder.KEY_VENDOR_GENERIC_LOW_LATENCY])

        val fallbackKeysApi30 = VideoDecoder.buildLowLatencyIntegerKeys(
            apiLevel = 30,
            includeVendorAndOperatingRate = false
        )
        assertEquals(1, fallbackKeysApi30[VideoDecoder.KEY_LOW_LATENCY_NAME])
        assertEquals(0, fallbackKeysApi30[VideoDecoder.KEY_PRIORITY_NAME])
        assertFalse(fallbackKeysApi30.containsKey(VideoDecoder.KEY_OPERATING_RATE_NAME))
        assertFalse(fallbackKeysApi30.containsKey(VideoDecoder.KEY_VENDOR_QTI_LOW_LATENCY))
        assertFalse(fallbackKeysApi30.containsKey(VideoDecoder.KEY_VENDOR_RTC_LOW_LATENCY))
        assertFalse(fallbackKeysApi30.containsKey(VideoDecoder.KEY_VENDOR_GENERIC_LOW_LATENCY))

        val keysApi29 = VideoDecoder.buildLowLatencyIntegerKeys(
            apiLevel = 29,
            includeVendorAndOperatingRate = true
        )
        assertFalse(keysApi29.containsKey(VideoDecoder.KEY_LOW_LATENCY_NAME))
        assertEquals(0, keysApi29[VideoDecoder.KEY_PRIORITY_NAME])

        // Succeeds on low-latency attempt without calling fallback.
        var fallbackCalls = 0
        val primary = VideoDecoder.configureWithLowLatencyFallback(
            configureLowLatency = { "low-latency-codec" },
            configureFallback = {
                fallbackCalls++
                "fallback-codec"
            }
        )
        assertEquals("low-latency-codec", primary.value)
        assertFalse(primary.usedFallback)
        assertEquals(0, fallbackCalls)

        // Falls back cleanly when vendor/operating-rate configure throws.
        var capturedError: Exception? = null
        val recovered = VideoDecoder.configureWithLowLatencyFallback(
            configureLowLatency = { throw IllegalArgumentException("vendor key rejected") },
            configureFallback = { "standard-codec" },
            onLowLatencyFailure = { capturedError = it }
        )
        assertEquals("standard-codec", recovered.value)
        assertTrue(recovered.usedFallback)
        assertNotNull(capturedError)
    }

    @Test
    fun framePacingDecisionsDropStaleBuffersInLowestLatencyAndKeepSteadilyInBalanced() {
        assertEquals(
            emptyList<Boolean>(),
            VideoDecoder.selectRenderDecisions(0, VideoDecoder.FramePacingMode.LOWEST_LATENCY)
        )
        assertEquals(
            listOf(true),
            VideoDecoder.selectRenderDecisions(1, VideoDecoder.FramePacingMode.LOWEST_LATENCY)
        )
        assertEquals(
            listOf(false, false, true),
            VideoDecoder.selectRenderDecisions(3, VideoDecoder.FramePacingMode.LOWEST_LATENCY)
        )

        assertEquals(
            listOf(true),
            VideoDecoder.selectRenderDecisions(1, VideoDecoder.FramePacingMode.BALANCED)
        )
        assertEquals(
            listOf(true, true),
            VideoDecoder.selectRenderDecisions(2, VideoDecoder.FramePacingMode.BALANCED)
        )
        assertEquals(
            listOf(false, false, true, true),
            VideoDecoder.selectRenderDecisions(4, VideoDecoder.FramePacingMode.BALANCED)
        )

        val decoder = VideoDecoder()
        assertEquals(VideoDecoder.FramePacingMode.LOWEST_LATENCY, decoder.framePacingMode)
        decoder.setFramePacingMode(VideoDecoder.FramePacingMode.BALANCED)
        assertEquals(VideoDecoder.FramePacingMode.BALANCED, decoder.framePacingMode)
        decoder.release()
    }

    @Test
    fun resolvesOutputFormatDimensionsUsingCropRectWhenPresent() {
        // Macroblock-padded 1920x1088 with crop rect -> exact 1920x1080
        assertEquals(
            1920 to 1080,
            VideoDecoder.resolveOutputFormatDimensions(
                formatWidth = 1920,
                formatHeight = 1088,
                cropLeft = 0,
                cropTop = 0,
                cropRight = 1919,
                cropBottom = 1079
            )
        )
        // Non-16:9 padded 2656x1232 with crop rect -> exact 2656x1220
        assertEquals(
            2656 to 1220,
            VideoDecoder.resolveOutputFormatDimensions(
                formatWidth = 2656,
                formatHeight = 1232,
                cropLeft = 0,
                cropTop = 0,
                cropRight = 2655,
                cropBottom = 1219
            )
        )
        // No crop keys -> raw format dimensions (e.g. 4K)
        assertEquals(
            3840 to 2160,
            VideoDecoder.resolveOutputFormatDimensions(3840, 2160)
        )
        // Invalid dimensions return null
        assertNull(VideoDecoder.resolveOutputFormatDimensions(0, 0))
    }
}
