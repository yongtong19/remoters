package com.remoters.android.ui.session

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.wrapContentWidth
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.VolumeOff
import androidx.compose.material.icons.automirrored.rounded.VolumeUp
import androidx.compose.material.icons.rounded.Close
import androidx.compose.material.icons.rounded.Home
import androidx.compose.material.icons.rounded.Keyboard
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remoters.android.R
import com.remoters.android.input.InputMode
import com.remoters.android.ui.NativeDisplaySize
import com.remoters.android.ui.RemotersColors
import com.remoters.android.ui.ResolutionChoice
import com.remoters.android.ui.StreamStats
import com.remoters.android.video.VideoDecoder

@Composable
internal fun SessionSettingsSheet(
    viewOnly: Boolean,
    hdrActive: Boolean,
    muted: Boolean,
    inputMode: InputMode,
    nativeDisplaySize: NativeDisplaySize,
    selectedResolution: ResolutionChoice,
    selectedMaxFps: Int,
    selectedBitrateMbps: Int,
    framePacingMode: VideoDecoder.FramePacingMode,
    stats: StreamStats,
    decodeMs: Double,
    gamepadCount: Int,
    onCollapse: () -> Unit,
    onToggleMute: () -> Unit,
    onToggleKeyboard: () -> Unit,
    onSelectInputMode: (InputMode) -> Unit,
    onSelectResolution: (ResolutionChoice) -> Unit,
    onSelectMaxFps: (Int) -> Unit,
    onSelectBitrate: (Int) -> Unit,
    onSelectFramePacing: (VideoDecoder.FramePacingMode) -> Unit,
    onGoHome: () -> Unit,
    onDisconnect: () -> Unit,
    modifier: Modifier = Modifier
) {
    Surface(
        shape = RoundedCornerShape(14.dp),
        color = Color(0xF5FAFAFC),
        border = BorderStroke(1.dp, Color(0x20000000)),
        shadowElevation = 8.dp,
        modifier = modifier.widthIn(min = 276.dp, max = 330.dp)
    ) {
        Column(
            modifier = Modifier
                .verticalScroll(rememberScrollState())
                .padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp)
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically
            ) {
                val showHdr = stats.hdr || hdrActive
                Text(
                    text = if (stats.width > 0) {
                        buildString {
                            append("${stats.codec} · ${stats.width}×${stats.height}")
                            if (showHdr) append(" · HDR")
                        }
                    } else {
                        stringResource(R.string.session_telemetry_title)
                    },
                    fontSize = 12.sp,
                    fontWeight = FontWeight.SemiBold,
                    color = RemotersColors.Ink
                )
                Icon(
                    imageVector = Icons.Rounded.Close,
                    contentDescription = stringResource(R.string.cd_collapse),
                    tint = RemotersColors.Muted,
                    modifier = Modifier
                        .size(16.dp)
                        .clickable(onClick = onCollapse)
                )
            }

            SessionTelemetryRows(
                stats = stats,
                decodeMs = decodeMs,
                gamepadCount = gamepadCount
            )

            HorizontalDivider(color = RemotersColors.Border)

            if (!viewOnly) {
                Text(
                    text = stringResource(R.string.session_input_mode_label),
                    fontSize = 11.sp,
                    color = RemotersColors.Muted
                )
                SegmentedOptionRow(
                    options = listOf(
                        InputMode.TOUCH to stringResource(R.string.input_mode_touch),
                        InputMode.MOUSE to stringResource(R.string.input_mode_mouse)
                    ),
                    selected = inputMode,
                    onSelect = onSelectInputMode
                )
                if (inputMode == InputMode.MOUSE) {
                    Text(
                        text = stringResource(R.string.mouse_mode_hint),
                        fontSize = 10.sp,
                        color = RemotersColors.Subtle
                    )
                }
            }

            Text(
                text = "${stringResource(R.string.quality_display_resolution)} · ${nativeDisplaySize.width}×${nativeDisplaySize.height}",
                fontSize = 11.sp,
                color = RemotersColors.Muted
            )
            SegmentedOptionRow(
                options = listOf(
                    ResolutionChoice.Native to stringResource(R.string.opt_native_short),
                    ResolutionChoice.Auto to stringResource(R.string.opt_auto),
                    ResolutionChoice.Fhd to stringResource(R.string.opt_1080p),
                    ResolutionChoice.Qhd to stringResource(R.string.opt_1440p)
                ),
                selected = selectedResolution,
                onSelect = onSelectResolution
            )

            Text(
                text = stringResource(R.string.quality_max_fps),
                fontSize = 11.sp,
                color = RemotersColors.Muted
            )
            SegmentedOptionRow(
                options = listOf(
                    0 to stringResource(R.string.opt_auto),
                    60 to "60",
                    90 to "90",
                    120 to "120"
                ),
                selected = selectedMaxFps,
                onSelect = onSelectMaxFps
            )

            Text(
                text = stringResource(R.string.session_bitrate_target),
                fontSize = 11.sp,
                color = RemotersColors.Muted
            )
            SegmentedOptionRow(
                options = listOf(
                    0 to stringResource(R.string.opt_auto),
                    25 to "25",
                    50 to "50",
                    80 to "80",
                    100 to "100"
                ),
                selected = selectedBitrateMbps,
                onSelect = onSelectBitrate
            )

            Text(
                text = stringResource(R.string.session_frame_pacing_label),
                fontSize = 11.sp,
                color = RemotersColors.Muted
            )
            SegmentedOptionRow(
                options = listOf(
                    VideoDecoder.FramePacingMode.LOWEST_LATENCY to stringResource(R.string.opt_low_latency),
                    VideoDecoder.FramePacingMode.BALANCED to stringResource(R.string.opt_balanced)
                ),
                selected = framePacingMode,
                onSelect = onSelectFramePacing
            )

            HorizontalDivider(color = RemotersColors.Border)

            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically
            ) {
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    OutlinedButton(
                        onClick = onToggleMute,
                        shape = RoundedCornerShape(8.dp),
                        contentPadding = PaddingValues(horizontal = 9.dp, vertical = 4.dp),
                        modifier = Modifier.height(32.dp)
                    ) {
                        Icon(
                            imageVector = if (muted) {
                                Icons.AutoMirrored.Rounded.VolumeOff
                            } else {
                                Icons.AutoMirrored.Rounded.VolumeUp
                            },
                            contentDescription = if (muted) {
                                stringResource(R.string.cd_unmute)
                            } else {
                                stringResource(R.string.cd_mute)
                            },
                            tint = RemotersColors.Ink,
                            modifier = Modifier.size(15.dp)
                        )
                    }
                    if (!viewOnly) {
                        OutlinedButton(
                            onClick = onToggleKeyboard,
                            shape = RoundedCornerShape(8.dp),
                            contentPadding = PaddingValues(horizontal = 9.dp, vertical = 4.dp),
                            modifier = Modifier.height(32.dp)
                        ) {
                            Icon(
                                imageVector = Icons.Rounded.Keyboard,
                                contentDescription = stringResource(R.string.cd_keyboard),
                                tint = RemotersColors.Ink,
                                modifier = Modifier.size(15.dp)
                            )
                        }
                    }
                    OutlinedButton(
                        onClick = onGoHome,
                        shape = RoundedCornerShape(8.dp),
                        contentPadding = PaddingValues(horizontal = 9.dp, vertical = 4.dp),
                        modifier = Modifier.height(32.dp)
                    ) {
                        Icon(
                            imageVector = Icons.Rounded.Home,
                            contentDescription = stringResource(R.string.cd_home),
                            tint = RemotersColors.Ink,
                            modifier = Modifier.size(15.dp)
                        )
                    }
                }
                OutlinedButton(
                    onClick = onDisconnect,
                    shape = RoundedCornerShape(8.dp),
                    border = BorderStroke(1.dp, RemotersColors.Danger),
                    colors = ButtonDefaults.outlinedButtonColors(
                        contentColor = RemotersColors.Danger
                    ),
                    contentPadding = PaddingValues(horizontal = 10.dp, vertical = 4.dp),
                    modifier = Modifier
                        .wrapContentWidth()
                        .height(32.dp)
                ) {
                    Text(
                        text = stringResource(R.string.action_disconnect),
                        fontSize = 12.sp,
                        fontWeight = FontWeight.Medium
                    )
                }
            }
        }
    }
}

@Composable
private fun <T> SegmentedOptionRow(
    options: List<Pair<T, String>>,
    selected: T,
    onSelect: (T) -> Unit
) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(4.dp)
    ) {
        options.forEach { (value, label) ->
            val active = selected == value
            Box(
                contentAlignment = Alignment.Center,
                modifier = Modifier
                    .weight(1f)
                    .height(26.dp)
                    .clip(RoundedCornerShape(7.dp))
                    .background(if (active) RemotersColors.AccentLight else RemotersColors.Surface)
                    .border(
                        width = 1.dp,
                        color = if (active) RemotersColors.Accent else RemotersColors.Border,
                        shape = RoundedCornerShape(7.dp)
                    )
                    .clickable { onSelect(value) }
            ) {
                Text(
                    text = label,
                    fontSize = 11.sp,
                    fontWeight = if (active) FontWeight.SemiBold else FontWeight.Normal,
                    color = if (active) RemotersColors.Accent else RemotersColors.Ink
                )
            }
        }
    }
}
