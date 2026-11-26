package com.remoters.android.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.wrapContentWidth
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remoters.android.R
import com.remoters.android.nativebridge.NativeBridge
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

@Composable
internal fun AccountTab(
    session: SavedAccountSession,
    onSessionUpdated: (SavedAccountSession) -> Unit,
    onServerUrlChanged: (String) -> Unit,
    onSignOut: () -> Unit
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var serverUrl by remember(session.serverUrl) { mutableStateOf(session.serverUrl) }
    var isRegisterMode by remember { mutableStateOf(false) }
    var username by remember { mutableStateOf(session.username) }
    var displayName by remember { mutableStateOf("") }
    var password by remember { mutableStateOf("") }
    var loading by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp)
    ) {
        if (session.token.isNotBlank()) {
            SectionCard {
                Text(
                    text = session.displayName.ifBlank { session.username },
                    fontSize = 16.sp,
                    fontWeight = FontWeight.SemiBold,
                    color = RemotersColors.Ink
                )
                Spacer(Modifier.height(4.dp))
                Text(
                    text = "@${session.username} · ${session.serverUrl}",
                    fontSize = 13.sp,
                    color = RemotersColors.Muted
                )
                Spacer(Modifier.height(14.dp))
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.End
                ) {
                    OutlinedButton(
                        onClick = onSignOut,
                        shape = RoundedCornerShape(10.dp),
                        border = BorderStroke(1.dp, RemotersColors.Border),
                        contentPadding = PaddingValues(horizontal = 16.dp, vertical = 8.dp),
                        modifier = Modifier.wrapContentWidth()
                    ) {
                        Text(
                            text = stringResource(R.string.action_sign_out),
                            color = RemotersColors.Danger,
                            fontSize = 13.sp
                        )
                    }
                }
            }
        } else {
            SectionCard {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    Text(
                        text = if (isRegisterMode) {
                            stringResource(R.string.account_create_title)
                        } else {
                            stringResource(R.string.account_sign_in_title)
                        },
                        fontSize = 15.sp,
                        fontWeight = FontWeight.SemiBold,
                        color = RemotersColors.Ink
                    )
                    TextButton(
                        onClick = { isRegisterMode = !isRegisterMode; error = null },
                        contentPadding = PaddingValues(horizontal = 8.dp, vertical = 4.dp)
                    ) {
                        Text(
                            text = if (isRegisterMode) {
                                stringResource(R.string.account_use_existing)
                            } else {
                                stringResource(R.string.account_create_link)
                            },
                            color = RemotersColors.Accent,
                            fontSize = 13.sp
                        )
                    }
                }
                Spacer(Modifier.height(8.dp))
                RemotersField(
                    value = serverUrl,
                    onValueChange = {
                        serverUrl = it
                        onServerUrlChanged(it)
                    },
                    label = stringResource(R.string.field_server_url),
                    placeholder = AccountPreferences.DEFAULT_SERVER_URL
                )
                Spacer(Modifier.height(10.dp))
                RemotersField(
                    value = username,
                    onValueChange = { username = it; error = null },
                    label = stringResource(R.string.field_username),
                    placeholder = stringResource(R.string.field_username_placeholder)
                )
                if (isRegisterMode) {
                    Spacer(Modifier.height(10.dp))
                    RemotersField(
                        value = displayName,
                        onValueChange = { displayName = it },
                        label = stringResource(R.string.field_display_name),
                        placeholder = stringResource(R.string.field_display_name_placeholder),
                        ascii = false
                    )
                }
                Spacer(Modifier.height(10.dp))
                RemotersField(
                    value = password,
                    onValueChange = { password = it; error = null },
                    label = stringResource(R.string.field_password),
                    placeholder = "••••••••",
                    isPassword = true
                )

                if (error != null) {
                    Spacer(Modifier.height(8.dp))
                    Text(text = error!!, color = RemotersColors.Danger, fontSize = 12.sp)
                }

                Spacer(Modifier.height(12.dp))
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.End
                ) {
                    CompactPrimaryButton(
                        label = when {
                            loading -> stringResource(R.string.status_working)
                            isRegisterMode -> stringResource(R.string.action_register)
                            else -> stringResource(R.string.action_sign_in)
                        },
                        enabled = !loading,
                        onClick = {
                            scope.launch {
                                loading = true
                                error = null
                                val trimmedUrl = serverUrl.trim()
                                val result = withContext(Dispatchers.IO) {
                                    runCatching {
                                        if (isRegisterMode) {
                                            NativeBridge.accountRegister(
                                                trimmedUrl,
                                                username.trim(),
                                                displayName.trim().ifBlank { username.trim() },
                                                password
                                            )
                                        } else {
                                            NativeBridge.accountLogin(
                                                trimmedUrl,
                                                username.trim(),
                                                password
                                            )
                                        }
                                    }.fold(
                                        onSuccess = { JsonParsers.parseAuthSession(it, trimmedUrl) },
                                        onFailure = { Result.failure(it) }
                                    )
                                }
                                result.onSuccess {
                                    password = ""
                                    onSessionUpdated(it)
                                }.onFailure {
                                    error = it.message ?: context.getString(R.string.err_auth_failed)
                                }
                                loading = false
                            }
                        }
                    )
                }
            }
        }
    }
}
