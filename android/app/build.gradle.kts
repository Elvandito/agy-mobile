import java.io.FileInputStream
import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

val keystorePropsFile = rootProject.file("keystore.properties")
val keystoreProps = Properties().apply {
    if (keystorePropsFile.exists()) {
        FileInputStream(keystorePropsFile).use { load(it) }
    }
}

android {
    namespace = "dev.antigravity.mobile"
    compileSdk = 35

    defaultConfig {
        applicationId = "dev.antigravity.mobile"
        minSdk = 21
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
    }

    signingConfigs {
        create("releaseSigned") {
            val ksPath = System.getenv("AGY_KEYSTORE_PATH") ?: keystoreProps.getProperty("storeFile")
            if (!ksPath.isNullOrBlank() && file(ksPath).exists()) {
                storeFile = file(ksPath)
                storePassword = System.getenv("AGY_KEYSTORE_PASSWORD")
                    ?: keystoreProps.getProperty("storePassword")
                keyAlias = System.getenv("AGY_KEY_ALIAS")
                    ?: keystoreProps.getProperty("keyAlias")
                keyPassword = System.getenv("AGY_KEY_PASSWORD")
                    ?: keystoreProps.getProperty("keyPassword")
                enableV1Signing = true
                enableV2Signing = true
            }
        }
    }

    buildTypes {
        getByName("debug") {
            isMinifyEnabled = false
        }
        getByName("release") {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
            val relSign = signingConfigs.findByName("releaseSigned")
            signingConfig = if (relSign?.storeFile != null) {
                relSign
            } else {
                signingConfigs.getByName("debug")
            }
        }
    }

    splits {
        abi {
            isEnable = true
            reset()
            include("armeabi-v7a", "arm64-v8a", "x86", "x86_64")
            isUniversalApk = true
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_11
        targetCompatibility = JavaVersion.VERSION_11
    }

    kotlinOptions {
        jvmTarget = "11"
    }

    packaging {
        jniLibs {
            // Required so API 21-22 devices extract and load native libraries cleanly with minimal RAM map pressure.
            useLegacyPackaging = true
        }
        resources {
            excludes += setOf("META-INF/*.kotlin_module", "META-INF/AL2.0", "META-INF/LGPL2.1")
        }
    }
}

dependencies {
    // Minimal Kotlin stdlib only; zero WebView, zero heavy UI frameworks, < 1 MB DEX footprint for 1 GB RAM devices.
}
