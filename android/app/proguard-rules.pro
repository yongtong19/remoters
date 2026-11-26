-keep class com.remoters.android.nativebridge.** { *; }
-keepclassmembers class * implements com.remoters.android.nativebridge.SessionCallback {
    *;
}
