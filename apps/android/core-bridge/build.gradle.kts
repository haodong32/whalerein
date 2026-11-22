import org.gradle.api.tasks.Exec
import org.gradle.api.tasks.Copy
import java.util.Properties

plugins {
    alias(libs.plugins.android.library)
    alias(libs.plugins.kotlin.serialization)
}

val repoRoot = rootProject.projectDir.parentFile.parentFile
val crateDir = repoRoot.resolve("apps/mobile-bindings")
val androidTarget = "aarch64-linux-android"
val ndkVersion = "28.2.13676358"

// The Core package version is the workspace version, matching build.rs.
val workspaceVersion: String = run {
    val manifest = repoRoot.resolve("Cargo.toml").readText()
    Regex("""(?m)^version\s*=\s*"([^"]+)"""")
        .find(manifest.substringAfter("[workspace.package]"))
        ?.groupValues
        ?.get(1)
        ?: "unknown"
}

// Provenance must match the value compiled into the Rust library. build.rs
// prefers WHALEREIN_MOBILE_SOURCE and appends the Core version, so the Gradle
// task passes the same source marker it renders into BuildConfig.
fun git(vararg args: String): String? = try {
    val process = ProcessBuilder("git", *args)
        .directory(repoRoot)
        .redirectErrorStream(true)
        .start()
    val output = process.inputStream.bufferedReader().readText().trim()
    if (process.waitFor() == 0) output.ifEmpty { null } else null
} catch (_: Exception) {
    null
}

fun defaultSource(): String {
    val head = git("rev-parse", "HEAD")?.take(12) ?: "unknown"
    val dirty = git("status", "--porcelain")?.isNotEmpty() == true
    return "git:$head${if (dirty) "-dirty" else ""}"
}

val mobileSource: String = providers.gradleProperty("whalerein.mobileSource")
    .orElse(providers.environmentVariable("WHALEREIN_MOBILE_SOURCE"))
    .getOrElse(defaultSource())
val coreRevision = "source:$mobileSource core:$workspaceVersion"
val appVersion: String = providers.gradleProperty("whalerein.appVersion").getOrElse("0.1.0")

val ndkDir: File = run {
    val configured = providers.gradleProperty("android.ndkPath").orNull
    if (configured != null) {
        file(configured)
    } else {
        val sdk = providers.gradleProperty("sdk.dir").orNull
            ?: System.getenv("ANDROID_HOME")
            ?: System.getenv("ANDROID_SDK_ROOT")
            ?: "${System.getProperty("user.home")}/Library/Android/sdk"
        file(sdk).resolve("ndk/$ndkVersion")
    }
}
val hostTag = when {
    System.getProperty("os.name").startsWith("Mac") -> "darwin-x86_64"
    System.getProperty("os.name").startsWith("Windows") -> "windows-x86_64"
    else -> "linux-x86_64"
}
val toolchainBin = ndkDir.resolve("toolchains/llvm/prebuilt/$hostTag/bin")
val androidLinker = toolchainBin.resolve("aarch64-linux-android31-clang")
val llvmAr = toolchainBin.resolve("llvm-ar")
val llvmRanlib = toolchainBin.resolve("llvm-ranlib")

val rustTargetDir = layout.buildDirectory.dir("rust-target")
val uniffiDir = layout.buildDirectory.dir("generated/uniffi")
val hostLibName = "libwhalerein_mobile.dylib"
val hostLib = rustTargetDir.map { it.file("debug/$hostLibName") }
val androidDebugLib = rustTargetDir.map { it.file("$androidTarget/debug/libwhalerein_mobile.so") }
val androidReleaseLib = rustTargetDir.map { it.file("$androidTarget/release/libwhalerein_mobile.so") }
val jniDebugDir = layout.buildDirectory.dir("generated/jniLibs/debug")
val jniReleaseDir = layout.buildDirectory.dir("generated/jniLibs/release")

val rustInputs = files(
    repoRoot.resolve("Cargo.toml"),
    repoRoot.resolve("Cargo.lock"),
    repoRoot.resolve("rust-toolchain.toml"),
    crateDir.resolve("Cargo.toml"),
    crateDir.resolve("uniffi.toml"),
    fileTree(repoRoot.resolve("core/src")) { include("**/*.rs") },
    repoRoot.resolve("core/Cargo.toml"),
    fileTree(repoRoot.resolve("proto")) { include("**/*.rs", "**/*.proto", "Cargo.toml") },
    fileTree(crateDir.resolve("src")) { include("**/*.rs") },
    fileTree(crateDir.resolve("build.rs")),
)

val cargoBuildHost = tasks.register<Exec>("cargoBuildHost") {
    group = "rust"
    description = "Build the host whalerein-mobile library used for UniFFI metadata"
    workingDir = repoRoot
    commandLine(
        "cargo", "build", "--locked", "-p", "whalerein-mobile", "--lib",
        "--no-default-features", "--features", "cli",
        "--target-dir", rustTargetDir.get().asFile.absolutePath,
    )
    environment("WHALEREIN_MOBILE_SOURCE", mobileSource)
    environment("CARGO_NET_OFFLINE", "true")
    inputs.files(rustInputs)
    inputs.property("mobileSource", mobileSource)
    outputs.file(hostLib)
}

val generateUniffiKotlin = tasks.register<Exec>("generateUniffiKotlin") {
    group = "rust"
    description = "Generate Kotlin UniFFI bindings from the host library metadata"
    dependsOn(cargoBuildHost)
    workingDir = repoRoot
    commandLine(
        "cargo", "run", "--locked", "-q", "-p", "whalerein-mobile",
        "--no-default-features", "--features", "cli",
        "--target-dir", rustTargetDir.get().asFile.absolutePath,
        "--bin", "uniffi-bindgen", "--", "generate",
        "--library", hostLib.get().asFile.absolutePath,
        "--language", "kotlin",
        "--out-dir", uniffiDir.get().asFile.absolutePath,
        "--no-format",
    )
    environment("WHALEREIN_MOBILE_SOURCE", mobileSource)
    environment("CARGO_NET_OFFLINE", "true")
    inputs.files(rustInputs)
    inputs.file(hostLib)
    inputs.property("mobileSource", mobileSource)
    outputs.dir(uniffiDir)
}

fun registerCargoBuild(
    name: String,
    profile: String,
    profileArgs: List<String>,
    output: Provider<RegularFile>,
): TaskProvider<Exec> = tasks.register<Exec>(name) {
    group = "rust"
    description = "Compile whalerein-mobile for $androidTarget ($profile)"
    workingDir = repoRoot
    commandLine(
        buildList {
            add("cargo")
            add("build")
            add("--locked")
            add("-p")
            add("whalerein-mobile")
            add("--lib")
            add("--no-default-features")
            add("--target")
            add(androidTarget)
            addAll(profileArgs)
            add("--target-dir")
            add(rustTargetDir.get().asFile.absolutePath)
        }
    )
    environment("WHALEREIN_MOBILE_SOURCE", mobileSource)
    environment("CARGO_NET_OFFLINE", "true")
    environment("CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER", androidLinker.absolutePath)
    environment("CC_aarch64-linux-android", androidLinker.absolutePath)
    environment("AR_aarch64-linux-android", llvmAr.absolutePath)
    environment("RANLIB_aarch64-linux-android", llvmRanlib.absolutePath)
    environment(
        "CARGO_ENCODED_RUSTFLAGS",
        listOf(
            "-Clink-arg=-Wl,-z,max-page-size=16384",
            "-Clink-arg=-Wl,-z,common-page-size=16384",
        ).joinToString("\u001f"),
    )
    inputs.files(rustInputs)
    inputs.property("mobileSource", mobileSource)
    inputs.property("target", androidTarget)
    inputs.property("ndk", ndkVersion)
    inputs.property("profile", profile)
    outputs.file(output)
}

val cargoBuildAndroidDebug = registerCargoBuild(
    "cargoBuildAndroidDebug", "debug", listOf(), androidDebugLib,
)
val cargoBuildAndroidRelease = registerCargoBuild(
    "cargoBuildAndroidRelease", "release", listOf("--release"), androidReleaseLib,
)

fun registerPackageNative(
    name: String,
    source: Provider<RegularFile>,
    destination: Provider<Directory>,
    buildTask: TaskProvider<Exec>,
): TaskProvider<Copy> = tasks.register<Copy>(name) {
    group = "rust"
    description = "Package $androidTarget native library into the Android jniLibs output"
    dependsOn(buildTask)
    from(source)
    into(destination.map { it.dir("arm64-v8a") })
    inputs.file(source)
    outputs.dir(destination)
}

val packageNativeDebug = registerPackageNative(
    "packageNativeDebug", androidDebugLib, jniDebugDir, cargoBuildAndroidDebug,
)
val packageNativeRelease = registerPackageNative(
    "packageNativeRelease", androidReleaseLib, jniReleaseDir, cargoBuildAndroidRelease,
)

android {
    namespace = "com.whalerein.android.corebridge"
    compileSdk = 37
    compileSdkMinor = 2
    ndkVersion = ndkVersion

    defaultConfig {
        minSdk = 31
        consumerProguardFiles("consumer-rules.pro")
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        buildConfigField("String", "CORE_REVISION", "\"$coreRevision\"")
        buildConfigField("String", "CORE_VERSION", "\"$workspaceVersion\"")
        buildConfigField("String", "APP_VERSION", "\"$appVersion\"")
        buildConfigField("String", "ANDROID_REDIRECT_URI", "\"https://app.whalerein.test/client/android/callback\"")
        manifestPlaceholders["whalereinCoreRevision"] = coreRevision
        manifestPlaceholders["whalereinCoreVersion"] = workspaceVersion
        manifestPlaceholders["whalereinAppVersion"] = appVersion
        ndk {
            abiFilters += "arm64-v8a"
        }
    }

    sourceSets {
        getByName("main") {
            kotlin.srcDir(uniffiDir.get().asFile)
            java.srcDir(uniffiDir.get().asFile)
        }
        getByName("debug") {
            jniLibs.srcDir(jniDebugDir.get().asFile)
        }
        getByName("release") {
            jniLibs.srcDir(jniReleaseDir.get().asFile)
        }
    }

    lint {
        // AGP 9.5.2's lint-vital analyzer crashes on a Kotlin property symbol in
        // this project ("Unexpected owner function: null"), which blocks release
        // packaging. Release lint stays a separate explicit check
        // (`./gradlew :app:lintNoGmsRelease`) instead of a packaging side effect.
        checkReleaseBuilds = false
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        buildConfig = true
    }

    packaging {
        jniLibs {
            useLegacyPackaging = false
        }
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}

dependencies {
    api("com.whalerein.android:state")
    implementation(libs.kotlinx.coroutines.android)
    implementation(libs.kotlinx.serialization.json)
    implementation(libs.androidx.annotation)
    implementation(libs.jna) {
        artifact {
            type = "aar"
        }
    }
    testImplementation(libs.junit.jupiter)
    testRuntimeOnly(libs.junit.platform.launcher)
    androidTestImplementation(libs.androidx.test.ext.junit)
    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(libs.androidx.test.core)
}

tasks.withType<Test>().configureEach {
    useJUnitPlatform()
}

// Wire generated sources and the native library before Kotlin compiles.
tasks.matching { it.name == "preDebugBuild" }.configureEach {
    dependsOn(generateUniffiKotlin, packageNativeDebug)
}
tasks.matching { it.name == "preReleaseBuild" }.configureEach {
    dependsOn(generateUniffiKotlin, packageNativeRelease)
}
tasks.matching { it.name == "compileDebugKotlin" || it.name == "compileReleaseKotlin" }
    .configureEach {
        dependsOn(generateUniffiKotlin)
    }
