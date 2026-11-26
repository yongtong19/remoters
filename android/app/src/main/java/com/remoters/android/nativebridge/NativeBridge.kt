package com.remoters.android.nativebridge

interface SessionCallback {
    fun onReady(viewOnly: Boolean, hdr: Boolean)
    fun onVideoFrame(
        codec: Int,
        keyframe: Boolean,
        width: Int,
        height: Int,
        data: ByteArray,
        hostLatencyUs: Long
    )
    fun onAudioPcm(pcm16Le: ByteArray, sampleRate: Int, channels: Int)
    fun onStatsJson(json: String)
    fun onClosed(error: String?)
}

object NativeBridge {
    init {
        System.loadLibrary("remoters_android_ffi")
    }

    external fun accountLogin(serverUrl: String, username: String, password: String): String
    external fun accountRegister(
        serverUrl: String,
        username: String,
        displayName: String,
        password: String
    ): String

    external fun accountListDevices(serverUrl: String, token: String): String
    external fun accountDeleteDevice(serverUrl: String, token: String, deviceId: String): String
    external fun discoverNearby(timeoutMs: Int): String

    external fun sessionStart(
        address: String,
        code: String,
        width: Int,
        height: Int,
        codecPref: Int,       // 0=Auto, 1=H264, 2=HEVC, 3=AV1
        supportedCodecs: Int, // bitmask: 1=H264, 2=HEVC, 4=AV1; 0=all/auto
        hdr: Boolean,
        maxFps: Int,          // 0=Auto, else 60/90/120
        bitrateMbps: Int,     // 0=Auto, else 15..100
        callback: SessionCallback
    ): Long

    external fun sessionSendPointerMove(handle: Long, x: Int, y: Int)
    external fun sessionSendPointerButton(
        handle: Long,
        x: Int,
        y: Int,
        button: Int,
        down: Boolean
    )

    external fun sessionSendScroll(
        handle: Long,
        x: Int,
        y: Int,
        vertical: Int,
        horizontal: Int
    )

    external fun sessionSendKey(handle: Long, vkCode: Int, down: Boolean)

    external fun sessionSendGamepad(
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

    external fun sessionSetBitrate(handle: Long, bitrateMbps: Int)
    external fun sessionSetAudioMuted(handle: Long, muted: Boolean)
    external fun sessionReleaseAll(handle: Long)
    external fun sessionStop(handle: Long)
}
