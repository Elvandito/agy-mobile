#!/usr/bin/env bash
set -euo pipefail

APK_IN_DIR="${1:?Usage: collect-and-verify-apks.sh <gradle-apk-output-dir> <dist-dir>}"
DIST_DIR="${2:?Usage: collect-and-verify-apks.sh <gradle-apk-output-dir> <dist-dir>}"

mkdir -p "$DIST_DIR"

find_apk() {
    local pattern="$1"
    local match
    match="$(find "$APK_IN_DIR" -maxdepth 2 -type f -name "$pattern" | head -n 1)"
    if [[ -z "$match" ]]; then
        echo "ERROR: Expected APK matching '$pattern' in '$APK_IN_DIR' was not found." >&2
        find "$APK_IN_DIR" -type f >&2 || true
        exit 1
    fi
    printf "%s" "$match"
}

cp -f "$(find_apk "*universal*.apk")"      "$DIST_DIR/agy-mobile-universal.apk"
cp -f "$(find_apk "*armeabi-v7a*.apk")"    "$DIST_DIR/agy-mobile-armv7.apk"
cp -f "$(find_apk "*arm64-v8a*.apk")"      "$DIST_DIR/agy-mobile-arm64.apk"
cp -f "$(find_apk "*-x86-*.apk")"          "$DIST_DIR/agy-mobile-x86.apk"
cp -f "$(find_apk "*x86_64*.apk")"         "$DIST_DIR/agy-mobile-x86_64.apk"

verify_single_abi_apk() {
    local apk="$1"
    local expected_abi="$2"
    local expected_elf_class="$3"
    local expected_machine="$4"

    echo "=== Verifying $apk (expecting $expected_abi / $expected_elf_class / $expected_machine) ==="
    local entries
    entries="$(unzip -Z1 "$apk" | grep '^lib/.*/libagy_runtime\.so$' || true)"
    if [[ "$entries" != "lib/${expected_abi}/libagy_runtime.so" ]]; then
        echo "ERROR: Unexpected native library layout in $apk:" >&2
        echo "$entries" >&2
        exit 1
    fi

    local tmp_dir
    tmp_dir="$(mktemp -d)"
    unzip -q "$apk" "lib/${expected_abi}/libagy_runtime.so" -d "$tmp_dir"
    local so_file="$tmp_dir/lib/${expected_abi}/libagy_runtime.so"

    local header_info
    header_info="$(readelf -h "$so_file")"
    echo "$header_info" | grep -E "Class:|Machine:"
    echo "$header_info" | grep -q "$expected_elf_class"
    echo "$header_info" | grep -q "$expected_machine"

    # Confirm only baseline API 21 Bionic libraries are linked (no API 24+ symbols or bloated C++ runtimes).
    local needed
    needed="$(readelf -d "$so_file" | grep '(NEEDED)' || true)"
    echo "$needed"
    while IFS= read -r line; do
        [[ -z "$line" ]] && continue
        if ! echo "$line" | grep -Eq '\[(libc\.so|libm\.so|libdl\.so|liblog\.so)\]'; then
            echo "ERROR: Non-API-21 or unexpected shared library dependency in $apk: $line" >&2
            rm -rf "$tmp_dir"
            exit 1
        fi
    done <<< "$needed"

    rm -rf "$tmp_dir"
}

verify_single_abi_apk "$DIST_DIR/agy-mobile-armv7.apk"  "armeabi-v7a" "ELF32" "ARM"
verify_single_abi_apk "$DIST_DIR/agy-mobile-arm64.apk"  "arm64-v8a"   "ELF64" "AArch64"
verify_single_abi_apk "$DIST_DIR/agy-mobile-x86.apk"    "x86"         "ELF32" "Intel 80386"
verify_single_abi_apk "$DIST_DIR/agy-mobile-x86_64.apk" "x86_64"      "ELF64" "Advanced Micro Devices X86-64"

echo "=== Verifying universal APK contains all 4 architectures ==="
univ_libs="$(unzip -Z1 "$DIST_DIR/agy-mobile-universal.apk" | grep '^lib/.*/libagy_runtime\.so$' | sort)"
echo "$univ_libs"
for abi in armeabi-v7a arm64-v8a x86 x86_64; do
    if ! echo "$univ_libs" | grep -qx "lib/${abi}/libagy_runtime.so"; then
        echo "ERROR: Universal APK is missing lib/${abi}/libagy_runtime.so" >&2
        exit 1
    fi
done

if command -v apksigner >/dev/null 2>&1; then
    for apk in "$DIST_DIR"/agy-mobile-*.apk; do
        echo "=== Verifying APK signature (v1 + v2) for $(basename "$apk") ==="
        apksigner verify --min-sdk-version 21 --verbose "$apk" | head -n 8
    done
fi

ls -lh "$DIST_DIR"/agy-mobile-*.apk
echo "All 5 APKs passed architecture, API 21 linkage, and signature checks."
