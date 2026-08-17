plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.guhhammer.vega.background"
    compileSdk = 34

    defaultConfig {
        // Android 7. The same floor the download page and the release notes
        // claim, so raising it here is a promise broken in two other places.
        minSdk = 24
        consumerProguardFiles("consumer-rules.pro")
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }

    kotlinOptions {
        jvmTarget = "1.8"
    }
}

dependencies {
    // ServiceCompat.startForeground and NotificationCompat, which is what keeps
    // the version checks for foreground service types out of our code.
    implementation("androidx.core:core-ktx:1.13.1")
    implementation(project(":tauri-android"))
}
