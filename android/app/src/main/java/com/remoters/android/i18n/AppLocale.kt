package com.remoters.android.i18n

import android.content.Context
import android.content.res.Configuration
import android.os.LocaleList
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import java.util.Locale

enum class AppLocale(
    val code: String,
    val shortLabel: String,
    val nativeName: String
) {
    EN(code = "en", shortLabel = "EN", nativeName = "English"),
    ZH_CN(code = "zh-CN", shortLabel = "中文", nativeName = "简体中文"),
    ES(code = "es", shortLabel = "ES", nativeName = "Español"),
    FR(code = "fr", shortLabel = "FR", nativeName = "Français"),
    JA(code = "ja", shortLabel = "JA", nativeName = "日本語"),
    KO(code = "ko", shortLabel = "KO", nativeName = "한국어");

    fun toJavaLocale(): Locale = when (this) {
        EN -> Locale.ENGLISH
        ZH_CN -> Locale.SIMPLIFIED_CHINESE
        ES -> Locale.forLanguageTag("es")
        FR -> Locale.FRENCH
        JA -> Locale.JAPANESE
        KO -> Locale.KOREAN
    }

    companion object {
        fun parse(raw: String?): AppLocale? {
            val normalized = raw?.trim()?.replace('_', '-')?.lowercase(Locale.ROOT)
                ?: return null
            if (normalized.isEmpty() || normalized == "system" || normalized == "auto") {
                return null
            }
            return when {
                normalized == "zh" || normalized.startsWith("zh-") -> ZH_CN
                normalized == "es" || normalized.startsWith("es-") -> ES
                normalized == "fr" || normalized.startsWith("fr-") -> FR
                normalized == "ja" || normalized.startsWith("ja-") -> JA
                normalized == "ko" || normalized.startsWith("ko-") -> KO
                normalized == "en" || normalized.startsWith("en-") -> EN
                else -> null
            }
        }

        fun fromJavaLocale(locale: Locale?): AppLocale {
            if (locale == null) return EN
            parse(locale.toLanguageTag())?.let { return it }
            return parse(locale.language) ?: EN
        }

        fun resolve(savedCode: String?, systemLocale: Locale = Locale.getDefault()): AppLocale {
            return parse(savedCode) ?: fromJavaLocale(systemLocale)
        }
    }
}

interface LocaleStorage {
    fun readSavedCode(): String?
    fun writeSavedCode(code: String?)
}

class LocalePreferencesController(
    private val storage: LocaleStorage,
    private val systemLocaleProvider: () -> Locale = { Locale.getDefault() }
) {
    private val _overrideLocale = MutableStateFlow(AppLocale.parse(storage.readSavedCode()))
    val overrideLocale: StateFlow<AppLocale?> = _overrideLocale.asStateFlow()

    private val _currentLocale = MutableStateFlow(resolveFromStorage())
    val currentLocale: StateFlow<AppLocale> = _currentLocale.asStateFlow()

    fun resolveFromStorage(): AppLocale {
        return AppLocale.resolve(storage.readSavedCode(), systemLocaleProvider())
    }

    fun refreshFromStorage(): AppLocale {
        val saved = AppLocale.parse(storage.readSavedCode())
        val resolved = saved ?: AppLocale.fromJavaLocale(systemLocaleProvider())
        _overrideLocale.value = saved
        _currentLocale.value = resolved
        return resolved
    }

    fun setLocale(locale: AppLocale?): AppLocale {
        storage.writeSavedCode(locale?.code)
        _overrideLocale.value = locale
        val resolved = locale ?: AppLocale.fromJavaLocale(systemLocaleProvider())
        _currentLocale.value = resolved
        return resolved
    }
}

private class SharedPrefsLocaleStorage(context: Context) : LocaleStorage {
    private val prefs = context.applicationContext
        .getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    override fun readSavedCode(): String? = prefs.getString(KEY_LOCALE, null)

    override fun writeSavedCode(code: String?) {
        prefs.edit().apply {
            if (code == null) remove(KEY_LOCALE) else putString(KEY_LOCALE, code)
        }.apply()
    }

    companion object {
        private const val PREFS_NAME = "remoters_locale_prefs"
        private const val KEY_LOCALE = "app_locale"
    }
}

object AppLocaleManager {
    private const val PREFS_NAME = "remoters_locale_prefs"
    private const val KEY_LOCALE = "app_locale"

    @Volatile
    private var controller: LocalePreferencesController? = null

    private val _currentLocale = MutableStateFlow(AppLocale.fromJavaLocale(Locale.getDefault()))
    val currentLocale: StateFlow<AppLocale> = _currentLocale.asStateFlow()

    private fun ensureController(context: Context): LocalePreferencesController {
        controller?.let { return it }
        synchronized(this) {
            controller?.let { return it }
            val created = LocalePreferencesController(SharedPrefsLocaleStorage(context))
            controller = created
            _currentLocale.value = created.currentLocale.value
            return created
        }
    }

    fun readSavedCode(context: Context): String? {
        return context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .getString(KEY_LOCALE, null)
    }

    fun resolveLocale(context: Context): AppLocale {
        val saved = readSavedCode(context)
        val systemLocale = context.resources.configuration.locales.let { list ->
            if (!list.isEmpty) list[0] else Locale.getDefault()
        }
        val resolved = AppLocale.resolve(saved, systemLocale)
        _currentLocale.value = resolved
        return resolved
    }

    fun setLocale(context: Context, locale: AppLocale?): AppLocale {
        val resolved = ensureController(context).setLocale(locale)
        _currentLocale.value = resolved
        return resolved
    }

    fun getOverrideLocale(context: Context): AppLocale? {
        return ensureController(context).overrideLocale.value
    }

    fun wrapContext(base: Context): Context {
        val locale = resolveLocale(base)
        return createLocalizedContext(base, locale)
    }

    fun createLocalizedContext(base: Context, locale: AppLocale): Context {
        val javaLocale = locale.toJavaLocale()
        val config = Configuration(base.resources.configuration)
        config.setLocale(javaLocale)
        config.setLocales(LocaleList(javaLocale))
        return base.createConfigurationContext(config)
    }
}

val LocalAppLocale = compositionLocalOf { AppLocale.EN }

@Composable
fun LocalizedAppContent(
    locale: AppLocale,
    content: @Composable () -> Unit
) {
    val baseContext = LocalContext.current
    val localizedContext = remember(baseContext, locale) {
        AppLocaleManager.createLocalizedContext(baseContext, locale)
    }
    val localizedConfig = remember(localizedContext) {
        Configuration(localizedContext.resources.configuration)
    }
    CompositionLocalProvider(
        LocalAppLocale provides locale,
        LocalContext provides localizedContext,
        LocalConfiguration provides localizedConfig,
        content = content
    )
}
