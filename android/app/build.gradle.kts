plugins { id("com.android.application") }

android {
    namespace = "cz.hajadlo.hark"
    compileSdk = 37
    buildToolsVersion = "36.0.0"
    defaultConfig {
        applicationId = "cz.hajadlo.hark"
        minSdk = 33
        targetSdk = 37
        versionCode = 4
        versionName = "0.2.2"
    }
    signingConfigs {
        getByName("debug") {
            storeFile = file(System.getenv("HARK_DEBUG_KEYSTORE") ?: "/signing/android-debug.keystore")
            storePassword = "android"
            keyAlias = "androiddebugkey"
            keyPassword = "android"
        }
    }
    buildFeatures { buildConfig = true }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    testOptions.unitTests.isIncludeAndroidResources = true
    testOptions.unitTests.all {
        it.jvmArgs("--add-opens=java.base/java.lang=ALL-UNNAMED", "--add-opens=java.base/java.util=ALL-UNNAMED",
            "--add-opens=java.base/java.io=ALL-UNNAMED", "--add-opens=java.base/java.net=ALL-UNNAMED",
            "--add-opens=java.base/java.security=ALL-UNNAMED", "--add-opens=java.base/java.text=ALL-UNNAMED",
            "--add-opens=java.base/jdk.internal.access=ALL-UNNAMED", "--add-opens=java.desktop/java.awt.font=ALL-UNNAMED",
            "--add-opens=jdk.compiler/com.sun.tools.javac.api=ALL-UNNAMED")
        it.environment("HARK_TEST_URL", System.getenv("HARK_TEST_URL") ?: "")
        it.environment("HARK_TEST_TOKEN_FILE", System.getenv("HARK_TEST_TOKEN_FILE") ?: "")
        it.environment("HARK_TEST_WAV", System.getenv("HARK_TEST_WAV") ?: "")
        it.testLogging { events("passed", "skipped", "failed"); showStandardStreams = true }
    }
}
dependencies {
    implementation("androidx.core:core:1.19.1")
    testImplementation("junit:junit:4.13.2")
    testImplementation("org.robolectric:robolectric:4.17")
    // The framework JSON implementation is stubbed in local JVM tests.
    testImplementation("org.json:json:20250517")
}
