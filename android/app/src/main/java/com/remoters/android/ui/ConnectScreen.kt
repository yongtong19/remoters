package com.remoters.android.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
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
internal fun ConnectTab(
    address: String,
    onAddressChange: (String) -> Unit,
    pairingCode: String,
    onCodeChange: (String) -> Unit,
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
    onFramePacingChange: (VideoDecoder.FramePacingMode) -> Unit,
    error: String?,
    onConnect: () -> Unit
) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp)
    ) {
        SectionCard {
            Text(
                text = stringResource(R.string.connect_host_section),
                fontSize = 15.sp,
                fontWeight = FontWeight.SemiBold,
                color = RemotersColors.Ink
            )
            Spacer(Modifier.height(10.dp))
            RemotersField(
                value = address,
                onValueChange = onAddressChange,
                label = stringResource(R.string.field_host_address),
                placeholder = stringResource(R.string.field_host_address_placeholder)
            )
            Spacer(Modifier.height(10.dp))
            RemotersField(
                value = pairingCode,
                onValueChange = onCodeChange,
                label = stringResource(R.string.field_pairing_code),
                placeholder = stringResource(R.string.field_pairing_code_placeholder),
                showPasteButton = true
            )
            if (error != null) {
                Spacer(Modifier.height(8.dp))
                Text(
                    text = error,
                    color = RemotersColors.Danger,
                    fontSize = 12.sp
                )
            }
            Spacer(Modifier.height(12.dp))
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.End
            ) {
                CompactPrimaryButton(
                    label = stringResource(R.string.action_connect),
                    onClick = onConnect,
                    trailingArrow = true
                )
            }
        }

        StreamQualitySection(
            nativeDisplaySize = nativeDisplaySize,
            resolutionChoice = resolutionChoice,
            onResolutionChange = onResolutionChange,
            codecPref = codecPref,
            onCodecChange = onCodecChange,
            hdrEnabled = hdrEnabled,
            onHdrChange = onHdrChange,
            maxFps = maxFps,
            onFpsChange = onFpsChange,
            bitrateMbps = bitrateMbps,
            onBitrateChange = onBitrateChange,
            framePacingMode = framePacingMode,
            onFramePacingChange = onFramePacingChange
        )
    }
}
