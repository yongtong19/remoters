package com.remoters.android.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.height
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remoters.android.R
import com.remoters.android.video.VideoDecoder

@Composable
internal fun ResolutionChoice.labelText(nativeSize: NativeDisplaySize): String = when (this) {
    ResolutionChoice.Native -> stringResource(
        R.string.opt_native_resolution,
        nativeSize.width,
        nativeSize.height
    )
    ResolutionChoice.Auto -> stringResource(R.string.opt_auto)
    ResolutionChoice.Fhd -> stringResource(R.string.opt_1080p)
    ResolutionChoice.Qhd -> stringResource(R.string.opt_1440p)
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun StreamQualitySection(
    nativeDisplaySize: NativeDisplaySize,
    resolutionChoice: ResolutionChoice,
    onResolutionChange: (ResolutionChoice) -> Unit,
    codecPref: Int,
    onCodecChange: (Int) -> Unit,
    hdrEnabled: Boolean,
    onHdrChange: (Boolean) -> Unit,
    maxFps: Int,
    onFpsChange: (Int) -> Unit,
    bitrateMbps: Int,
    onBitrateChange: (Int) -> Unit,
    framePacingMode: VideoDecoder.FramePacingMode,
    onFramePacingChange: (VideoDecoder.FramePacingMode) -> Unit
) {
    val autoLabel = stringResource(R.string.opt_auto)

    SectionCard {
        Text(
            text = stringResource(R.string.quality_section_title),
            fontSize = 15.sp,
            fontWeight = FontWeight.SemiBold,
            color = RemotersColors.Ink
        )
        Spacer(Modifier.height(10.dp))

        ChipSelectorRow(
            label = stringResource(R.string.quality_codec),
            options = listOf(0 to autoLabel, 3 to "AV1", 2 to "HEVC", 1 to "H.264"),
            selected = codecPref,
            onSelect = onCodecChange
        )
        Spacer(Modifier.height(10.dp))

        ChipSelectorRow(
            label = stringResource(R.string.quality_dynamic_range),
            options = listOf(
                false to stringResource(R.string.opt_sdr),
                true to stringResource(R.string.opt_hdr_10bit)
            ),
            selected = hdrEnabled,
            onSelect = onHdrChange
        )
        Spacer(Modifier.height(10.dp))

        ChipSelectorRow(
            label = stringResource(R.string.quality_max_fps),
            options = listOf(
                0 to autoLabel,
                60 to "60",
                90 to "90",
                120 to stringResource(R.string.opt_120_hz)
            ),
            selected = maxFps,
            onSelect = onFpsChange
        )
        Spacer(Modifier.height(10.dp))

        ChipSelectorRow(
            label = stringResource(R.string.quality_bitrate),
            options = listOf(
                0 to autoLabel,
                25 to "25M",
                50 to "50M",
                80 to "80M",
                100 to "100M"
            ),
            selected = bitrateMbps,
            onSelect = onBitrateChange
        )
        Spacer(Modifier.height(10.dp))

        ChipSelectorRow(
            label = stringResource(R.string.quality_frame_pacing),
            options = listOf(
                VideoDecoder.FramePacingMode.LOWEST_LATENCY to stringResource(R.string.opt_low_latency),
                VideoDecoder.FramePacingMode.BALANCED to stringResource(R.string.opt_balanced)
            ),
            selected = framePacingMode,
            onSelect = onFramePacingChange
        )
        Spacer(Modifier.height(10.dp))

        Text(
            text = stringResource(R.string.quality_display_resolution),
            fontSize = 12.sp,
            fontWeight = FontWeight.Medium,
            color = RemotersColors.Muted
        )
        Spacer(Modifier.height(6.dp))
        FlowRow(
            horizontalArrangement = Arrangement.spacedBy(6.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp)
        ) {
            ResolutionChoice.entries.forEach { option ->
                CompactChip(
                    label = option.labelText(nativeDisplaySize),
                    selected = resolutionChoice == option,
                    onClick = { onResolutionChange(option) }
                )
            }
        }
    }
}
