package com.remoters.android.i18n

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File
import java.util.Locale
import javax.xml.parsers.DocumentBuilderFactory

class AppLocaleTest {

    private class InMemoryLocaleStorage(var saved: String? = null) : LocaleStorage {
        override fun readSavedCode(): String? = saved
        override fun writeSavedCode(code: String?) {
            saved = code
        }
    }

    @Test
    fun parsesAllSixSupportedLocalesAndVariants() {
        assertEquals(AppLocale.EN, AppLocale.parse("en"))
        assertEquals(AppLocale.EN, AppLocale.parse("en-US"))
        assertEquals(AppLocale.EN, AppLocale.parse("EN_gb"))

        assertEquals(AppLocale.ZH_CN, AppLocale.parse("zh"))
        assertEquals(AppLocale.ZH_CN, AppLocale.parse("zh-CN"))
        assertEquals(AppLocale.ZH_CN, AppLocale.parse("zh_CN"))
        assertEquals(AppLocale.ZH_CN, AppLocale.parse("zh-Hans-CN"))

        assertEquals(AppLocale.ES, AppLocale.parse("es"))
        assertEquals(AppLocale.ES, AppLocale.parse("es-MX"))
        assertEquals(AppLocale.ES, AppLocale.parse("es_ES"))

        assertEquals(AppLocale.FR, AppLocale.parse("fr"))
        assertEquals(AppLocale.FR, AppLocale.parse("fr-CA"))

        assertEquals(AppLocale.JA, AppLocale.parse("ja"))
        assertEquals(AppLocale.JA, AppLocale.parse("ja-JP"))

        assertEquals(AppLocale.KO, AppLocale.parse("ko"))
        assertEquals(AppLocale.KO, AppLocale.parse("ko-KR"))

        assertNull(AppLocale.parse(null))
        assertNull(AppLocale.parse(""))
        assertNull(AppLocale.parse("   "))
        assertNull(AppLocale.parse("system"))
        assertNull(AppLocale.parse("auto"))
        assertNull(AppLocale.parse("de-DE"))
    }

    @Test
    fun detectsSystemLocaleWithEnglishFallback() {
        assertEquals(AppLocale.ZH_CN, AppLocale.fromJavaLocale(Locale.SIMPLIFIED_CHINESE))
        assertEquals(AppLocale.ES, AppLocale.fromJavaLocale(Locale.forLanguageTag("es-AR")))
        assertEquals(AppLocale.FR, AppLocale.fromJavaLocale(Locale.FRANCE))
        assertEquals(AppLocale.JA, AppLocale.fromJavaLocale(Locale.JAPAN))
        assertEquals(AppLocale.KO, AppLocale.fromJavaLocale(Locale.KOREA))
        assertEquals(AppLocale.EN, AppLocale.fromJavaLocale(Locale.US))
        assertEquals(AppLocale.EN, AppLocale.fromJavaLocale(Locale.GERMANY))
        assertEquals(AppLocale.EN, AppLocale.fromJavaLocale(null))
    }

    @Test
    fun manualOverrideTakesPrecedenceOverSystemAndPersists() {
        val storage = InMemoryLocaleStorage()
        var systemLocale = Locale.JAPAN
        val controller = LocalePreferencesController(
            storage = storage,
            systemLocaleProvider = { systemLocale }
        )

        // System default auto-detected as Japanese
        assertNull(controller.overrideLocale.value)
        assertEquals(AppLocale.JA, controller.currentLocale.value)

        // Manual override persists across re-creation
        controller.setLocale(AppLocale.ZH_CN)
        assertEquals("zh-CN", storage.saved)
        assertEquals(AppLocale.ZH_CN, controller.overrideLocale.value)
        assertEquals(AppLocale.ZH_CN, controller.currentLocale.value)

        val reloaded = LocalePreferencesController(
            storage = storage,
            systemLocaleProvider = { systemLocale }
        )
        assertEquals(AppLocale.ZH_CN, reloaded.currentLocale.value)

        // Clearing override falls back to current system locale
        systemLocale = Locale.KOREA
        reloaded.setLocale(null)
        assertNull(storage.saved)
        assertNull(reloaded.overrideLocale.value)
        assertEquals(AppLocale.KO, reloaded.currentLocale.value)
    }

    @Test
    fun exposesAllSixShortLabelsAndNativeNames() {
        assertEquals(6, AppLocale.entries.size)
        assertEquals(
            listOf("EN", "中文", "ES", "FR", "JA", "KO"),
            AppLocale.entries.map { it.shortLabel }
        )
        assertEquals(
            listOf("English", "简体中文", "Español", "Français", "日本語", "한국어"),
            AppLocale.entries.map { it.nativeName }
        )
        // Round-trip every entry's code and java Locale
        for (locale in AppLocale.entries) {
            assertEquals(locale, AppLocale.parse(locale.code))
            assertEquals(locale, AppLocale.fromJavaLocale(locale.toJavaLocale()))
        }
    }

    @Test
    fun allSixLanguageStringResourcesHaveMatchingNonEmptyKeys() {
        val resRoot = listOf(
            File("src/main/res"),
            File("app/src/main/res")
        ).first { it.isDirectory }

        val folders = listOf(
            "values",
            "values-zh-rCN",
            "values-es",
            "values-fr",
            "values-ja",
            "values-ko"
        )

        fun parseStringKeys(file: File): Map<String, String> {
            val doc = DocumentBuilderFactory.newInstance().newDocumentBuilder().parse(file)
            val nodes = doc.getElementsByTagName("string")
            val result = LinkedHashMap<String, String>()
            for (i in 0 until nodes.length) {
                val node = nodes.item(i)
                val name = node.attributes.getNamedItem("name").nodeValue
                result[name] = node.textContent.trim()
            }
            return result
        }

        val baseStrings = parseStringKeys(File(resRoot, "values/strings.xml"))
        assertTrue(baseStrings.containsKey("opt_native_resolution"))
        assertTrue(baseStrings.containsKey("input_mode_mouse"))
        assertTrue(baseStrings.containsKey("mouse_btn_drag"))

        for (folder in folders) {
            val file = File(resRoot, "$folder/strings.xml")
            assertTrue("Missing $folder/strings.xml", file.isFile)
            val map = parseStringKeys(file)
            assertEquals("Key mismatch in $folder/strings.xml", baseStrings.keys, map.keys)
            for ((k, v) in map) {
                assertTrue("Empty string $k in $folder/strings.xml", v.isNotEmpty())
            }
        }
    }
}
