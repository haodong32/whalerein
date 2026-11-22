# Android client

The Android client is built with Gradle 9.6.1 and JDK 21. It has three modules:

| Module | Kind | Responsibility |
|---|---|---|
| `:app` | Android application | Single Activity, Compose UI, Navigation 3, system Intents, file pickers, locale resources and the composition root. Product flavors are `gms` and `noGms`. |
| `:state` | Kotlin/JVM library | Screen models, `UiState`, typed Core ports and deterministic JVM tests. It never applies an Android plugin and never imports `android.*`. |
| `:core-bridge` | Android library | UniFFI Kotlin bindings, Rust library packaging, Keystore-backed `SecretStore`, TLS adapters and the app-facing port implementations. |

`state` is a standalone Gradle build consumed by the root build through
`includeBuild`. That keeps the fast JVM test path independent of the Android
SDK, NDK and any device.

## JDK-only tests

The state module is the ordinary Android unit layer and the only Android target
selected by an ordinary push.

```bash
export JAVA_HOME=/opt/homebrew/opt/openjdk@21
cd apps/android
./gradlew -p state test --console=plain
./gradlew -p state test --tests '*ConversationViewModelTest'
```

Through the repository runner (from the repository root):

```bash
./repo test --project android-state --json
./repo test --project android-state --only AC-24.focused --json
./repo test --project android-state --list
./repo test --project android-state --coverage --json
./repo check android-state
```

The runner invokes `apps/android/gradlew -p state cleanTest test`, passes
`JAVA_HOME` through when it is set, parses Gradle's JUnit XML
under `apps/android/state/build/test-results/test/`, and saves the repository
report with the tested revision, project, case IDs and statuses.

Focused runs add `-PcaseFilter=<literal case id>`. The state build registers a
JUnit condition that disables every non-matching body before execution, then
parses the full `@DisplayName` from XML. Filter-skipped cases are not selected
evidence: a filtered run that observes only skipped cases is an unmatched
selector, not a pass. A genuinely skipped selected case is reported by an
unfiltered run. `--list` adds `-PcaseList=true`, which disables all bodies
and emits every case display name as a skipped testcase for discovery.
`--coverage` runs `./gradlew -p state cleanTest test jacocoTestCoverageVerification` and
reports JaCoCo line ratios per package from
`apps/android/state/build/reports/jacoco/test/jacocoTestReport.xml`. The state
build enforces a 90% per-package line rule; a package below the gate fails the
saved report. Ordinary push CI does not request Android coverage; full/explicit
verification does.

## Build and package

```bash
export JAVA_HOME=/opt/homebrew/opt/openjdk@21
cd apps/android
./gradlew :app:assembleNoGmsDebug
./gradlew :app:testNoGmsDebugUnitTest
./gradlew :app:bundleNoGmsRelease
./gradlew :app:connectedNoGmsDebugAndroidTest
```

`local.properties` carries `sdk.dir` and is Git-ignored. The application uses one
application ID and one version sequence for both flavors. `noGms` carries no
Google service dependency, manifest entry or initializer. Release packaging is
minified and shrunk.

Signing is deliberately outside the tracked tree. Local development uses a
repository-local test keystore created by `apps/android/scripts/`; release
signing material comes from environment or keystore properties and is never
committed. Candidate artifacts are identified by application ID, version code
and version name, the signing certificate, the Rust/UniFFI revision embedded by
`:core-bridge`, and the packaged 16 KB-compatible native libraries. The
integration suite checks page alignment with `zipalign -c -P 16` where the SDK
tools are available. A source build is not release evidence; an authorized
signed upgrade and publication run remain separate gates.

## Device suites

These are explicit repository targets, never part of bare `./repo test` and
never selected by an ordinary pull-request push:

```bash
./repo test android-ui --json
./repo test android-integration --json
./repo test android-performance --json
./repo test android-ui --only AC-13.adaptive
./repo test android-ui --list
```

| Target | Gradle task by default | Scope |
|---|---|---|
| `android-ui` | `:app:connectedNoGmsDebugAndroidTest` | Compose fixture instrumentation on a compatible target. |
| `android-integration` | `:app:connectedNoGmsDebugAndroidTest` | Native bridge, Keystore, TLS and device checks. Set `WHALEREIN_ANDROID_INTEGRATION_TASK` when the integration source set gets a dedicated module task. |
| `android-performance` | `:benchmark:connectedNoGmsReleaseAndroidTest` | Macrobenchmark with fixed workloads on a physical device. Set `WHALEREIN_ANDROID_PERFORMANCE_TASK` for a different benchmark module. |

All device suites require `adb` and an Android SDK (`ANDROID_HOME`,
`ANDROID_SDK_ROOT`, or `local.properties`). `android-performance` additionally
rejects an emulator. When `WHALEREIN_ANDROID_LOCAL_STACK=1` is set, an
integration run also requires `WHALEREIN_ANDROID_ACCESS_URL` for the official
local Cloud or Standalone stack. Missing prerequisites produce explicit
`unavailable` evidence with exit status 3; they are never reported as a skip or
a pass. `--only` filters parsed instrumentation case IDs, and an unmatched
selector is a usage error. Reports record the selected suite and task, case IDs
and statuses, the tested revision, and the device ABI, API level, release,
effective page size and emulator flag when `adb` exposes them.

## CI

Ordinary push CI runs only JDK-only `android-state` static checks and tests. It
installs no Android SDK/NDK, packages nothing, starts no emulator and runs no
device or performance suite. The `run-full-ci` label event, manual dispatch and
the full workflow request the Android device suites; a push that merely keeps
the label attached stays on the quick path. Performance runs are manual and need
a physical-device runner.

## Local device acceptance against the development Cloud

`scripts/setup-rootable-avd.sh` prepares a non-Play `google_apis` emulator for
the local `*.whalerein.test` Cloud: it adds guest host entries pointing at the
host loopback (`10.0.2.2`) and installs the mkcert development CA in the system
trust store. Start the AVD with `-writable-system`, then run the script with the
serial selected:

```bash
emulator -avd wr_api33_root -writable-system -no-snapshot &
WHALEREIN_AVD_SERIAL=emulator-5554 apps/android/scripts/setup-rootable-avd.sh
```

API 33 is the default because its system CA store is writable. Android 14+
keeps CAs in the Conscrypt APEX; the script attempts the bind-mount overlay, but
app and zygote mount namespaces may not observe it. API 37 system images also
failed to boot on the recorded host. Chrome still labels `*.test` as "identity
not verified" even with a trusted CA because `.test` is a special-use TLD, so a
fully verified browser journey needs a real domain or staging environment.
