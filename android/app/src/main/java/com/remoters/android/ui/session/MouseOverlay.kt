package com.remoters.android.ui.session

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.ChevronLeft
import androidx.compose.material.icons.rounded.Mouse
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remoters.android.R
import com.remoters.android.input.TouchInputHandler
import com.remoters.android.input.computeFittedVideoRect
import com.remoters.android.ui.RemotersColors
import kotlin.math.roundToInt

@Composable
internal fun VirtualMouseCursorOverlay(
    cursorX: Int,
    cursorY: Int,
    frameWidth: Int,
    frameHeight: Int,
    dragLocked: Boolean,
    modifier: Modifier = Modifier
) {
    Canvas(modifier = modifier.fillMaxSize()) {
        val rect = computeFittedVideoRect(
            viewWidth = size.width.roundToInt().coerceAtLeast(1),
            viewHeight = size.height.roundToInt().coerceAtLeast(1),
            frameWidth = frameWidth,
            frameHeight = frameHeight
        )
        val px = rect.offsetX + (cursorX.coerceIn(0, TouchInputHandler.MAX_COORD) / 65535f) * rect.width
        val py = rect.offsetY + (cursorY.coerceIn(0, TouchInputHandler.MAX_COORD) / 65535f) * rect.height
        val s = 1.dp.toPx()

        fun buildCursorPath(originX: Float, originY: Float): Path = Path().apply {
            moveTo(originX, originY)
            lineTo(originX, originY + 18f * s)
            lineTo(originX + 4.4f * s, originY + 13.8f * s)
            lineTo(originX + 7.6f * s, originY + 20.5f * s)
            lineTo(originX + 10.4f * s, originY + 19.2f * s)
            lineTo(originX + 7.2f * s, originY + 12.6f * s)
            lineTo(originX + 13.0f * s, originY + 12.6f * s)
            close()
        }

        // Soft drop shadow for visibility on pure-white desktop windows
        drawPath(
            path = buildCursorPath(px + 1.2f * s, py + 1.5f * s),
            color = Color(0x55000000)
        )

        val cursorPath = buildCursorPath(px, py)
        drawPath(
            path = cursorPath,
            color = Color.White
        )
        drawPath(
            path = cursorPath,
            color = Color(0xFF111317),
            style = Stroke(
                width = 1.5f * s,
                join = StrokeJoin.Round,
                cap = StrokeCap.Round
            )
        )

        if (dragLocked) {
            drawCircle(
                color = RemotersColors.Accent,
                radius = 4f * s,
                center = Offset(px + 14.5f * s, py + 5f * s)
            )
            drawCircle(
                color = Color.White,
                radius = 4f * s,
                center = Offset(px + 14.5f * s, py + 5f * s),
                style = Stroke(width = 1.2f * s)
            )
        }
    }
}

@Composable
internal fun MouseActionBar(
    dragLocked: Boolean,
    minimized: Boolean,
    onLeftClick: () -> Unit,
    onMiddleClick: () -> Unit,
    onRightClick: () -> Unit,
    onToggleDragLock: () -> Unit,
    onToggleMinimized: () -> Unit,
    modifier: Modifier = Modifier
) {
    if (minimized) {
        Surface(
            shape = CircleShape,
            color = Color(0xE0FAFAFC),
            border = BorderStroke(
                width = 1.dp,
                color = if (dragLocked) RemotersColors.Accent else Color(0x28000000)
            ),
            shadowElevation = 4.dp,
            modifier = modifier
                .padding(12.dp)
                .clickable(onClick = onToggleMinimized)
        ) {
            Box(
                contentAlignment = Alignment.Center,
                modifier = Modifier.padding(10.dp)
            ) {
                Icon(
                    imageVector = Icons.Rounded.Mouse,
                    contentDescription = stringResource(R.string.cd_expand_mouse_bar),
                    tint = if (dragLocked) RemotersColors.Accent else RemotersColors.Ink,
                    modifier = Modifier.size(18.dp)
                )
            }
        }
        return
    }

    Surface(
        shape = RoundedCornerShape(999.dp),
        color = Color(0xE8FAFAFC),
        border = BorderStroke(1.dp, Color(0x24000000)),
        shadowElevation = 4.dp,
        modifier = modifier.padding(12.dp)
    ) {
        Row(
            modifier = Modifier.padding(horizontal = 6.dp, vertical = 5.dp),
            horizontalArrangement = Arrangement.spacedBy(5.dp),
            verticalAlignment = Alignment.CenterVertically
        ) {
            MouseBarButton(
                label = stringResource(R.string.mouse_btn_left),
                active = false,
                onClick = onLeftClick
            )
            MouseBarButton(
                label = stringResource(R.string.mouse_btn_middle),
                active = false,
                onClick = onMiddleClick
            )
            MouseBarButton(
                label = stringResource(R.string.mouse_btn_right),
                active = false,
                onClick = onRightClick
            )
            MouseBarButton(
                label = stringResource(R.string.mouse_btn_drag),
                active = dragLocked,
                onClick = onToggleDragLock
            )
            Box(
                contentAlignment = Alignment.Center,
                modifier = Modifier
                    .size(28.dp)
                    .clip(CircleShape)
                    .clickable(onClick = onToggleMinimized)
            ) {
                Icon(
                    imageVector = Icons.Rounded.ChevronLeft,
                    contentDescription = stringResource(R.string.cd_minimize_mouse_bar),
                    tint = RemotersColors.Muted,
                    modifier = Modifier.size(18.dp)
                )
            }
        }
    }
}

@Composable
private fun MouseBarButton(
    label: String,
    active: Boolean,
    onClick: () -> Unit
) {
    Box(
        contentAlignment = Alignment.Center,
        modifier = Modifier
            .height(28.dp)
            .clip(RoundedCornerShape(999.dp))
            .background(if (active) RemotersColors.AccentLight else RemotersColors.Surface)
            .border(
                width = 1.dp,
                color = if (active) RemotersColors.Accent else RemotersColors.Border,
                shape = RoundedCornerShape(999.dp)
            )
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp)
    ) {
        Text(
            text = label,
            fontSize = 11.sp,
            fontWeight = if (active) FontWeight.SemiBold else FontWeight.Medium,
            color = if (active) RemotersColors.Accent else RemotersColors.Ink
        )
    }
}
