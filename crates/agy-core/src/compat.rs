use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbiTarget {
    Armv7,
    Arm64,
    X86,
    X86_64,
    Other,
}

impl AbiTarget {
    pub fn current_compiled() -> Self {
        #[cfg(target_arch = "arm")]
        {
            Self::Armv7
        }
        #[cfg(target_arch = "aarch64")]
        {
            Self::Arm64
        }
        #[cfg(target_arch = "x86")]
        {
            Self::X86
        }
        #[cfg(target_arch = "x86_64")]
        {
            Self::X86_64
        }
        #[cfg(not(any(
            target_arch = "arm",
            target_arch = "aarch64",
            target_arch = "x86",
            target_arch = "x86_64"
        )))]
        {
            Self::Other
        }
    }

    pub fn rust_triple(self) -> &'static str {
        match self {
            Self::Armv7 => "armv7-linux-androideabi",
            Self::Arm64 => "aarch64-linux-android",
            Self::X86 => "i686-linux-android",
            Self::X86_64 => "x86_64-linux-android",
            Self::Other => "unknown-target",
        }
    }

    pub fn android_abi(self) -> &'static str {
        match self {
            Self::Armv7 => "armeabi-v7a",
            Self::Arm64 => "arm64-v8a",
            Self::X86 => "x86",
            Self::X86_64 => "x86_64",
            Self::Other => "unknown",
        }
    }

    pub fn pointer_bits(self) -> u8 {
        match self {
            Self::Armv7 | Self::X86 => 32,
            Self::Arm64 | Self::X86_64 => 64,
            Self::Other => (std::mem::size_of::<usize>() * 8) as u8,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub abi: AbiTarget,
    pub android_sdk_int: u32,
    pub total_ram_mb: u64,
    pub detected_cli_path: Option<PathBuf>,
    pub detected_cli_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OfficialCliStatus {
    Available {
        binary_path: PathBuf,
        version: String,
    },
    InstallableArm64 {
        manifest_url: String,
    },
    UnsupportedArchitecture {
        abi: AbiTarget,
        reason: String,
    },
    UnsupportedAndroidApi {
        sdk_int: u32,
        minimum_required_sdk: u32,
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSelection {
    Auto,
    PreferNativeCli,
    ForceCompatibleApi,
}

impl Default for RuntimeSelection {
    fn default() -> Self {
        Self::Auto
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionRuntime {
    NativeCli {
        binary_path: PathBuf,
        version: String,
    },
    CompatibleApi {
        endpoint: String,
        fallback_reason: String,
    },
}

impl ExecutionRuntime {
    pub fn badge_label(&self) -> &'static str {
        match self {
            Self::NativeCli { .. } => "RUNTIME: NATIVE CLI",
            Self::CompatibleApi { .. } => "RUNTIME: API-COMPAT",
        }
    }

    pub fn is_native_cli(&self) -> bool {
        matches!(self, Self::NativeCli { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompatibilityReport {
    pub abi: AbiTarget,
    pub rust_triple: String,
    pub android_abi: String,
    pub pointer_bits: u8,
    pub android_sdk_int: u32,
    pub total_ram_mb: u64,
    pub low_ram_optimized: bool,
    pub official_cli_status: OfficialCliStatus,
    pub active_runtime: ExecutionRuntime,
}

// Google's official Antigravity updater publishes android_arm64 built with NDK r27d for API 24+.
// ARMv7, x86, x86_64 Android manifests return HTTP 404, and API 21-23 linkers reject API 24 binaries.
pub const OFFICIAL_ARM64_MANIFEST_URL: &str =
    "https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests/android_arm64.json";
pub const DEFAULT_COMPAT_API_ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta";

pub fn assess_device_compatibility(
    device: &DeviceInfo,
    selection: RuntimeSelection,
    api_endpoint: &str,
) -> CompatibilityReport {
    let endpoint = if api_endpoint.trim().is_empty() {
        DEFAULT_COMPAT_API_ENDPOINT.to_string()
    } else {
        api_endpoint.trim_end_matches('/').to_string()
    };

    let official_cli_status = if device.abi != AbiTarget::Arm64 {
        OfficialCliStatus::UnsupportedArchitecture {
            abi: device.abi,
            reason: format!(
                "Google distributes official Android agy binaries only for arm64-v8a (android_arm64). {} ({}-bit) has no upstream Google CLI build.",
                device.abi.android_abi(),
                device.abi.pointer_bits()
            ),
        }
    } else if device.android_sdk_int < 24 {
        OfficialCliStatus::UnsupportedAndroidApi {
            sdk_int: device.android_sdk_int,
            minimum_required_sdk: 24,
            reason: format!(
                "Google's official android_arm64 agy binary is linked against Android API 24 Bionic symbols; current device runs API {}.",
                device.android_sdk_int
            ),
        }
    } else if let Some(path) = &device.detected_cli_path {
        OfficialCliStatus::Available {
            binary_path: path.clone(),
            version: device
                .detected_cli_version
                .clone()
                .unwrap_or_else(|| "detected".to_string()),
        }
    } else {
        OfficialCliStatus::InstallableArm64 {
            manifest_url: OFFICIAL_ARM64_MANIFEST_URL.to_string(),
        }
    };

    let active_runtime = match (&official_cli_status, selection) {
        (_, RuntimeSelection::ForceCompatibleApi) => ExecutionRuntime::CompatibleApi {
            endpoint,
            fallback_reason: "User selected pure-Rust API compatibility runtime.".to_string(),
        },
        (
            OfficialCliStatus::Available {
                binary_path,
                version,
            },
            RuntimeSelection::Auto | RuntimeSelection::PreferNativeCli,
        ) => ExecutionRuntime::NativeCli {
            binary_path: binary_path.clone(),
            version: version.clone(),
        },
        (OfficialCliStatus::InstallableArm64 { .. }, _) => ExecutionRuntime::CompatibleApi {
            endpoint,
            fallback_reason:
                "Official android_arm64 CLI is compatible with this device but not yet installed in app bin; running Rust API compatibility mode."
                    .to_string(),
        },
        (OfficialCliStatus::UnsupportedArchitecture { reason, .. }, _) => {
            ExecutionRuntime::CompatibleApi {
                endpoint,
                fallback_reason: reason.clone(),
            }
        }
        (OfficialCliStatus::UnsupportedAndroidApi { reason, .. }, _) => {
            ExecutionRuntime::CompatibleApi {
                endpoint,
                fallback_reason: reason.clone(),
            }
        }
    };

    CompatibilityReport {
        abi: device.abi,
        rust_triple: device.abi.rust_triple().to_string(),
        android_abi: device.abi.android_abi().to_string(),
        pointer_bits: device.abi.pointer_bits(),
        android_sdk_int: device.android_sdk_int,
        total_ram_mb: device.total_ram_mb,
        low_ram_optimized: device.total_ram_mb <= 1536,
        official_cli_status,
        active_runtime,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn armv7_api21_selects_compatible_api_honestly() {
        let dev = DeviceInfo {
            abi: AbiTarget::Armv7,
            android_sdk_int: 21,
            total_ram_mb: 1024,
            detected_cli_path: None,
            detected_cli_version: None,
        };
        let report = assess_device_compatibility(&dev, RuntimeSelection::Auto, "");
        assert_eq!(report.pointer_bits, 32);
        assert!(report.low_ram_optimized);
        assert!(!report.active_runtime.is_native_cli());
        assert!(matches!(
            report.official_cli_status,
            OfficialCliStatus::UnsupportedArchitecture {
                abi: AbiTarget::Armv7,
                ..
            }
        ));
    }

    #[test]
    fn arm64_api22_rejects_api24_cli_and_uses_compat_runtime() {
        let dev = DeviceInfo {
            abi: AbiTarget::Arm64,
            android_sdk_int: 22,
            total_ram_mb: 2048,
            detected_cli_path: Some(PathBuf::from("/data/local/tmp/agy")),
            detected_cli_version: Some("1.5.16".to_string()),
        };
        let report = assess_device_compatibility(&dev, RuntimeSelection::Auto, "");
        assert!(!report.active_runtime.is_native_cli());
        assert!(matches!(
            report.official_cli_status,
            OfficialCliStatus::UnsupportedAndroidApi { sdk_int: 22, .. }
        ));
    }

    #[test]
    fn arm64_api29_with_binary_uses_native_cli_unless_forced() {
        let dev = DeviceInfo {
            abi: AbiTarget::Arm64,
            android_sdk_int: 29,
            total_ram_mb: 4096,
            detected_cli_path: Some(PathBuf::from("/data/user/0/app/files/bin/agy")),
            detected_cli_version: Some("1.5.16".to_string()),
        };
        let native_report = assess_device_compatibility(&dev, RuntimeSelection::Auto, "");
        assert!(native_report.active_runtime.is_native_cli());

        let forced_compat =
            assess_device_compatibility(&dev, RuntimeSelection::ForceCompatibleApi, "");
        assert!(!forced_compat.active_runtime.is_native_cli());
    }
}
