# UniFFI/JNA bridge entry points and JNI-resolved native methods.
-keep class com.whalerein.android.corebridge.** { *; }
-keep class com.sun.jna.** { *; }
-keepclassmembers class * extends com.sun.jna.Structure { *; }
-keepclasseswithmembernames class * {
    native <methods>;
}
-dontwarn java.awt.**
-dontwarn com.sun.jna.**

# Keep the generated provenance metadata readable in the artifact.
-keep class com.whalerein.android.corebridge.CoreProvenance { *; }
