package com.remoters.android.audio

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import java.util.concurrent.atomic.AtomicBoolean

class AudioPlayer {
    private val lock = Any()
    private val muted = AtomicBoolean(false)
    private var track: AudioTrack? = null
    private var currentSampleRate = 0
    private var currentChannels = 0

    fun setMuted(isMuted: Boolean) {
        muted.set(isMuted)
        synchronized(lock) {
            val active = track ?: return
            if (isMuted) {
                try {
                    active.pause()
                    active.flush()
                } catch (_: IllegalStateException) {
                }
            } else {
                try {
                    active.play()
                } catch (_: IllegalStateException) {
                }
            }
        }
    }

    fun isMuted(): Boolean = muted.get()

    fun pushPcm(pcm16Le: ByteArray, sampleRate: Int, channels: Int) {
        if (pcm16Le.isEmpty() || muted.get()) return
        val rate = if (sampleRate > 0) sampleRate else DEFAULT_SAMPLE_RATE
        val ch = if (channels == 1 || channels == 2) channels else DEFAULT_CHANNELS

        synchronized(lock) {
            if (track == null || currentSampleRate != rate || currentChannels != ch) {
                configureTrackLocked(rate, ch)
            }
            val active = track ?: return
            if (active.playState != AudioTrack.PLAYSTATE_PLAYING && !muted.get()) {
                try {
                    active.play()
                } catch (_: IllegalStateException) {
                    return
                }
            }
            active.write(pcm16Le, 0, pcm16Le.size, AudioTrack.WRITE_NON_BLOCKING)
        }
    }

    private fun configureTrackLocked(sampleRate: Int, channels: Int) {
        releaseTrackLocked()
        val channelMask = if (channels == 1) {
            AudioFormat.CHANNEL_OUT_MONO
        } else {
            AudioFormat.CHANNEL_OUT_STEREO
        }
        val minBuf = AudioTrack.getMinBufferSize(
            sampleRate,
            channelMask,
            AudioFormat.ENCODING_PCM_16BIT
        ).coerceAtLeast(4096)

        val attributes = AudioAttributes.Builder()
            .setUsage(AudioAttributes.USAGE_GAME)
            .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
            .build()

        val format = AudioFormat.Builder()
            .setSampleRate(sampleRate)
            .setChannelMask(channelMask)
            .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
            .build()

        val built = AudioTrack.Builder()
            .setAudioAttributes(attributes)
            .setAudioFormat(format)
            .setTransferMode(AudioTrack.MODE_STREAM)
            .setBufferSizeInBytes(minBuf * 2)
            .setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY)
            .build()

        track = built
        currentSampleRate = sampleRate
        currentChannels = channels
        if (!muted.get()) {
            built.play()
        }
    }

    private fun releaseTrackLocked() {
        val existing = track
        track = null
        currentSampleRate = 0
        currentChannels = 0
        if (existing != null) {
            try {
                existing.stop()
            } catch (_: IllegalStateException) {
            }
            try {
                existing.release()
            } catch (_: Exception) {
            }
        }
    }

    fun release() {
        synchronized(lock) {
            releaseTrackLocked()
        }
    }

    companion object {
        const val DEFAULT_SAMPLE_RATE = 48_000
        const val DEFAULT_CHANNELS = 2
    }
}
