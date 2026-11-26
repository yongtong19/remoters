package com.remoters.android.ui

import android.content.Context
import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.CastConnected
import androidx.compose.material.icons.rounded.Devices
import androidx.compose.material.icons.rounded.Person
import androidx.compose.material.icons.rounded.WifiFind
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.NavigationBarItemDefaults
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
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
import com.remoters.android.i18n.AppLocale
import com.remoters.android.i18n.AppLocaleManager
import com.remoters.android.i18n.LocalizedAppContent
import com.remoters.android.nativebridge.NativeBridge
import com.remoters.android.session.SessionLaunchRequest
import com.remoters.android.session.SessionManager
import com.remoters.android.ui.session.SessionActivity
import com.remoters.android.video.VideoDecoder
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

private enum class HomeTab {
    Connect,
    Devices,
    Nearby,
    Account
}

@Composable
private fun HomeTab.labelText(): String = when (this) {
    HomeTab.Connect -> stringResource(R.string.tab_connect)
    HomeTab.Devices -> stringResource(R.string.tab_devices)
    HomeTab.Nearby -> stringResource(R.string.tab_nearby)
    HomeTab.Account -> stringResource(R.string.tab_account)
}

class MainActivity : ComponentActivity() {
    override fun attachBaseContext(newBase: Context) {
        super.attachBaseContext(AppLocaleManager.wrapContext(newBase))
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        AppLocaleManager.resolveLocale(this)
        val deepLinkAddress = intent?.data?.getQueryParameter("address").orEmpty()
        val deepLinkCode = intent?.data?.getQueryParameter("code").orEmpty()

        setContent {
            val currentLocale by AppLocaleManager.currentLocale.collectAsState()
            LocalizedAppContent(locale = currentLocale) {
                RemotersTheme {
                    RemotersMainScreen(
                        currentLocale = currentLocale,
                        onSelectLocale = { locale ->
                            AppLocaleManager.setLocale(this@MainActivity, locale)
                        },
                        initialAddress = deepLinkAddress,
                        initialCode = deepLinkCode,
                        onOpenSession = { sessionId ->
                            val intent = Intent(this@MainActivity, SessionActivity::class.java).apply {
                                putExtra(SessionActivity.EXTRA_SESSION_ID, sessionId)
                            }
                            startActivity(intent)
                        }
                    )
                }
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun RemotersMainScreen(
    currentLocale: AppLocale,
    onSelectLocale: (AppLocale) -> Unit,
    initialAddress: String = "",
    initialCode: String = "",
    onOpenSession: (sessionId: String) -> Unit
) {
    val context = LocalContext.current
    val prefs = remember { AccountPreferences(context) }
    val nativeDisplaySize = remember(context) { detectNativeLandscapeSize(context) }
    val supportedCodecsMask = remember { VideoDecoder.probeSupportedCodecsMask() }
    val scope = rememberCoroutineScope()

    val liveSessions by SessionManager.sessions.collectAsState()

    var selectedTab by remember { mutableStateOf(HomeTab.Connect) }
    var accountSession by remember { mutableStateOf(prefs.load()) }

    var address by remember { mutableStateOf(initialAddress.ifBlank { prefs.loadLastAddress() }) }
    var pairingCode by remember { mutableStateOf(initialCode) }
    var selectedHostTitle by remember { mutableStateOf("") }
    var resolutionChoice by remember {
        mutableStateOf(ResolutionChoice.fromStoredName(prefs.loadResolutionChoice()))
    }
    var codecPref by remember { mutableIntStateOf(0) } // 0=Auto, 1=H264, 2=HEVC, 3=AV1
    var hdrEnabled by remember { mutableStateOf(false) }
    var maxFps by remember { mutableIntStateOf(0) } // 0=Auto, 60, 90, 120
    var bitrateMbps by remember { mutableIntStateOf(0) } // 0=Auto, 25, 50, 80, 100
    var framePacingMode by remember { mutableStateOf(VideoDecoder.FramePacingMode.LOWEST_LATENCY) }
    var connectError by remember { mutableStateOf<String?>(null) }

    var devices by remember { mutableStateOf<List<AccountDevice>>(emptyList()) }
    var devicesLoading by remember { mutableStateOf(false) }
    var devicesError by remember { mutableStateOf<String?>(null) }

    var nearbyHosts by remember { mutableStateOf<List<NearbyHost>>(emptyList()) }
    var nearbyLoading by remember { mutableStateOf(false) }
    var nearbyError by remember { mutableStateOf<String?>(null) }

    fun launchWithPreferences(
        targetAddress: String,
        targetCode: String,
        targetTitle: String = ""
    ) {
        val normalizedAddr = normalizeHostAddress(targetAddress)
        val normalizedCode = targetCode.filterNot { it.isWhitespace() }
        if (normalizedAddr.isBlank()) {
            connectError = context.getString(R.string.err_enter_host_address)
            selectedTab = HomeTab.Connect
            return
        }
        if (normalizedCode.isBlank()) {
            connectError = context.getString(R.string.err_enter_pairing_code)
            selectedTab = HomeTab.Connect
            return
        }
        connectError = null
        prefs.saveLastAddress(normalizedAddr)
        prefs.saveResolutionChoice(resolutionChoice.name)
        val (targetWidth, targetHeight) = resolutionChoice.resolveDimensions(nativeDisplaySize)
        val result = SessionManager.connectOrSwitch(
            SessionLaunchRequest(
                title = targetTitle.ifBlank { selectedHostTitle },
                address = normalizedAddr,
                code = normalizedCode,
                width = targetWidth,
                height = targetHeight,
                codecPref = codecPref,
                supportedCodecs = supportedCodecsMask,
                hdr = hdrEnabled,
                maxFps = maxFps,
                bitrateMbps = bitrateMbps,
                framePacingMode = framePacingMode
            )
        )
        onOpenSession(result.session.id)
    }

    fun refreshDevices() {
        val session = accountSession
        if (session.token.isBlank()) return
        scope.launch {
            devicesLoading = true
            devicesError = null
            val result = withContext(Dispatchers.IO) {
                runCatching {
                    NativeBridge.accountListDevices(session.serverUrl, session.token)
                }.fold(
                    onSuccess = { JsonParsers.parseDevices(it) },
                    onFailure = { Result.failure(it) }
                )
            }
            result.onSuccess { devices = it }
                .onFailure { devicesError = it.message ?: context.getString(R.string.err_load_devices) }
            devicesLoading = false
        }
    }

    fun scanNearby() {
        scope.launch {
            nearbyLoading = true
            nearbyError = null
            val result = withContext(Dispatchers.IO) {
                runCatching {
                    NativeBridge.discoverNearby(1500)
                }.fold(
                    onSuccess = { JsonParsers.parseNearbyHosts(it) },
                    onFailure = { Result.failure(it) }
                )
            }
            result.onSuccess { nearbyHosts = it }
                .onFailure { nearbyError = it.message ?: context.getString(R.string.err_lan_scan_failed) }
            nearbyLoading = false
        }
    }

    LaunchedEffect(selectedTab, accountSession.token) {
        if (selectedTab == HomeTab.Devices && accountSession.token.isNotBlank()) {
            refreshDevices()
        } else if (selectedTab == HomeTab.Nearby && nearbyHosts.isEmpty() && !nearbyLoading) {
            scanNearby()
        }
    }

    Scaffold(
        containerColor = RemotersColors.Canvas,
        topBar = {
            Column {
                TopAppBar(
                    colors = TopAppBarDefaults.topAppBarColors(
                        containerColor = RemotersColors.Canvas,
                        titleContentColor = RemotersColors.Ink
                    ),
                    title = {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text(
                                text = stringResource(R.string.app_name),
                                fontSize = 18.sp,
                                fontWeight = FontWeight.SemiBold,
                                color = RemotersColors.Ink
                            )
                            Spacer(Modifier.width(8.dp))
                            Surface(
                                shape = RoundedCornerShape(999.dp),
                                color = RemotersColors.Sidebar,
                                border = BorderStroke(1.dp, RemotersColors.Border)
                            ) {
                                Text(
                                    text = selectedTab.labelText(),
                                    fontSize = 11.sp,
                                    color = RemotersColors.Muted,
                                    modifier = Modifier.padding(horizontal = 8.dp, vertical = 2.dp)
                                )
                            }
                        }
                    },
                    actions = {
                        LanguagePickerPill(
                            currentLocale = currentLocale,
                            onSelectLocale = onSelectLocale,
                            modifier = Modifier.padding(end = 12.dp)
                        )
                    }
                )
                HomeSessionTabStrip(
                    sessions = liveSessions,
                    onSelectSession = { session ->
                        SessionManager.switchTo(session.id)
                        onOpenSession(session.id)
                    },
                    onCloseSession = { session ->
                        SessionManager.closeSession(session.id)
                    }
                )
            }
        },
        bottomBar = {
            NavigationBar(
                containerColor = RemotersColors.Sidebar,
                tonalElevation = 0.dp
            ) {
                HomeTab.entries.forEach { tab ->
                    val icon = when (tab) {
                        HomeTab.Connect -> Icons.Rounded.CastConnected
                        HomeTab.Devices -> Icons.Rounded.Devices
                        HomeTab.Nearby -> Icons.Rounded.WifiFind
                        HomeTab.Account -> Icons.Rounded.Person
                    }
                    val tabLabel = tab.labelText()
                    NavigationBarItem(
                        selected = selectedTab == tab,
                        onClick = { selectedTab = tab },
                        icon = { Icon(icon, contentDescription = tabLabel) },
                        label = { Text(tabLabel, fontSize = 11.sp) },
                        colors = NavigationBarItemDefaults.colors(
                            selectedIconColor = RemotersColors.Accent,
                            selectedTextColor = RemotersColors.Accent,
                            indicatorColor = RemotersColors.AccentLight,
                            unselectedIconColor = RemotersColors.Muted,
                            unselectedTextColor = RemotersColors.Muted
                        )
                    )
                }
            }
        }
    ) { padding ->
        Box(
            modifier = Modifier
                .fillMaxSize()
                .background(RemotersColors.Canvas)
                .padding(padding)
        ) {
            when (selectedTab) {
                HomeTab.Connect -> ConnectTab(
                    address = address,
                    onAddressChange = {
                        address = it
                        selectedHostTitle = ""
                        connectError = null
                    },
                    pairingCode = pairingCode,
                    onCodeChange = { pairingCode = it; connectError = null },
                    nativeDisplaySize = nativeDisplaySize,
                    resolutionChoice = resolutionChoice,
                    onResolutionChange = { choice ->
                        resolutionChoice = choice
                        prefs.saveResolutionChoice(choice.name)
                    },
                    codecPref = codecPref,
                    onCodecChange = { codecPref = it },
                    hdrEnabled = hdrEnabled,
                    onHdrChange = { hdrEnabled = it },
                    maxFps = maxFps,
                    onFpsChange = { maxFps = it },
                    bitrateMbps = bitrateMbps,
                    onBitrateChange = { bitrateMbps = it },
                    framePacingMode = framePacingMode,
                    onFramePacingChange = { framePacingMode = it },
                    error = connectError,
                    onConnect = { launchWithPreferences(address, pairingCode, selectedHostTitle) }
                )

                HomeTab.Devices -> MyDevicesTab(
                    signedIn = accountSession.token.isNotBlank(),
                    devices = devices,
                    loading = devicesLoading,
                    error = devicesError,
                    onRefresh = { refreshDevices() },
                    onGoAccount = { selectedTab = HomeTab.Account },
                    onConnectDevice = { device ->
                        val hostAddr = device.hostAddress.orEmpty()
                        val code = device.pairingCode.orEmpty()
                        if (hostAddr.isNotBlank() && code.isNotBlank()) {
                            address = hostAddr
                            pairingCode = code
                            selectedHostTitle = device.name
                            launchWithPreferences(hostAddr, code, device.name)
                        } else {
                            address = hostAddr
                            selectedHostTitle = device.name
                            selectedTab = HomeTab.Connect
                            connectError = context.getString(R.string.err_device_missing_address_or_code)
                        }
                    },
                    onDeleteDevice = { device ->
                        scope.launch {
                            val result = withContext(Dispatchers.IO) {
                                runCatching {
                                    NativeBridge.accountDeleteDevice(
                                        accountSession.serverUrl,
                                        accountSession.token,
                                        device.deviceId
                                    )
                                }.fold(
                                    onSuccess = { JsonParsers.parseDeleteResult(it) },
                                    onFailure = { Result.failure(it) }
                                )
                            }
                            result.onSuccess {
                                devices = devices.filterNot { it.deviceId == device.deviceId }
                            }.onFailure {
                                devicesError = it.message ?: context.getString(R.string.err_remove_device)
                            }
                        }
                    }
                )

                HomeTab.Nearby -> NearbyTab(
                    hosts = nearbyHosts,
                    loading = nearbyLoading,
                    error = nearbyError,
                    onScan = { scanNearby() },
                    onSelectHost = { host ->
                        address = host.address
                        selectedHostTitle = host.hostName
                        selectedTab = HomeTab.Connect
                    }
                )

                HomeTab.Account -> AccountTab(
                    session = accountSession,
                    onSessionUpdated = { updated ->
                        accountSession = updated
                        prefs.saveSession(updated)
                    },
                    onServerUrlChanged = { url ->
                        accountSession = accountSession.copy(serverUrl = url)
                        prefs.saveServerUrl(url)
                    },
                    onSignOut = {
                        prefs.clearAuth()
                        accountSession = accountSession.copy(
                            token = "",
                            userId = "",
                            username = "",
                            displayName = ""
                        )
                        devices = emptyList()
                    }
                )
            }
        }
    }
}
