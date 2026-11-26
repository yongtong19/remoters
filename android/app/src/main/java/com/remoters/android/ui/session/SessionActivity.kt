package com.remoters.android.ui.session

import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.Surface
import android.view.WindowManager
import android.view.inputmethod.InputMethodManager
import android.widget.FrameLayout
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.remoters.android.i18n.AppLocaleManager
import com.remoters.android.i18n.LocalizedAppContent
import com.remoters.android.input.GamepadHandler
import com.remoters.android.input.InputMode
import com.remoters.android.input.InputSink
import com.remoters.android.input.TouchInputHandler
import com.remoters.android.session.ManagedSession
import com.remoters.android.session.SessionConnectionState
import com.remoters.android.session.SessionLaunchRequest
import com.remoters.android.session.SessionManager
import com.remoters.android.ui.AccountPreferences
import com.remoters.android.ui.MainActivity
import com.remoters.android.ui.NativeDisplaySize
import com.remoters.android.ui.RemotersTheme
import com.remoters.android.ui.ResolutionChoice
import com.remoters.android.ui.detectNativeLandscapeSize
import com.remoters.android.video.VideoDecoder

class SessionActivity : ComponentActivity() {

    private lateinit var accountPrefs: AccountPreferences
    private lateinit var touchHandler: TouchInputHandler
    private lateinit var gamepadHandler: GamepadHandler
    private var videoHost: VideoHostViews? = null
    private var currentSurface: Surface? = null
    private var nativeDisplaySize: NativeDisplaySize = NativeDisplaySize(1920, 1080)

    private var pillExpanded by mutableStateOf(false)
    private var inputMode by mutableStateOf(InputMode.TOUCH)
    private var cursorX by mutableIntStateOf(TouchInputHandler.CENTER_COORD)
    private var cursorY by mutableIntStateOf(TouchInputHandler.CENTER_COORD)
    private var dragLocked by mutableStateOf(false)
    private var mouseBarMinimized by mutableStateOf(false)

    override fun attachBaseContext(newBase: Context) {
        super.attachBaseContext(AppLocaleManager.wrapContext(newBase))
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        AppLocaleManager.resolveLocale(this)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            window.attributes = window.attributes.apply {
                layoutInDisplayCutoutMode =
                    WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES
            }
        }
        hideSystemBars()

        accountPrefs = AccountPreferences(this)
        nativeDisplaySize = detectNativeLandscapeSize(this)
        inputMode = InputMode.fromStoredName(accountPrefs.loadInputMode())

        touchHandler = TouchInputHandler(
            sink = object : InputSink {
                override fun sendPointerMove(x: Int, y: Int) {
                    SessionManager.sendPointerMove(x, y)
                }

                override fun sendPointerButton(x: Int, y: Int, button: Int, down: Boolean) {
                    SessionManager.sendPointerButton(x, y, button, down)
                }

                override fun sendScroll(x: Int, y: Int, vertical: Int, horizontal: Int) {
                    SessionManager.sendScroll(x, y, vertical, horizontal)
                }

                override fun sendKey(vkCode: Int, down: Boolean) {
                    SessionManager.sendKey(vkCode, down)
                }
            },
            onCursorChanged = { x, y ->
                cursorX = x
                cursorY = y
            },
            onDragLockChanged = { locked ->
                dragLocked = locked
            }
        )
        touchHandler.setInputMode(inputMode)

        gamepadHandler = GamepadHandler(this) { gp ->
            SessionManager.sendGamepad(
                gp.slot,
                gp.connected,
                gp.buttons,
                gp.lt,
                gp.rt,
                gp.lx,
                gp.ly,
                gp.rx,
                gp.ry
            )
        }
        gamepadHandler.start()

        SessionManager.setListeners(
            onFrameSizeChanged = { sessionId, width, height ->
                if (sessionId == SessionManager.activeSessionId.value) {
                    touchHandler.updateFrameSize(width, height)
                    runOnUiThread { applyActiveAspectFit(width, height) }
                }
            },
            onSessionStarted = { session ->
                if (session.id == SessionManager.activeSessionId.value) {
                    syncActiveGamepadsToSession()
                }
            }
        )

        handleLaunchIntent(intent)

        if (SessionManager.activeSession == null) {
            finish()
            return
        }

        setContent {
            val currentLocale by AppLocaleManager.currentLocale.collectAsState()
            val sessions by SessionManager.sessions.collectAsState()
            val activeSessionId by SessionManager.activeSessionId.collectAsState()
            val activeSession = sessions.firstOrNull { it.id == activeSessionId }
            val gamepadCount by gamepadHandler.connectedCount.collectAsState()

            LaunchedEffect(sessions.size, activeSessionId) {
                if (sessions.isEmpty() || activeSession == null) {
                    finish()
                } else {
                    syncActiveSessionMetrics(activeSession)
                }
            }

            LocalizedAppContent(locale = currentLocale) {
                RemotersTheme {
                    if (activeSession != null) {
                        val snapshot by activeSession.state.collectAsState()
                        LaunchedEffect(snapshot.connectionState, snapshot.lastError) {
                            if (snapshot.connectionState == SessionConnectionState.CLOSED &&
                                snapshot.lastError.isNullOrBlank()
                            ) {
                                closeSessionOrFinish(activeSession.id)
                            }
                        }

                        val selectedResolution = ResolutionChoice.fromDimensions(
                            width = snapshot.requestedWidth,
                            height = snapshot.requestedHeight,
                            nativeSize = nativeDisplaySize
                        )

                        SessionScreen(
                            sessions = sessions,
                            activeSessionId = activeSession.id,
                            activeSnapshot = snapshot,
                            nativeDisplaySize = nativeDisplaySize,
                            selectedResolution = selectedResolution,
                            pillExpanded = pillExpanded,
                            gamepadCount = gamepadCount,
                            inputMode = inputMode,
                            cursorX = cursorX,
                            cursorY = cursorY,
                            dragLocked = dragLocked,
                            mouseBarMinimized = mouseBarMinimized,
                            onToggleExpanded = { pillExpanded = !pillExpanded },
                            onSelectSession = { targetId ->
                                touchHandler.cancelActiveGestures()
                                touchHandler.setDragLocked(false)
                                SessionManager.switchTo(targetId)
                                SessionManager.getSession(targetId)?.let { switched ->
                                    syncActiveSessionMetrics(switched)
                                    syncActiveGamepadsToSession()
                                }
                            },
                            onCloseSession = { targetId ->
                                closeSessionOrFinish(targetId)
                            },
                            onGoHome = { goHomeKeepingSessionsAlive() },
                            onToggleMute = {
                                SessionManager.setAudioMuted(activeSession.id, !snapshot.audioMuted)
                            },
                            onToggleKeyboard = { toggleSoftKeyboard() },
                            onSelectInputMode = { nextMode ->
                                inputMode = nextMode
                                accountPrefs.saveInputMode(nextMode.name)
                                touchHandler.setInputMode(nextMode)
                            },
                            onSelectResolution = { choice ->
                                accountPrefs.saveResolutionChoice(choice.name)
                                val (targetW, targetH) = choice.resolveDimensions(nativeDisplaySize)
                                SessionManager.reconfigureStream(
                                    sessionId = activeSession.id,
                                    width = targetW,
                                    height = targetH,
                                    maxFps = snapshot.requestedMaxFps
                                )
                            },
                            onSelectMaxFps = { fps ->
                                SessionManager.reconfigureStream(
                                    sessionId = activeSession.id,
                                    width = snapshot.requestedWidth,
                                    height = snapshot.requestedHeight,
                                    maxFps = fps
                                )
                            },
                            onSelectBitrate = { mbps ->
                                SessionManager.setBitrate(activeSession.id, mbps)
                            },
                            onSelectFramePacing = { mode ->
                                SessionManager.setFramePacingMode(activeSession.id, mode)
                            },
                            onMouseLeftClick = {
                                touchHandler.clickButton(TouchInputHandler.BUTTON_LEFT)
                            },
                            onMouseMiddleClick = {
                                touchHandler.clickButton(TouchInputHandler.BUTTON_MIDDLE)
                            },
                            onMouseRightClick = {
                                touchHandler.clickButton(TouchInputHandler.BUTTON_RIGHT)
                            },
                            onToggleDragLock = {
                                touchHandler.setDragLocked(!dragLocked)
                            },
                            onToggleMouseBarMinimized = {
                                mouseBarMinimized = !mouseBarMinimized
                            },
                            onDisconnect = {
                                closeSessionOrFinish(activeSession.id)
                            },
                            createVideoHost = { ctx -> createVideoContainer(ctx) }
                        )
                    }
                }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleLaunchIntent(intent)
        SessionManager.activeSession?.let { active ->
            syncActiveSessionMetrics(active)
            syncActiveGamepadsToSession()
        }
    }

    override fun onResume() {
        super.onResume()
        hideSystemBars()
        SessionManager.attachForeground(currentSurface)
        SessionManager.activeSession?.let { active ->
            syncActiveSessionMetrics(active)
            syncActiveGamepadsToSession()
        }
    }

    override fun onPause() {
        super.onPause()
        touchHandler.cancelActiveGestures()
        touchHandler.setDragLocked(false)
        SessionManager.detachForeground()
    }

    override fun onDestroy() {
        super.onDestroy()
        SessionManager.setListeners(null, null)
        SessionManager.detachForeground()
        gamepadHandler.stop()
    }

    private fun handleLaunchIntent(launchIntent: Intent?) {
        val targetId = launchIntent?.getStringExtra(EXTRA_SESSION_ID)
        if (!targetId.isNullOrBlank()) {
            SessionManager.switchTo(targetId)
            return
        }
        val address = launchIntent?.getStringExtra(EXTRA_ADDRESS).orEmpty()
        val code = launchIntent?.getStringExtra(EXTRA_CODE).orEmpty()
        if (address.isNotBlank() || code.isNotBlank()) {
            val pacingName = launchIntent?.getStringExtra(EXTRA_FRAME_PACING)
            val pacingMode = VideoDecoder.FramePacingMode.entries.firstOrNull {
                it.name == pacingName
            } ?: VideoDecoder.FramePacingMode.LOWEST_LATENCY
            SessionManager.connectOrSwitch(
                SessionLaunchRequest(
                    title = launchIntent?.getStringExtra(EXTRA_TITLE).orEmpty(),
                    address = address,
                    code = code,
                    width = launchIntent?.getIntExtra(EXTRA_WIDTH, 0) ?: 0,
                    height = launchIntent?.getIntExtra(EXTRA_HEIGHT, 0) ?: 0,
                    codecPref = launchIntent?.getIntExtra(EXTRA_CODEC_PREF, 0) ?: 0,
                    supportedCodecs = launchIntent?.getIntExtra(EXTRA_SUPPORTED_CODECS, -1) ?: -1,
                    hdr = launchIntent?.getBooleanExtra(EXTRA_HDR, false) ?: false,
                    maxFps = launchIntent?.getIntExtra(EXTRA_MAX_FPS, 0) ?: 0,
                    bitrateMbps = launchIntent?.getIntExtra(EXTRA_BITRATE_MBPS, 0) ?: 0,
                    framePacingMode = pacingMode
                )
            )
        }
    }

    private fun createVideoContainer(ctx: Context): FrameLayout {
        val built = buildSessionVideoHost(
            context = ctx,
            touchHandler = touchHandler,
            onSurfaceAvailable = { surface ->
                currentSurface = surface
                SessionManager.attachForeground(surface)
            },
            onSurfaceChanged = { surface ->
                currentSurface = surface
                SessionManager.updateForegroundSurface(surface)
            },
            onSurfaceDestroyed = {
                currentSurface = null
                SessionManager.updateForegroundSurface(null)
            }
        )
        videoHost = built
        return built.container
    }

    private fun syncActiveSessionMetrics(session: ManagedSession) {
        val w = session.frameWidth
        val h = session.frameHeight
        if (w > 0 && h > 0) {
            touchHandler.updateFrameSize(w, h)
            applyActiveAspectFit(w, h)
        }
    }

    private fun syncActiveGamepadsToSession() {
        gamepadHandler.activeSnapshots().forEach { gp ->
            SessionManager.sendGamepad(
                gp.slot,
                gp.connected,
                gp.buttons,
                gp.lt,
                gp.rt,
                gp.lx,
                gp.ly,
                gp.rx,
                gp.ry
            )
        }
    }

    private fun closeSessionOrFinish(sessionId: String) {
        val nextId = SessionManager.closeSession(sessionId)
        if (nextId == null) {
            finish()
        } else {
            SessionManager.getSession(nextId)?.let { next ->
                syncActiveSessionMetrics(next)
                syncActiveGamepadsToSession()
            }
        }
    }

    private fun goHomeKeepingSessionsAlive() {
        pillExpanded = false
        SessionManager.detachForeground()
        val intent = Intent(this, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP
        }
        startActivity(intent)
        finish()
    }

    private fun applyActiveAspectFit(frameW: Int, frameH: Int) {
        val host = videoHost ?: return
        applyAspectFitLayout(
            host.surfaceView,
            host.container.width,
            host.container.height,
            frameW,
            frameH
        )
    }

    private fun toggleSoftKeyboard() {
        val proxy = videoHost?.keyboardProxy ?: return
        proxy.requestFocus()
        val imm = getSystemService(Context.INPUT_METHOD_SERVICE) as InputMethodManager
        imm.showSoftInput(proxy, InputMethodManager.SHOW_IMPLICIT)
    }

    @Suppress("DEPRECATION")
    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        if (gamepadHandler.handleKeyEvent(event)) {
            return true
        }
        val canControl = SessionManager.activeSession?.canControl ?: false
        if (canControl &&
            event.keyCode != KeyEvent.KEYCODE_BACK &&
            event.keyCode != KeyEvent.KEYCODE_VOLUME_UP &&
            event.keyCode != KeyEvent.KEYCODE_VOLUME_DOWN &&
            (event.device?.sources?.and(InputDevice.SOURCE_KEYBOARD) == InputDevice.SOURCE_KEYBOARD ||
                event.action == KeyEvent.ACTION_MULTIPLE)
        ) {
            if (touchHandler.handleKeyEvent(event)) {
                return true
            }
        }
        if (event.keyCode == KeyEvent.KEYCODE_BACK && event.action == KeyEvent.ACTION_UP) {
            val active = SessionManager.activeSession
            if (pillExpanded) {
                pillExpanded = false
            } else if (active != null && active.connectionState == SessionConnectionState.CLOSED) {
                closeSessionOrFinish(active.id)
            } else {
                goHomeKeepingSessionsAlive()
            }
            return true
        }
        return super.dispatchKeyEvent(event)
    }

    override fun dispatchGenericMotionEvent(event: MotionEvent): Boolean {
        if (gamepadHandler.handleMotionEvent(event)) {
            return true
        }
        return super.dispatchGenericMotionEvent(event)
    }

    private fun hideSystemBars() {
        WindowCompat.setDecorFitsSystemWindows(window, false)
        WindowInsetsControllerCompat(window, window.decorView).apply {
            hide(WindowInsetsCompat.Type.systemBars())
            systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        }
    }

    companion object {
        const val EXTRA_SESSION_ID = "session_id"
        const val EXTRA_TITLE = "title"
        const val EXTRA_ADDRESS = "address"
        const val EXTRA_CODE = "code"
        const val EXTRA_WIDTH = "width"
        const val EXTRA_HEIGHT = "height"
        const val EXTRA_CODEC_PREF = "codec_pref"
        const val EXTRA_SUPPORTED_CODECS = "supported_codecs"
        const val EXTRA_HDR = "hdr"
        const val EXTRA_MAX_FPS = "max_fps"
        const val EXTRA_BITRATE_MBPS = "bitrate_mbps"
        const val EXTRA_FRAME_PACING = "frame_pacing"
    }
}
