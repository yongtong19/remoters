package com.remoters.android.ui

import android.content.Context
import org.json.JSONObject

data class SavedAccountSession(
    val serverUrl: String,
    val token: String,
    val userId: String,
    val username: String,
    val displayName: String
)

data class AccountDevice(
    val deviceId: String,
    val name: String,
    val platform: String,
    val hostAddress: String?,
    val pairingCode: String?,
    val sharing: Boolean,
    val viewOnly: Boolean,
    val displayLabel: String,
    val lastSeenUnixMs: Long,
    val online: Boolean
)

data class NearbyHost(
    val instanceId: Long,
    val address: String,
    val hostName: String,
    val platform: String,
    val displayLabel: String,
    val viewOnly: Boolean
)

data class StreamStats(
    val width: Int = 0,
    val height: Int = 0,
    val targetFps: Int = 0,
    val codec: String = "—",
    val hdr: Boolean = false,
    val fps: Double = 0.0,
    val videoBps: Double = 0.0,
    val rttMs: Double = 0.0,
    val encodeMs: Double = 0.0,
    val e2eMs: Double = 0.0,
    val lostPackets: Long = 0L
)

class AccountPreferences(context: Context) {
    private val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    fun load(): SavedAccountSession {
        return SavedAccountSession(
            serverUrl = prefs.getString(KEY_SERVER_URL, DEFAULT_SERVER_URL) ?: DEFAULT_SERVER_URL,
            token = prefs.getString(KEY_TOKEN, "") ?: "",
            userId = prefs.getString(KEY_USER_ID, "") ?: "",
            username = prefs.getString(KEY_USERNAME, "") ?: "",
            displayName = prefs.getString(KEY_DISPLAY_NAME, "") ?: ""
        )
    }

    fun saveSession(session: SavedAccountSession) {
        prefs.edit()
            .putString(KEY_SERVER_URL, session.serverUrl)
            .putString(KEY_TOKEN, session.token)
            .putString(KEY_USER_ID, session.userId)
            .putString(KEY_USERNAME, session.username)
            .putString(KEY_DISPLAY_NAME, session.displayName)
            .apply()
    }

    fun saveServerUrl(serverUrl: String) {
        prefs.edit().putString(KEY_SERVER_URL, serverUrl).apply()
    }

    fun clearAuth() {
        prefs.edit()
            .remove(KEY_TOKEN)
            .remove(KEY_USER_ID)
            .remove(KEY_USERNAME)
            .remove(KEY_DISPLAY_NAME)
            .apply()
    }

    fun loadLastAddress(): String = prefs.getString(KEY_LAST_ADDRESS, "") ?: ""

    fun saveLastAddress(address: String) {
        prefs.edit().putString(KEY_LAST_ADDRESS, address).apply()
    }

    fun loadResolutionChoice(): String? = prefs.getString(KEY_RESOLUTION_CHOICE, null)

    fun saveResolutionChoice(choiceName: String) {
        prefs.edit().putString(KEY_RESOLUTION_CHOICE, choiceName).apply()
    }

    fun loadInputMode(): String? = prefs.getString(KEY_INPUT_MODE, null)

    fun saveInputMode(modeName: String) {
        prefs.edit().putString(KEY_INPUT_MODE, modeName).apply()
    }

    companion object {
        private const val PREFS_NAME = "remoters_account_prefs"
        const val DEFAULT_SERVER_URL = "http://127.0.0.1:8787"
        private const val KEY_SERVER_URL = "server_url"
        private const val KEY_TOKEN = "token"
        private const val KEY_USER_ID = "user_id"
        private const val KEY_USERNAME = "username"
        private const val KEY_DISPLAY_NAME = "display_name"
        private const val KEY_LAST_ADDRESS = "last_address"
        private const val KEY_RESOLUTION_CHOICE = "resolution_choice"
        private const val KEY_INPUT_MODE = "input_mode"
    }
}

object JsonParsers {
    fun parseAuthSession(raw: String, serverUrl: String): Result<SavedAccountSession> {
        return runCatching {
            val obj = JSONObject(raw)
            if (!obj.optBoolean("ok", false)) {
                error(obj.optString("error", "Authentication failed"))
            }
            val token = obj.getString("token")
            val user = obj.getJSONObject("user")
            SavedAccountSession(
                serverUrl = serverUrl,
                token = token,
                userId = user.optString("id", ""),
                username = user.optString("username", ""),
                displayName = user.optString("display_name", "")
            )
        }
    }

    fun parseDevices(raw: String): Result<List<AccountDevice>> {
        return runCatching {
            val obj = JSONObject(raw)
            if (!obj.optBoolean("ok", false)) {
                error(obj.optString("error", "Failed to list devices"))
            }
            val array = obj.optJSONArray("devices") ?: return@runCatching emptyList()
            buildList(array.length()) {
                for (i in 0 until array.length()) {
                    val item = array.getJSONObject(i)
                    add(
                        AccountDevice(
                            deviceId = item.optString("device_id", ""),
                            name = item.optString("name", "Device"),
                            platform = item.optString("platform", "Desktop"),
                            hostAddress = item.optString("host_address", "").ifBlank { null },
                            pairingCode = item.optString("pairing_code", "").ifBlank { null },
                            sharing = item.optBoolean("sharing", false),
                            viewOnly = item.optBoolean("view_only", false),
                            displayLabel = item.optString("display_label", ""),
                            lastSeenUnixMs = item.optLong("last_seen_unix_ms", 0L),
                            online = item.optBoolean("online", false)
                        )
                    )
                }
            }
        }
    }

    fun parseDeleteResult(raw: String): Result<Unit> {
        return runCatching {
            val obj = JSONObject(raw)
            if (!obj.optBoolean("ok", false)) {
                error(obj.optString("error", "Failed to remove device"))
            }
        }
    }

    fun parseNearbyHosts(raw: String): Result<List<NearbyHost>> {
        return runCatching {
            val obj = JSONObject(raw)
            if (!obj.optBoolean("ok", false)) {
                error(obj.optString("error", "Discovery failed"))
            }
            val array = obj.optJSONArray("hosts") ?: return@runCatching emptyList()
            buildList(array.length()) {
                for (i in 0 until array.length()) {
                    val item = array.getJSONObject(i)
                    add(
                        NearbyHost(
                            instanceId = item.optLong("instance_id", 0L),
                            address = item.optString("address", ""),
                            hostName = item.optString("host_name", "Host"),
                            platform = item.optString("platform", "Desktop"),
                            displayLabel = item.optString("display_label", ""),
                            viewOnly = item.optBoolean("view_only", false)
                        )
                    )
                }
            }
        }
    }

    fun parseStats(raw: String): StreamStats? {
        return runCatching {
            val obj = JSONObject(raw)
            if (!obj.optBoolean("ok", true)) return null
            StreamStats(
                width = obj.optInt("width", 0),
                height = obj.optInt("height", 0),
                targetFps = obj.optInt("targetFps", 0),
                codec = obj.optString("codec", "—"),
                hdr = obj.optBoolean("hdr", false),
                fps = obj.optDouble("fps", 0.0),
                videoBps = obj.optDouble("videoBps", 0.0),
                rttMs = obj.optDouble("rttMs", 0.0),
                encodeMs = obj.optDouble("encodeMs", 0.0),
                e2eMs = obj.optDouble("e2eMs", 0.0),
                lostPackets = obj.optLong("lostPackets", 0L)
            )
        }.getOrNull()
    }
}
