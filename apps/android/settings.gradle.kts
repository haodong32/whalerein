pluginManagement {
    repositories {
        google {
            content {
                includeGroupByRegex("com\\.android.*")
                includeGroupByRegex("com\\.google.*")
                includeGroupByRegex("androidx.*")
            }
        }
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "whalerein-android"

// `state` is a standalone JDK-only Gradle build so the JVM test path never
// configures an Android plugin or requires the Android SDK. The root build
// consumes it through dependency substitution.
includeBuild("state") {
    dependencySubstitution {
        substitute(module("com.whalerein.android:state")).using(project(":"))
    }
}
include(":core-bridge", ":app")
