package com.remoters.android.ui.session

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Gamepad
import androidx.compose.material.icons.rounded.Mouse
import androidx.compose.material.icons.rounded.TouchApp
import androidx.compose.material3.Icon
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
import com.remoters.android.session.ManagedSession
import com.remoters.android.session.ManagedSessionSnapshot
import com.remoters.android.session.SessionConnectionState
import com.remoters.android.ui.AddHomeTabButton
import com.remoters.android.ui.NativeDisplaySize
import com.remoters.android.ui.RemotersColors
import com.remoters.android.ui.ResolutionChoice
import com.remoters.android.ui.SessionTabPill
import com.remoters.android.video.VideoDecoder
import kotlin.math.roundToInt

@Composable
internal fun SessionFloatingChrome(
    sessions: List<ManagedSession>,
    activeSessionId: String?,
    activeSnapshot: ManagedSessionSnapshot,
    nativeDisplaySize: NativeDisplaySize,
    selectedResolution: ResolutionChoice,
    pillExpanded: Boolean,
    gamepadCount: Int,
    inputMode: InputMode,
    onToggleExpanded: () -> Unit,
    onSelectSession: (String) -> Unit,
    onCloseSession: (String) -> Unit,
    onGoHome: () -> Unit,
    onToggleMute: () -> Unit,
    onToggleKeyboard: () -> Unit,
    onSelectInputMode: (InputMode) -> Unit,
    onSelectResolution: (ResolutionChoice) -> Unit,
    onSelectMaxFps: (Int) -> Unit,
    onSelectBitrate: (Int) -> Unit,
    onSelectFramePacing: (VideoDecoder.FramePacingMode) -> Unit,
    onDisconnect: () -> Unit,
    modifier: Modifier = Modifier
) {
    val isReady = activeSnapshot.connectionState == SessionConnectionState.CONNECTED
    val viewOnly = !activeSnapshot.canControl

    Column(
        modifier = modifier
            .fillMaxWidth()
            .padding(12.dp),
        horizontalAlignment = Alignment.End
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically
        ) {
            if (pillExpanded) {
                Surface(
                    shape = RoundedCornerShape(999.dp),
                    color = Color(0xEBFAFAFC),
                    border = BorderStroke(1.dp, Color(0x24000000)),
                    shadowElevation = 4.dp,
                    modifier = Modifier
                        .weight(1f, fill = false)
                        .padding(end = 8.dp)
                ) {
                    Row(
                        modifier = Modifier
                            .horizontalScroll(rememberScrollState())
                            .padding(horizontal = 6.dp, vertical = 4.dp),
                        horizontalArrangement = Arrangement.spacedBy(6.dp),
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        AddHomeTabButton(onClick = onGoHome)
                        sessions.forEach { session ->
                            SessionTabPill(
                                session = session,
                                selected = session.id == activeSessionId,
                                onClick = { onSelectSession(session.id) },
                                onClose = { onCloseSession(session.id) }
                            )
                        }
                    }
                }
            } else {
                Spacer(Modifier.weight(1f))
            }

            Row(
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                verticalAlignment = Alignment.CenterVertically
            ) {
                if (!viewOnly) {
                    InputModeQuickPill(
                        inputMode = inputMode,
                        onToggle = {
                            val next = if (inputMode == InputMode.MOUSE) {
                                InputMode.TOUCH
                            } else {
                                InputMode.MOUSE
                            }
                            onSelectInputMode(next)
                        }
                    )
                }

                SessionStatusPill(
                    ready = isReady,
                    viewOnly = viewOnly,
                    statsFps = activeSnapshot.stats.fps,
                    statsE2eMs = activeSnapshot.stats.e2eMs,
                    gamepadCount = gamepadCount,
                    sessionCount = sessions.size,
                    onClick = onToggleExpanded
                )
            }
        }

        if (pillExpanded) {
            Spacer(Modifier.height(6.dp))
            SessionSettingsSheet(
                viewOnly = viewOnly,
                hdrActive = activeSnapshot.hdr,
                muted = activeSnapshot.audioMuted,
                inputMode = inputMode,
                nativeDisplaySize = nativeDisplaySize,
                selectedResolution = selectedResolution,
                selectedMaxFps = activeSnapshot.requestedMaxFps,
                selectedBitrateMbps = activeSnapshot.bitrateMbps,
                framePacingMode = activeSnapshot.framePacingMode,
                stats = activeSnapshot.stats,
                decodeMs = activeSnapshot.decodeMs,
                gamepadCount = gamepadCount,
                onCollapse = onToggleExpanded,
                onToggleMute = onToggleMute,
                onToggleKeyboard = onToggleKeyboard,
                onSelectInputMode = onSelectInputMode,
                onSelectResolution = onSelectResolution,
                onSelectMaxFps = onSelectMaxFps,
                onSelectBitrate = onSelectBitrate,
                onSelectFramePacing = onSelectFramePacing,
                onGoHome = onGoHome,
                onDisconnect = onDisconnect
            )
        }
    }
}

@Composable
private fun InputModeQuickPill(
    inputMode: InputMode,
    onToggle: () -> Unit
) {
    val isMouse = inputMode == InputMode.MOUSE
    Surface(
        shape = RoundedCornerShape(999.dp),
        color = if (isMouse) RemotersColors.AccentLight else Color(0xE8FAFAFC),
        border = BorderStroke(
            width = 1.dp,
            color = if (isMouse) RemotersColors.Accent else Color(0x24000000)
        ),
        shadowElevation = 4.dp,
        modifier = Modifier.clickable(onClick = onToggle)
    ) {
        Row(
            modifier = Modifier.padding(horizontal = 10.dp, vertical = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(4.dp)
        ) {
            Icon(
                imageVector = if (isMouse) Icons.Rounded.Mouse else Icons.Rounded.TouchApp,
                contentDescription = stringResource(R.string.cd_switch_input_mode),
                tint = if (isMouse) RemotersColors.Accent else RemotersColors.Ink,
                modifier = Modifier.size(14.dp)
            )
            Text(
                text = stringResource(
                    if (isMouse) R.string.input_mode_mouse else R.string.input_mode_touch
                ),
                fontSize = 11.sp,
                fontWeight = FontWeight.Medium,
                color = if (isMouse) RemotersColors.Accent else RemotersColors.Ink
            )
        }
    }
}

@Composable
private fun SessionStatusPill(
    ready: Boolean,
    viewOnly: Boolean,
    statsFps: Double,
    statsE2eMs: Double,
    gamepadCount: Int,
    sessionCount: Int,
    onClick: () -> Unit
) {
    val modeLabel = if (viewOnly) {
        stringResource(R.string.session_mode_view)
    } else {
        stringResource(R.string.session_mode_control)
    }

    Surface(
        shape = RoundedCornerShape(999.dp),
        color = Color(0xE8FAFAFC),
        border = BorderStroke(1.dp, Color(0x24000000)),
        shadowElevation = 4.dp,
        modifier = Modifier.clickable(onClick = onClick)
    ) {
        Row(
            modifier = Modifier.padding(horizontal = 12.dp, vertical = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(6.dp)
        ) {
            Box(
                modifier = Modifier
                    .size(7.dp)
                    .clip(CircleShape)
                    .background(if (ready) RemotersColors.Online else RemotersColors.Subtle)
            )
            Text(
                text = buildString {
                    append(modeLabel)
                    if (statsFps > 0.0) {
                        append(" · ${statsFps.roundToInt()} fps")
                    }
                    if (statsE2eMs > 0.0) {
                        append(" · ${statsE2eMs.roundToInt()} ms")
                    }
                },
                fontSize = 12.sp,
                fontWeight = FontWeight.Medium,
                color = RemotersColors.Ink
            )
            if (sessionCount > 1) {
                Surface(
                    shape = RoundedCornerShape(999.dp),
                    color = RemotersColors.AccentLight
                ) {
                    Text(
                        text = sessionCount.toString(),
                        fontSize = 10.sp,
                        fontWeight = FontWeight.SemiBold,
                        color = RemotersColors.Accent,
                        modifier = Modifier.padding(horizontal = 6.dp, vertical = 1.dp)
                    )
                }
            }
            if (gamepadCount > 0) {
                Icon(
                    imageVector = Icons.Rounded.Gamepad,
                    contentDescription = stringResource(R.string.cd_controllers),
                    tint = RemotersColors.Accent,
                    modifier = Modifier.size(14.dp)
                )
                Text(
                    text = gamepadCount.toString(),
                    fontSize = 11.sp,
                    fontWeight = FontWeight.SemiBold,
                    color = RemotersColors.Accent
                )
            }
        }
    }
}
