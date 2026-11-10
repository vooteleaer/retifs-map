plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)
}

// The version (retifs docs/versioning.md): the workspace's, a release's when
// this commit is tagged with it, else a build towards it; the commit count is
// the versionCode, so each build installs over the one before.
val repo = rootProject.projectDir.parentFile
fun git(vararg args: String): String = try {
    ProcessBuilder(listOf("git", "-C", repo.path) + args).redirectErrorStream(true).start().inputStream.bufferedReader().readText().trim()
} catch (e: Exception) {
    ""
}
val workspaceVersion = File(repo, "Cargo.toml").readLines().first { it.startsWith("version = ") }.substringAfter('"').substringBefore('"')
val commits = git("rev-list", "--count", "HEAD").toIntOrNull() ?: 1
val released = git("describe", "--exact-match", "--tags", "HEAD") == "v$workspaceVersion"

android {
    namespace = "org.retifs.map"
    compileSdk = 34

    defaultConfig {
        applicationId = "org.retifs.map"
        minSdk = 26
        targetSdk = 34
        versionCode = commits
        versionName = if (released) workspaceVersion else "$workspaceVersion-dev.$commits"
        ndk { abiFilters += listOf("arm64-v8a", "x86_64") }
    }

    buildFeatures { compose = true }

    buildTypes {
        // Shrunk (R8 keeps the native methods the JNI library implements).
        // Signed with the debug key for now: for testers, not a store.
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.compose.ui)
    implementation(libs.androidx.compose.material3)
    implementation(libs.androidx.compose.material.icons)
}
