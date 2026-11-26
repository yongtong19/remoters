package com.remoters.android.ui.session

import android.annotation.SuppressLint
import android.content.Context
import android.text.InputType
import android.view.KeyEvent
import android.view.Surface
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.widget.FrameLayout
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.viewinterop.AndroidView
import com.remoters.android.input.InputMode
import com.remoters.android.input.TouchInputHandler
import com.remoters.android.input.computeFittedVideoRect
import com.remoters.android.session.ManagedSession
import com.remoters.android.session.ManagedSessionSnapshot
import com.remoters.android.session.SessionConnectionState
import com.remoters.android.session.SessionManager
import com.remoters.android.ui.NativeDisplaySize
import com.remoters.android.ui.RemotersColors
import com.remoters.android.ui.ResolutionChoice
import com.remoters.android.video.VideoDecoder
import kotlin.math.roundToInt

internal data class VideoHostViews(
    val container: FrameLayout,
    val surfaceView: SurfaceView,
    val keyboardProxy: SoftKeyboardProxyView
)

@SuppressLint("ClickableViewAccessibility")
internal fun buildSessionVideoHost(
    context: Context,
    touchHandler: TouchInputHandler,
    onSurfaceAvailable: (Surface) -> Unit,
    onSurfaceChanged: (Surface) -> Unit,
    onSurfaceDestroyed: () -> Unit
): VideoHostViews {
    val container = FrameLayout(context).apply {
        setBackgroundColor(android.graphics.Color.parseColor("#0E0F11"))
        isFocusable = true
        isFocusableInTouchMode = true
    }
    val surfaceView = SurfaceView(context).apply {
        holder.addCallback(object : SurfaceHolder.Callback {
            override fun surfaceCreated(holder: SurfaceHolder) {
                onSurfaceAvailable(holder.surface)
            }

            override fun surfaceChanged(
                holder: SurfaceHolder,
                format: Int,
                width: Int,
                height: Int
            ) {
                onSurfaceChanged(holder.surface)
            }

            override fun surfaceDestroyed(holder: SurfaceHolder) {
                onSurfaceDestroyed()
            }
        })
    }
    val lp = FrameLayout.LayoutParams(
        FrameLayout.LayoutParams.MATCH_PARENT,
        FrameLayout.LayoutParams.MATCH_PARENT,
        android.view.Gravity.CENTER
    )
    container.addView(surfaceView, lp)

    val imeProxy = SoftKeyboardProxyView(context, touchHandler)
    container.addView(imeProxy, FrameLayout.LayoutParams(1, 1))

    container.addOnLayoutChangeListener { _, left, top, right, bottom, _, _, _, _ ->
        val w = right - left
        val h = bottom - top
        touchHandler.updateViewSize(w, h)
        val active = SessionManager.activeSession
        applyAspectFitLayout(surfaceView, w, h, active?.frameWidth ?: 0, active?.frameHeight ?: 0)
    }
    container.setOnTouchListener { _, event ->
        val canControl = SessionManager.activeSession?.canControl ?: false
        if (canControl) touchHandler.onTouchEvent(event) else false
    }

    return VideoHostViews(container, surfaceView, imeProxy)
}

internal fun applyAspectFitLayout(
    surfaceView: SurfaceView,
    containerW: Int,
    containerH: Int,
    frameW: Int,
    frameH: Int
) {
    if (containerW <= 0 || containerH <= 0 || frameW <= 0 || frameH <= 0) return
    val rect = computeFittedVideoRect(containerW, containerH, frameW, frameH)
    val fittedW = rect.width.roundToInt().coerceAtLeast(1)
    val fittedH = rect.height.roundToInt().coerceAtLeast(1)
    val params = surfaceView.layoutParams as? FrameLayout.LayoutParams ?: return
    if (params.width != fittedW || params.height != fittedH) {
        params.width = fittedW
        params.height = fittedH
        params.gravity = android.view.Gravity.CENTER
        surfaceView.layoutParams = params
    }
}

internal class SoftKeyboardProxyView(
    context: Context,
    private val touchHandler: TouchInputHandler
) : View(context) {
    init {
        isFocusable = true
        isFocusableInTouchMode = true
    }

    override fun onCheckIsTextEditor(): Boolean = true

    override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection {
        outAttrs.inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
        outAttrs.imeOptions = EditorInfo.IME_FLAG_NO_EXTRACT_UI or EditorInfo.IME_ACTION_NONE
        return object : BaseInputConnection(this, false) {
            override fun commitText(text: CharSequence?, newCursorPosition: Int): Boolean {
                if (!text.isNullOrEmpty()) {
                    touchHandler.sendTypedText(text)
                }
                return true
            }

            override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
                repeat(beforeLength.coerceAtLeast(1)) {
                    touchHandler.handleKeyEvent(KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_DEL))
                    touchHandler.handleKeyEvent(KeyEvent(KeyEvent.ACTION_UP, KeyEvent.KEYCODE_DEL))
                }
                return true
            }

            override fun sendKeyEvent(event: KeyEvent): Boolean {
                return touchHandler.handleKeyEvent(event) || super.sendKeyEvent(event)
            }
        }
    }
}

@Composable
internal fun SessionScreen(
    sessions: List<ManagedSession>,
    activeSessionId: String,
    activeSnapshot: ManagedSessionSnapshot,
    nativeDisplaySize: NativeDisplaySize,
    selectedResolution: ResolutionChoice,
    pillExpanded: Boolean,
    gamepadCount: Int,
    inputMode: InputMode,
    cursorX: Int,
    cursorY: Int,
    dragLocked: Boolean,
    mouseBarMinimized: Boolean,
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
    onMouseLeftClick: () -> Unit,
    onMouseMiddleClick: () -> Unit,
    onMouseRightClick: () -> Unit,
    onToggleDragLock: () -> Unit,
    onToggleMouseBarMinimized: () -> Unit,
    onDisconnect: () -> Unit,
    createVideoHost: (Context) -> FrameLayout
) {
    val isConnecting = activeSnapshot.connectionState == SessionConnectionState.CONNECTING
    val closedError = if (activeSnapshot.connectionState == SessionConnectionState.CLOSED) {
        activeSnapshot.lastError
    } else {
        null
    }

    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(RemotersColors.SessionBg)
    ) {
        AndroidView(
            factory = createVideoHost,
            modifier = Modifier.fillMaxSize()
        )

        if (inputMode == InputMode.MOUSE && activeSnapshot.canControl) {
            VirtualMouseCursorOverlay(
                cursorX = cursorX,
                cursorY = cursorY,
                frameWidth = activeSnapshot.frameWidth,
                frameHeight = activeSnapshot.frameHeight,
                dragLocked = dragLocked
            )
        }

        if (isConnecting && closedError == null) {
            ConnectingOverlayCard(modifier = Modifier.align(Alignment.Center))
        }

        if (closedError != null) {
            SessionEndedCard(
                errorMessage = closedError,
                onClose = onDisconnect,
                modifier = Modifier.align(Alignment.Center)
            )
        }

        if (inputMode == InputMode.MOUSE && activeSnapshot.canControl && closedError == null) {
            MouseActionBar(
                dragLocked = dragLocked,
                minimized = mouseBarMinimized,
                onLeftClick = onMouseLeftClick,
                onMiddleClick = onMouseMiddleClick,
                onRightClick = onMouseRightClick,
                onToggleDragLock = onToggleDragLock,
                onToggleMinimized = onToggleMouseBarMinimized,
                modifier = Modifier.align(Alignment.BottomStart)
            )
        }

        SessionFloatingChrome(
            sessions = sessions,
            activeSessionId = activeSessionId,
            activeSnapshot = activeSnapshot,
            nativeDisplaySize = nativeDisplaySize,
            selectedResolution = selectedResolution,
            pillExpanded = pillExpanded,
            gamepadCount = gamepadCount,
            inputMode = inputMode,
            onToggleExpanded = onToggleExpanded,
            onSelectSession = onSelectSession,
            onCloseSession = onCloseSession,
            onGoHome = onGoHome,
            onToggleMute = onToggleMute,
            onToggleKeyboard = onToggleKeyboard,
            onSelectInputMode = onSelectInputMode,
            onSelectResolution = onSelectResolution,
            onSelectMaxFps = onSelectMaxFps,
            onSelectBitrate = onSelectBitrate,
            onSelectFramePacing = onSelectFramePacing,
            onDisconnect = onDisconnect,
            modifier = Modifier.align(Alignment.TopEnd)
        )
    }
}
