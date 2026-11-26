package com.remoters.android.ui.session

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.wrapContentWidth
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remoters.android.R
import com.remoters.android.ui.RemotersColors
import com.remoters.android.ui.StreamStats
import java.util.Locale
import kotlin.math.roundToInt

@Composable
internal fun ConnectingOverlayCard(
    modifier: Modifier = Modifier
) {
    Surface(
        modifier = modifier,
        shape = RoundedCornerShape(12.dp),
        color = Color(0xEBFAFAFC),
        border = BorderStroke(1.dp, RemotersColors.Border)
    ) {
        Text(
            text = stringResource(R.string.session_connecting),
            fontSize = 13.sp,
            color = RemotersColors.Ink,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 10.dp)
        )
    }
}

@Composable
internal fun SessionEndedCard(
    errorMessage: String,
    onClose: () -> Unit,
    modifier: Modifier = Modifier
) {
    Surface(
        modifier = modifier
            .widthIn(max = 360.dp)
            .padding(16.dp),
        shape = RoundedCornerShape(14.dp),
        color = RemotersColors.Canvas,
        border = BorderStroke(1.dp, RemotersColors.Border)
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(
                text = stringResource(R.string.session_ended_title),
                fontSize = 15.sp,
                fontWeight = FontWeight.SemiBold,
                color = RemotersColors.Ink
            )
            Spacer(Modifier.height(6.dp))
            Text(
                text = errorMessage,
                fontSize = 13.sp,
                color = RemotersColors.Muted
            )
            Spacer(Modifier.height(12.dp))
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.End
            ) {
                OutlinedButton(
                    onClick = onClose,
                    shape = RoundedCornerShape(10.dp),
                    contentPadding = PaddingValues(horizontal = 14.dp, vertical = 6.dp),
                    modifier = Modifier.wrapContentWidth()
                ) {
                    Text(
                        text = stringResource(R.string.action_close),
                        fontSize = 13.sp,
                        color = RemotersColors.Ink
                    )
                }
            }
        }
    }
}

@Composable
internal fun SessionTelemetryRows(
    stats: StreamStats,
    decodeMs: Double,
    gamepadCount: Int
) {
    TelemetryRow(stringResource(R.string.stat_e2e_latency), formatMs(stats.e2eMs))
    TelemetryRow(
        stringResource(R.string.stat_encode_decode),
        "${formatMs(stats.encodeMs)} / ${formatMs(decodeMs)}"
    )
    TelemetryRow(
        stringResource(R.string.stat_rtt_loss),
        "${formatMs(stats.rttMs)} · ${stats.lostPackets}"
    )
    TelemetryRow(
        stringResource(R.string.stat_fps),
        String.format(Locale.US, "%.1f fps", stats.fps)
    )
    TelemetryRow(stringResource(R.string.stat_bitrate), formatMbps(stats.videoBps))
    TelemetryRow(
        stringResource(R.string.stat_gamepads),
        stringResource(R.string.stat_gamepads_connected, gamepadCount)
    )
}

@Composable
internal fun TelemetryRow(label: String, value: String) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.SpaceBetween
    ) {
        Text(text = label, fontSize = 11.sp, color = RemotersColors.Muted)
        Text(
            text = value,
            fontSize = 11.sp,
            fontFamily = FontFamily.Monospace,
            fontWeight = FontWeight.Medium,
            color = RemotersColors.Ink
        )
    }
}

internal fun formatMs(ms: Double): String {
    if (!ms.isFinite() || ms <= 0.0) return "—"
    return if (ms < 10.0) {
        String.format(Locale.US, "%.1f ms", ms)
    } else {
        "${ms.roundToInt()} ms"
    }
}

internal fun formatMbps(bps: Double): String {
    if (!bps.isFinite() || bps <= 0.0) return "0 Mbps"
    return String.format(Locale.US, "%.1f Mbps", bps / 1_000_000.0)
}
