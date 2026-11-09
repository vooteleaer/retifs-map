# Builds retifs Map for Android (a debug APK): the core's JNI library for
# each ABI with the NDK, into app/src/main/jniLibs, then Gradle. It reaches
# retifs through the retifs app (org.retifs.desktop), which must be installed.
#
#     powershell -ExecutionPolicy Bypass -File android\build-apk.ps1 [-Abis arm64-v8a,x86_64] [-Release]
#
# Needs the Android SDK with NDK 27 ($env:ANDROID_HOME, default C:\Android\Sdk),
# JDK 17, rustup targets aarch64-linux-android and x86_64-linux-android, and
# retifs next to retifs-map.
param([string[]]$Abis = @("arm64-v8a", "x86_64"), [switch]$Release)

$ErrorActionPreference = "Stop"
$Here = $PSScriptRoot
$Root = (Resolve-Path "$Here\..").Path
$Sdk = if ($env:ANDROID_HOME) { $env:ANDROID_HOME } else { "C:\Android\Sdk" }
$Ndk = Get-ChildItem "$Sdk\ndk" | Sort-Object Name | Select-Object -Last 1
$Bin = "$($Ndk.FullName)\toolchains\llvm\prebuilt\windows-x86_64\bin"
$env:CARGO_TARGET_DIR = Join-Path $Root "target"
$Api = 26

function Invoke-Cargo { & cargo.exe @args; if ($LASTEXITCODE -ne 0) { throw "cargo $args failed" } }

Get-ChildItem "$Here\app\src\main\jniLibs" -Directory -ErrorAction SilentlyContinue |
    Where-Object { $Abis -notcontains $_.Name } | Remove-Item -Recurse -Force

foreach ($abi in $Abis) {
    $triple = @{ "arm64-v8a" = "aarch64-linux-android"; "x86_64" = "x86_64-linux-android" }[$abi]
    $var = $triple.ToUpper().Replace("-", "_")
    $under = $triple.Replace("-", "_")
    Set-Item "env:CARGO_TARGET_${var}_LINKER" "$Bin\$triple$Api-clang.cmd"
    Set-Item "env:CC_$under" "$Bin\$triple$Api-clang.cmd"
    Set-Item "env:AR_$under" "$Bin\llvm-ar.exe"
    Write-Host "== $abi ($triple)"
    Invoke-Cargo build --release --target $triple --manifest-path "$Root\Cargo.toml" -p retifs-map-jni
    $out = "$Here\app\src\main\jniLibs\$abi"
    New-Item -ItemType Directory -Force $out | Out-Null
    Copy-Item "$env:CARGO_TARGET_DIR\$triple\release\libretifs_map_android.so" "$out\libretifs_map_android.so"
    & "$Bin\llvm-strip.exe" "$out\libretifs_map_android.so"
}

Write-Host "== APK"
Push-Location $Here
try {
    & .\gradlew.bat --no-daemon $(if ($Release) { "assembleRelease" } else { "assembleDebug" })
    if ($LASTEXITCODE -ne 0) { throw "gradle failed" }
} finally { Pop-Location }
$apk = if ($Release) { "$Here\app\build\outputs\apk\release\app-release.apk" } else { "$Here\app\build\outputs\apk\debug\app-debug.apk" }
"== $apk ({0:N1} MB)" -f ((Get-Item $apk).Length / 1MB)
