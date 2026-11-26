package com.remoters.android.ui

import org.junit.Assert.assertEquals
import org.junit.Test

class DisplayResolutionTest {

    @Test
    fun computesLandscapeEvenNativeDimensionsForNon16By9Phones() {
        // Xiaomi 17 portrait physical size -> landscape 2656x1220
        assertEquals(NativeDisplaySize(2656, 1220), computeNativeLandscapeSize(1220, 2656))
        // Already landscape 22:10 -> preserved exactly
        assertEquals(NativeDisplaySize(2640, 1200), computeNativeLandscapeSize(2640, 1200))
        // Odd dimensions aligned down to even
        assertEquals(NativeDisplaySize(2654, 1220), computeNativeLandscapeSize(1221, 2655))
        // Oversized ultrawide 4400x2000 (2.2:1) clamped to <= 3840x2160 preserving aspect ratio
        assertEquals(NativeDisplaySize(3840, 1744), computeNativeLandscapeSize(4400, 2000))
        // Invalid dimensions fall back safely to 1920x1080
        assertEquals(NativeDisplaySize(1920, 1080), computeNativeLandscapeSize(0, 0))
    }

    @Test
    fun resolutionChoiceResolvesDefaultsAndRoundTrips() {
        val nativeSize = NativeDisplaySize(2656, 1220)
        assertEquals(ResolutionChoice.Native, ResolutionChoice.fromStoredName(null))
        assertEquals(ResolutionChoice.Native, ResolutionChoice.fromStoredName("Unknown"))
        assertEquals(ResolutionChoice.Qhd, ResolutionChoice.fromStoredName("Qhd"))

        assertEquals(2656 to 1220, ResolutionChoice.Native.resolveDimensions(nativeSize))
        assertEquals(0 to 0, ResolutionChoice.Auto.resolveDimensions(nativeSize))
        assertEquals(1920 to 1080, ResolutionChoice.Fhd.resolveDimensions(nativeSize))
        assertEquals(2560 to 1440, ResolutionChoice.Qhd.resolveDimensions(nativeSize))

        assertEquals(ResolutionChoice.Auto, ResolutionChoice.fromDimensions(0, 0, nativeSize))
        assertEquals(ResolutionChoice.Native, ResolutionChoice.fromDimensions(2656, 1220, nativeSize))
        assertEquals(ResolutionChoice.Fhd, ResolutionChoice.fromDimensions(1920, 1080, nativeSize))
        assertEquals(ResolutionChoice.Qhd, ResolutionChoice.fromDimensions(2560, 1440, nativeSize))
    }
}
