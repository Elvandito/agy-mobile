# Changelog

All notable changes to AGY Mobile are documented in this file.

## [0.1.0] - 2026-11-25

### Added
- Multi-crate Rust workspace (`agy-core`, `agy-auth`, `agy-terminal`, `agy-workspace`, `agy-runtime`) targeting `armv7-linux-androideabi`, `aarch64-linux-android`, `i686-linux-android`, and `x86_64-linux-android` at Android API 21+.
- Explicit dual-runtime engine separating `NativeCli` execution (`android_arm64` on Android API 24+ with SHA-512 manifest verification and `stream-json` NDJSON IPC) from `CompatibleApi` execution (pure-Rust SSE streaming Gemini agent loop working on all 32-bit and 64-bit Android 5.0+ devices).
- OAuth 2.0 Authorization Code with PKCE (`S256`) flow, loopback callback listener, manual code exchange fallback, automatic token refresh, Gemini API key header mode, `SecretToken` debug/display redaction, and Android Keystore hardware/software-backed envelope encryption (API 21+).
- Bounded `VecDeque` ring-buffer terminal with streaming ANSI CSI/SGR parser, command history, `Ctrl+C` stream/process interruption, double `Ctrl+D` session exit, and low-memory trim eviction for 1 GB RAM devices.
- Workspace boundary enforcement, streaming line-range file reader, streaming workspace search, targeted file editing with confirmation gates, Android Storage Access Framework (SAF) two-way sync bridge, and pure-Rust `.git` repository status/log/diff inspector.
- Native Android client (`minSdk = 21`, zero WebView) with custom monospace terminal renderer, responsive phone/tablet layouts, keyboard navigation, and verified WCAG AA Dark and Light-Paper themes.
- Reproducible GitHub Actions workflow producing signed per-ABI and universal APKs alongside automated architecture and emulator verification.
