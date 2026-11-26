package com.remoters.android.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.DeleteOutline
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remoters.android.R

@Composable
internal fun MyDevicesTab(
    signedIn: Boolean,
    devices: List<AccountDevice>,
    loading: Boolean,
    error: String?,
    onRefresh: () -> Unit,
    onGoAccount: () -> Unit,
    onConnectDevice: (AccountDevice) -> Unit,
    onDeleteDevice: (AccountDevice) -> Unit
) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp)
    ) {
        if (!signedIn) {
            SectionCard {
                Text(
                    text = stringResource(R.string.devices_sign_in_title),
                    fontSize = 15.sp,
                    fontWeight = FontWeight.SemiBold,
                    color = RemotersColors.Ink
                )
                Spacer(Modifier.height(6.dp))
                Text(
                    text = stringResource(R.string.devices_sign_in_desc),
                    fontSize = 13.sp,
                    color = RemotersColors.Muted
                )
                Spacer(Modifier.height(12.dp))
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.End
                ) {
                    CompactPrimaryButton(
                        label = stringResource(R.string.action_open_account),
                        onClick = onGoAccount
                    )
                }
            }
            return
        }

        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically
        ) {
            Text(
                text = stringResource(R.string.devices_registered_count, devices.size),
                fontSize = 13.sp,
                color = RemotersColors.Muted
            )
            CompactSecondaryButton(
                label = if (loading) {
                    stringResource(R.string.status_refreshing)
                } else {
                    stringResource(R.string.action_refresh)
                },
                enabled = !loading,
                onClick = onRefresh
            )
        }

        if (error != null) {
            Text(text = error, color = RemotersColors.Danger, fontSize = 12.sp)
        }

        if (loading && devices.isEmpty()) {
            Box(
                Modifier
                    .fillMaxWidth()
                    .padding(24.dp),
                contentAlignment = Alignment.Center
            ) {
                CircularProgressIndicator(
                    color = RemotersColors.Accent,
                    modifier = Modifier.size(28.dp)
                )
            }
        } else if (devices.isEmpty()) {
            SectionCard {
                Text(
                    text = stringResource(R.string.devices_empty),
                    fontSize = 14.sp,
                    color = RemotersColors.Muted
                )
            }
        } else {
            LazyColumn(
                verticalArrangement = Arrangement.spacedBy(10.dp),
                contentPadding = PaddingValues(bottom = 16.dp)
            ) {
                items(devices, key = { it.deviceId }) { device ->
                    SectionCard {
                        Row(
                            modifier = Modifier.fillMaxWidth(),
                            horizontalArrangement = Arrangement.SpaceBetween,
                            verticalAlignment = Alignment.CenterVertically
                        ) {
                            Row(
                                verticalAlignment = Alignment.CenterVertically,
                                modifier = Modifier.weight(1f)
                            ) {
                                PlatformBadge(device.platform)
                                Spacer(Modifier.width(10.dp))
                                Column {
                                    Text(
                                        text = device.name,
                                        fontSize = 14.sp,
                                        fontWeight = FontWeight.SemiBold,
                                        color = RemotersColors.Ink
                                    )
                                    val sub = listOfNotNull(
                                        device.platform,
                                        device.displayLabel.takeIf { it.isNotBlank() },
                                        device.hostAddress
                                    ).joinToString(" · ")
                                    Text(
                                        text = sub,
                                        fontSize = 12.sp,
                                        color = RemotersColors.Muted
                                    )
                                }
                            }
                            StatusPill(
                                label = when {
                                    device.online && device.sharing -> stringResource(R.string.status_sharing)
                                    device.online -> stringResource(R.string.status_online)
                                    else -> stringResource(R.string.status_offline)
                                },
                                active = device.online && device.sharing
                            )
                        }
                        Spacer(Modifier.height(10.dp))
                        Row(
                            modifier = Modifier.fillMaxWidth(),
                            horizontalArrangement = Arrangement.SpaceBetween,
                            verticalAlignment = Alignment.CenterVertically
                        ) {
                            IconButton(
                                onClick = { onDeleteDevice(device) },
                                modifier = Modifier.size(34.dp)
                            ) {
                                Icon(
                                    imageVector = Icons.Rounded.DeleteOutline,
                                    contentDescription = stringResource(R.string.cd_remove_device),
                                    tint = RemotersColors.Danger
                                )
                            }
                            if (device.online && device.sharing && !device.hostAddress.isNullOrBlank()) {
                                CompactPrimaryButton(
                                    label = if (device.viewOnly) {
                                        stringResource(R.string.action_view)
                                    } else {
                                        stringResource(R.string.action_connect)
                                    },
                                    onClick = { onConnectDevice(device) },
                                    trailingArrow = true
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
internal fun NearbyTab(
    hosts: List<NearbyHost>,
    loading: Boolean,
    error: String?,
    onScan: () -> Unit,
    onSelectHost: (NearbyHost) -> Unit
) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp)
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically
        ) {
            Text(
                text = stringResource(R.string.nearby_header),
                fontSize = 13.sp,
                color = RemotersColors.Muted
            )
            CompactSecondaryButton(
                label = if (loading) {
                    stringResource(R.string.status_scanning)
                } else {
                    stringResource(R.string.action_scan_lan)
                },
                enabled = !loading,
                onClick = onScan
            )
        }

        if (error != null) {
            Text(text = error, color = RemotersColors.Danger, fontSize = 12.sp)
        }

        if (loading && hosts.isEmpty()) {
            Box(
                Modifier
                    .fillMaxWidth()
                    .padding(24.dp),
                contentAlignment = Alignment.Center
            ) {
                CircularProgressIndicator(
                    color = RemotersColors.Accent,
                    modifier = Modifier.size(28.dp)
                )
            }
        } else if (hosts.isEmpty()) {
            SectionCard {
                Text(
                    text = stringResource(R.string.nearby_empty),
                    fontSize = 13.sp,
                    color = RemotersColors.Muted
                )
            }
        } else {
            LazyColumn(
                verticalArrangement = Arrangement.spacedBy(10.dp),
                contentPadding = PaddingValues(bottom = 16.dp)
            ) {
                items(hosts, key = { "${it.instanceId}-${it.address}" }) { host ->
                    SectionCard {
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .clickable { onSelectHost(host) },
                            horizontalArrangement = Arrangement.SpaceBetween,
                            verticalAlignment = Alignment.CenterVertically
                        ) {
                            Row(
                                verticalAlignment = Alignment.CenterVertically,
                                modifier = Modifier.weight(1f)
                            ) {
                                PlatformBadge(host.platform)
                                Spacer(Modifier.width(10.dp))
                                Column {
                                    Text(
                                        text = host.hostName,
                                        fontSize = 14.sp,
                                        fontWeight = FontWeight.SemiBold,
                                        color = RemotersColors.Ink
                                    )
                                    Text(
                                        text = listOf(host.address, host.displayLabel)
                                            .filter { it.isNotBlank() }
                                            .joinToString(" · "),
                                        fontSize = 12.sp,
                                        fontFamily = FontFamily.Monospace,
                                        color = RemotersColors.Muted
                                    )
                                }
                            }
                            CompactPrimaryButton(
                                label = stringResource(R.string.action_use),
                                onClick = { onSelectHost(host) }
                            )
                        }
                    }
                }
            }
        }
    }
}
