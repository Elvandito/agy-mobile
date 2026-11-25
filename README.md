# AGY Mobile

Native Rust Android client and compatibility runtime for Google Antigravity CLI (`agy`), built for Android 5.0+ (API 21+) and low-RAM 32-bit and 64-bit devices.

## Why Two Runtimes Exist

Google's official `agy` installer (`https://antigravity.google/cli/install.sh`) publishes a single Android native binary: `android_arm64` (`cli_android_arm64.tar.gz`), compiled with Android NDK r27d against Android API 24 Bionic symbols. Upstream manifest queries for `android_arm` / `android_armv7`, `android_386`, and `android_amd64` return HTTP 404.

Rather than disguising an API client as the upstream binary or dropping 32-bit devices, AGY Mobile ships two explicit, clearly labeled execution runtimes inside `libagy_runtime.so`:

1. **Native CLI Mode (`RUNTIME: NATIVE CLI`)**: Available on `arm64-v8a` devices running Android 7.0+ (API 24+) when Google's official `agy` binary is detected or downloaded and SHA-512 verified from Google's official updater manifest. Communicates over `agy --output-format stream-json`.
2. **Rust API Compatibility Mode (`RUNTIME: API-COMPAT`)**: Works across all four Android ABIs (`armeabi-v7a`, `arm64-v8a`, `x86`, `x86_64`) down to Android 5.0 (API 21) and 1 GB RAM. Connects directly to Google's documented `generativelanguage.googleapis.com/v1beta` SSE streaming API using `rustls` (TLS 1.2/1.3 with bundled Mozilla roots, bypassing outdated Android 5.0 system CA stores) and implements a workspace-bounded tool loop with permission confirmation gates.

The top header bar and `/runtime` command always show which runtime is active and why.

## Supported Architectures & APK Outputs

| APK Artifact | Android ABI | Rust Target | Pointer Width | Min Android SDK |
|---|---|---|---|---|
| `agy-mobile-armv7.apk` | `armeabi-v7a` | `armv7-linux-androideabi` | 32-bit | API 21 (Android 5.0) |
| `agy-mobile-arm64.apk` | `arm64-v8a` | `aarch64-linux-android` | 64-bit | API 21 (Android 5.0) |
| `agy-mobile-x86.apk` | `x86` | `i686-linux-android` | 32-bit | API 21 (Android 5.0) |
| `agy-mobile-x86_64.apk` | `x86_64` | `x86_64-linux-android` | 64-bit | API 21 (Android 5.0) |
| `agy-mobile-universal.apk` | All 4 ABIs above | All 4 targets above | 32-bit & 64-bit | API 21 (Android 5.0) |

## Repository Layout

- `crates/agy-core/`: ABI/SDK compatibility matrix, configuration, 1 GB RAM memory budget profiles, streaming JSONL session persistence, and approval types.
- `crates/agy-auth/`: Zero-secret Google OAuth 2.0 PKCE (`S256`) flow, loopback callback listener, token refresh, Gemini API key header credential, `SecretToken` redaction wrapper, AES-256-GCM envelope encryption, and log scrubbing.
- `crates/agy-terminal/`: Streaming ANSI CSI/SGR parser, bounded `VecDeque` scrollback buffer with `onTrimMemory` eviction, persistent command history, `Ctrl+C` cancellation, and double `Ctrl+D` session exit.
- `crates/agy-workspace/`: Path-traversal sandbox, 8 KB chunked streaming file reader and search, atomic file editing, and a pure-Rust `.git` repository status/log/diff reader (`DIRC` v2/v3 index + zlib loose objects) that works on stock Android devices without a `git` binary.
- `crates/agy-runtime/`: Unified `AgyEngine`, `NativeCli` driver, `CompatibleApi` SSE agent loop, and panic-safe JNI exports (`libagy_runtime.so`).
- `android/`: Minimal Kotlin Android SDK client (`minSdk = 21`, zero WebView, zero heavy UI frameworks), Android Keystore wrapper (RSA-wrapped AES on API 21-22, hardware-backed AES-GCM on API 23+), Storage Access Framework two-way sync, and verified WCAG AA Dark Charcoal and Light Paper themes.
- `tests/`: End-to-end Rust integration suite covering OAuth PKCE exchange, SSE tool calling, file-edit permission confirmation, Plan mode enforcement, low-RAM trim behavior, and session persistence.

## Quick Installation

1. Download the APK matching your device architecture (or `agy-mobile-universal.apk` if unsure) from the GitHub Actions workflow artifacts or Releases page.
2. Install via `adb`:
   ```bash
   adb install -r agy-mobile-armv7.apk
   ```
   Or open the APK directly on your Android device.
3. Open **Config** inside AGY Mobile to sign in with your own Google Cloud OAuth 2.0 Client ID (Installed / Android PKCE flow, no client secret embedded) or save an authorized Gemini API key into the Android Keystore vault.
4. Open **Workspace** to mount a project folder through Android Storage Access Framework (`Open SAF Folder`) or create a local workspace (`New Project`).

See [INSTALL_AND_TROUBLESHOOTING.md](INSTALL_AND_TROUBLESHOOTING.md) for detailed OAuth setup and troubleshooting, and [ARCHITECTURE_COMPATIBILITY.md](ARCHITECTURE_COMPATIBILITY.md) for the binary and ABI compatibility analysis.

## Building From Source

```bash
rustup target add armv7-linux-androideabi aarch64-linux-android i686-linux-android x86_64-linux-android
cargo test --workspace
cargo ndk \
  -t armv7-linux-androideabi \
  -t aarch64-linux-android \
  -t i686-linux-android \
  -t x86_64-linux-android \
  --platform 21 \
  -o android/app/src/main/jniLibs \
  build --release -p agy-runtime
python3 scripts/generate-launcher-icons.py
cd android && gradle --no-daemon assembleRelease
```

## License

MIT. See [LICENSE](LICENSE).
