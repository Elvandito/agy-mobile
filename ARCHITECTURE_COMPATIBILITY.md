# Architecture Compatibility Report

## 1. Upstream Google `agy` Binary Investigation

Before writing AGY Mobile, we inspected Google's official Antigravity CLI bootstrapper (`https://antigravity.google/cli/install.sh`), release manifest service (`https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests/<platform>.json`), and shipped Android ELF binary.

### Verified Upstream Findings

| Platform Manifest Key | Upstream HTTP Status | Binary Format & Linker | Minimum Android API |
|---|---|---|---|
| `android_arm64` | `200 OK` | `ELF 64-bit LSB pie executable, ARM aarch64`, `/system/bin/linker64`, built with NDK r27d | API 24 (Android 7.0) |
| `android_arm` / `android_armv7` | `404 Not Found` | Not published by Google | N/A |
| `android_386` (x86 32-bit) | `404 Not Found` | Not published by Google | N/A |
| `android_amd64` (x86_64) | `404 Not Found` | Not published by Google | N/A |
| `linux_arm64` / `linux_amd64` | `200 OK` | GNU libc / musl desktop binaries (rejected by Android Bionic linker) | Desktop Linux only |

### Engineering Implications

1. Compiling a Rust wrapper does not recompile Google's proprietary CLI binary for 32-bit ARMv7, 32-bit x86, x86_64 Android, or Android 5.0-6.0 (API 21-23).
2. Even on 64-bit ARM64 phones, Android 5.0 through 6.0 (API 21-23) cannot load Google's `android_arm64` binary because its ELF dynamic section requires Bionic symbols introduced in API 24.
3. On Android 5.0 (API 21), system `SSLContext` lacks TLS 1.3 and misses newer root CA certificates. Using platform TLS causes HTTPS handshake failures on unpatched Lollipop devices.

## 2. AGY Mobile Dual-Runtime Compatibility Matrix

| Device Class | Rust Library Target | APK Variant | `NativeCli` Mode | `CompatibleApi` Mode | TLS Stack | Keystore Backing |
|---|---|---|---|---|---|---|
| ARMv7 (`armeabi-v7a`), API 21-35 | `armv7-linux-androideabi` | `agy-mobile-armv7.apk` | Not supported upstream (honestly reported) | Full support | `rustls` + bundled WebPKI roots | API 21-22: RSA-2048 wrap + AES-256-GCM; API 23+: Hardware AES-256-GCM |
| ARM64 (`arm64-v8a`), API 21-23 | `aarch64-linux-android` | `agy-mobile-arm64.apk` | Blocked safely (requires API 24 Bionic) | Full support | `rustls` + bundled WebPKI roots | API 21-22: RSA-2048 wrap + AES-256-GCM; API 23: Hardware AES-256-GCM |
| ARM64 (`arm64-v8a`), API 24-35 | `aarch64-linux-android` | `agy-mobile-arm64.apk` | Supported (`stream-json` + SHA-512 verified installer) | Full support | `rustls` + bundled WebPKI roots | Hardware/TEE AES-256-GCM |
| x86 (`x86`), API 21-35 | `i686-linux-android` | `agy-mobile-x86.apk` | Not supported upstream (honestly reported) | Full support | `rustls` + bundled WebPKI roots | API 21-22: RSA-2048 wrap + AES-256-GCM; API 23+: AES-256-GCM |
| x86_64 (`x86_64`), API 21-35 | `x86_64-linux-android` | `agy-mobile-x86_64.apk` | Not supported upstream (honestly reported) | Full support | `rustls` + bundled WebPKI roots | API 21-22: RSA-2048 wrap + AES-256-GCM; API 23+: AES-256-GCM |

## 3. Low-RAM (1 GB) & 32-Bit Optimizations

- **Pointer-width safety**: File sizes, Unix millisecond timestamps, and SHA-512 download counters use explicit `u64` / `i64` types rather than `usize`, preventing 32-bit overflow on `armv7-linux-androideabi` and `i686-linux-android`.
- **Bounded Tokio runtime**: Spawns 2 worker threads with `512 KB` stacks (`LowRam1Gb` profile) instead of default multi-megabyte per-core thread pools.
- **Bounded terminal scrollback**: Caps scrollback at 600 lines / 96 KB on `<= 1.5 GB` RAM devices (`VecDeque` ring buffer) and hooks `Activity.onTrimMemory` (`TRIM_MEMORY_RUNNING_LOW`, `TRIM_MEMORY_RUNNING_CRITICAL`) to evict scrollback down to 150 lines / 24 KB on OS pressure.
- **Streaming file & Git operations**: Workspace reads, searches, SAF mirroring, and Git `blob` SHA-1 hashing stream in 8 KB buffers rather than reading whole files into heap memory.
- **Zero WebView / zero Compose**: Pure Android framework views (`ListView` row recycling, `SpannableStringBuilder`, `GradientDrawable`) keep `classes.dex` tiny (single DEX, no Multidex startup penalty on Android 5.0).
- **Bionic linkage verification**: `scripts/collect-and-verify-apks.sh` inspects every `.so` with `readelf` in CI to confirm only `libc.so`, `libm.so`, `libdl.so`, and `liblog.so` are linked.
