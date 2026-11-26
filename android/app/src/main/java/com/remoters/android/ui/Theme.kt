package com.remoters.android.ui

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

object RemotersColors {
    val Canvas = Color(0xFFFFFFFF)
    val Sidebar = Color(0xFFF5F5F7)
    val Surface = Color(0xFFF7F7F9)
    val Border = Color(0xFFE5E5EA)
    val Ink = Color(0xFF1D1D1F)
    val Muted = Color(0xFF6E6E73)
    val Subtle = Color(0xFF8E8E93)
    val Accent = Color(0xFF007AFF)
    val AccentLight = Color(0xFFEBF3FF)
    val Online = Color(0xFF34C759)
    val Danger = Color(0xFFFF3B30)
    val SessionBg = Color(0xFF0E0F11)
}

private val LightColorScheme = lightColorScheme(
    primary = RemotersColors.Accent,
    onPrimary = RemotersColors.Canvas,
    primaryContainer = RemotersColors.AccentLight,
    onPrimaryContainer = RemotersColors.Accent,
    secondary = RemotersColors.Muted,
    onSecondary = RemotersColors.Canvas,
    background = RemotersColors.Canvas,
    onBackground = RemotersColors.Ink,
    surface = RemotersColors.Canvas,
    onSurface = RemotersColors.Ink,
    surfaceVariant = RemotersColors.Sidebar,
    onSurfaceVariant = RemotersColors.Muted,
    outline = RemotersColors.Border,
    error = RemotersColors.Danger,
    onError = RemotersColors.Canvas
)

@Composable
fun RemotersTheme(content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = LightColorScheme,
        content = content
    )
}
