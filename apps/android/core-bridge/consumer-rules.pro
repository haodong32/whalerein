# UniFFI/JNA bridge. Generated Kotlin and JNA callbacks are reached by name
# through JNI, so keep the bridge surface and native method names.
-keep class com.whalerein.android.corebridge.** { *; }
-keep class com.sun.jna.** { *; }
-keepclassmembers class * extends com.sun.jna.Structure { *; }
-keepclasseswithmembernames class * {
    native <methods>;
}
-dontwarn java.awt.**
-dontwarn com.sun.jna.**
