package com.remoters.android.ui

import android.content.Context
import android.os.Build
import android.util.DisplayMetrics
import android.view.WindowManager
import kotlin.math.max
import kotlin.math.min

data class NativeDisplaySize(
    val width: Int,
    val height: Int
)

internal enum class ResolutionChoice {
    Native,
    Auto,
    Fhd,
    Qhd;

    fun resolveDimensions(nativeSize: NativeDisplaySize): Pair<Int, Int> = when (this) {
        Native -> nativeSize.width to nativeSize.height
        Auto -> 0 to 0
        Fhd -> 1920 to 1080
        Qhd -> 2560 to 1440
    }

    companion object {
        fun fromStoredName(name: String?): ResolutionChoice {
            return entries.firstOrNull { it.name == name } ?: Native
        }

        fun fromDimensions(
            width: Int,
            height: Int,
            nativeSize: NativeDisplaySize
        ): ResolutionChoice = when {
            width <= 0 || height <= 0 -> Auto
            width == nativeSize.width && height == nativeSize.height -> Native
            width == 1920 && height == 1080 -> Fhd
            width == 2560 && height == 1440 -> Qhd
            else -> Native
        }
    }
}

internal fun computeNativeLandscapeSize(
    rawWidth: Int,
    rawHeight: Int,
    maxLongEdge: Int = 3840,
    maxShortEdge: Int = 2160
): NativeDisplaySize {
    if (rawWidth <= 0 || rawHeight <= 0) {
        return NativeDisplaySize(1920, 1080)
    }
    val longEdge = max(rawWidth, rawHeight)
    val shortEdge = min(rawWidth, rawHeight)
    val scale = min(
        1.0,
        min(maxLongEdge.toDouble() / longEdge, maxShortEdge.toDouble() / shortEdge)
    )
    val alignedWidth = ((longEdge * scale).toInt().coerceAtLeast(2)) and 1.inv()
    val alignedHeight = ((shortEdge * scale).toInt().coerceAtLeast(2)) and 1.inv()
    return NativeDisplaySize(alignedWidth, alignedHeight)
}

internal fun detectNativeLandscapeSize(context: Context): NativeDisplaySize {
    val windowManager = context.getSystemService(Context.WINDOW_SERVICE) as? WindowManager
        ?: return NativeDisplaySize(1920, 1080)
    val (rawW, rawH) = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
        val bounds = windowManager.maximumWindowMetrics.bounds
        bounds.width() to bounds.height()
    } else {
        val metrics = DisplayMetrics()
        @Suppress("DEPRECATION")
        windowManager.defaultDisplay?.getRealMetrics(metrics)
        metrics.widthPixels to metrics.heightPixels
    }
    return computeNativeLandscapeSize(rawW, rawH)
}
