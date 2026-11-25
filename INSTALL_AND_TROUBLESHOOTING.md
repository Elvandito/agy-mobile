# Installation and Troubleshooting Guide

## 1. Choosing the Right APK

Check your Android device CPU ABI via `adb shell getprop ro.product.cpu.abi` (or install `agy-mobile-universal.apk` if you do not have `adb` handy):

- `armeabi-v7a` (older 32-bit phones, budget Android Go devices): install `agy-mobile-armv7.apk`
- `arm64-v8a` (modern 64-bit ARM phones and tablets): install `agy-mobile-arm64.apk`
- `x86` (32-bit Intel Atom tablets and Android 5.0 x86 emulators): install `agy-mobile-x86.apk`
- `x86_64` (Chromebooks with Android container and x86_64 emulators): install `agy-mobile-x86_64.apk`
- Mixed fleet or unknown ABI: install `agy-mobile-universal.apk`

### Installing via ADB

```bash
adb install -r agy-mobile-armv7.apk
```

### Installing Directly on Device

1. Copy the `.apk` file to the device Downloads folder.
2. Tap the `.apk` in the system Files app.
3. On Android 5.0-7.1, enable **Settings -> Security -> Unknown sources** when prompted. On Android 8.0+, grant **Install unknown apps** for your file manager.

## 2. Setting Up Authentication

AGY Mobile never embeds hardcoded OAuth client secrets or shared API keys.

### Option A: Google OAuth 2.0 with PKCE (`S256`)

1. Open Google Cloud Console -> **APIs & Services -> Library** and enable the **Generative Language API**.
2. Open **APIs & Services -> Credentials -> Create Credentials -> OAuth client ID**.
3. Select **Desktop app** (or **Installed** client supporting loopback `http://127.0.0.1` PKCE without a client secret) and copy your Client ID (`*.apps.googleusercontent.com`).
4. In AGY Mobile, open **Config**, paste your Client ID, and tap **Sign In with Google Browser OAuth (PKCE)**.
5. Complete sign-in in your browser. AGY Mobile listens on an ephemeral `127.0.0.1:<port>` loopback socket and exchanges the authorization code automatically. If an older Android 5.0 browser blocks localhost redirects, copy the final URL from the browser address bar, paste it into the **Redirect URL or authorization code** box in **Config**, and tap **Exchange Authorization Code**.

### Option B: Authorized Gemini API Key

1. Generate an API key in Google AI Studio.
2. In AGY Mobile, open **Config**, enter the key in the **Gemini API Key** field, and tap **Save Key**.
3. The key is encrypted with AES-256-GCM under an Android Keystore key (`KeyPairGeneratorSpec` RSA-wrapped key on API 21-22, `KeyGenParameterSpec` AES-256-GCM key on API 23+) and sent exclusively via the `x-goog-api-key` HTTP header (never in URLs or logs).

### Option C: Official Native CLI (`arm64-v8a`, Android 7.0 / API 24+ Only)

On 64-bit ARM devices running Android 7.0+, open **Config** and tap **Download Official Google ARM64 CLI (SHA-512 verified)** to download Google's official `android_arm64` binary into AGY Mobile's private `bin/` directory, or let AGY Mobile detect an existing compatible binary.

## 3. Mounting a Workspace (Storage Access Framework)

1. Switch to the **Workspace** tab.
2. Tap **Open SAF Folder** and grant folder access in Android's system document picker.
3. AGY Mobile mirrors text files (`<= 4 MB` per file) in 8 KB streaming chunks so native tools and the built-in pure-Rust Git inspector work immediately, flushing edits back to the SAF document tree automatically.
4. On stripped AOSP builds that omit `DocumentsUI`, tap **New Project** to create a managed workspace inside app storage.

## 4. Troubleshooting

### `INSTALL_FAILED_NO_MATCHING_ABIS`
You attempted to install a single-ABI APK (such as `agy-mobile-arm64.apk`) on a 32-bit `armeabi-v7a` or `x86` device. Install the matching architecture APK or `agy-mobile-universal.apk`.

### `[SAF UNAVAILABLE] Device lacks DocumentsUI picker`
Some custom Android 5.x/6.x ROMs remove the system `com.android.documentsui` package. Use **Workspace -> New Project** to create local workspaces inside AGY Mobile's sandboxed storage.

### `Authentication rejected (HTTP 401 / 403)`
- Confirm the **Generative Language API** is enabled on the Google Cloud project owning your OAuth Client ID or API key.
- Tap **Sign Out** in **Config** and re-authenticate to clear an expired or revoked token.

### Official CLI Install Button Hidden
The button is shown only when `abi == arm64-v8a` and `SDK_INT >= 24`. On 32-bit ARMv7, x86, x86_64, and Android 5.0-6.0, Google does not publish a compatible native binary, so AGY Mobile runs its built-in Rust API compatibility engine automatically.
