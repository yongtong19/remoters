package com.remoters.android.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Add
import androidx.compose.material.icons.rounded.Close
import androidx.compose.material.icons.rounded.Home
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remoters.android.R
import com.remoters.android.session.ManagedSession
import com.remoters.android.session.SessionConnectionState

@Composable
internal fun HomeSessionTabStrip(
    sessions: List<ManagedSession>,
    onSelectSession: (ManagedSession) -> Unit,
    onCloseSession: (ManagedSession) -> Unit,
    modifier: Modifier = Modifier
) {
    if (sessions.isEmpty()) return

    Row(
        modifier = modifier
            .fillMaxWidth()
            .background(RemotersColors.Sidebar)
            .horizontalScroll(rememberScrollState())
            .padding(horizontal = 12.dp, vertical = 8.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically
    ) {
        // Active Home indicator pill
        Surface(
            shape = RoundedCornerShape(999.dp),
            color = RemotersColors.AccentLight,
            border = BorderStroke(1.dp, RemotersColors.Accent)
        ) {
            Row(
                modifier = Modifier.padding(horizontal = 10.dp, vertical = 5.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(5.dp)
            ) {
                Icon(
                    imageVector = Icons.Rounded.Home,
                    contentDescription = stringResource(R.string.cd_home),
                    tint = RemotersColors.Accent,
                    modifier = Modifier.size(14.dp)
                )
                Text(
                    text = stringResource(R.string.tab_home),
                    fontSize = 12.sp,
                    fontWeight = FontWeight.SemiBold,
                    color = RemotersColors.Accent
                )
            }
        }

        sessions.forEach { session ->
            SessionTabPill(
                session = session,
                selected = false,
                onClick = { onSelectSession(session) },
                onClose = { onCloseSession(session) }
            )
        }
    }
    HorizontalDivider(color = RemotersColors.Border)
}

@Composable
internal fun SessionTabPill(
    session: ManagedSession,
    selected: Boolean,
    onClick: () -> Unit,
    onClose: () -> Unit,
    modifier: Modifier = Modifier
) {
    val snapshot by session.state.collectAsState()
    val dotColor = when (snapshot.connectionState) {
        SessionConnectionState.CONNECTED -> RemotersColors.Online
        SessionConnectionState.CONNECTING -> Color(0xFFFF9F0A)
        SessionConnectionState.CLOSED -> RemotersColors.Danger
    }

    Surface(
        shape = RoundedCornerShape(999.dp),
        color = if (selected) RemotersColors.AccentLight else RemotersColors.Canvas,
        border = BorderStroke(
            width = 1.dp,
            color = if (selected) RemotersColors.Accent else RemotersColors.Border
        ),
        modifier = modifier.clickable(onClick = onClick)
    ) {
        Row(
            modifier = Modifier.padding(start = 10.dp, end = 6.dp, top = 5.dp, bottom = 5.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(6.dp)
        ) {
            Box(
                modifier = Modifier
                    .size(7.dp)
                    .clip(CircleShape)
                    .background(dotColor)
            )
            Text(
                text = session.title,
                fontSize = 12.sp,
                fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Medium,
                color = if (selected) RemotersColors.Accent else RemotersColors.Ink,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.widthIn(max = 132.dp)
            )
            Box(
                modifier = Modifier
                    .size(18.dp)
                    .clip(CircleShape)
                    .clickable(onClick = onClose),
                contentAlignment = Alignment.Center
            ) {
                Icon(
                    imageVector = Icons.Rounded.Close,
                    contentDescription = stringResource(R.string.cd_close_session),
                    tint = if (selected) RemotersColors.Accent else RemotersColors.Subtle,
                    modifier = Modifier.size(13.dp)
                )
            }
        }
    }
}

@Composable
internal fun AddHomeTabButton(
    onClick: () -> Unit,
    modifier: Modifier = Modifier
) {
    Surface(
        shape = RoundedCornerShape(999.dp),
        color = RemotersColors.Canvas,
        border = BorderStroke(1.dp, RemotersColors.Border),
        modifier = modifier.clickable(onClick = onClick)
    ) {
        Row(
            modifier = Modifier.padding(horizontal = 10.dp, vertical = 5.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(4.dp)
        ) {
            Icon(
                imageVector = Icons.Rounded.Add,
                contentDescription = stringResource(R.string.cd_home),
                tint = RemotersColors.Accent,
                modifier = Modifier.size(14.dp)
            )
            Text(
                text = stringResource(R.string.tab_home),
                fontSize = 12.sp,
                fontWeight = FontWeight.Medium,
                color = RemotersColors.Ink
            )
            Spacer(Modifier.width(1.dp))
        }
    }
}
