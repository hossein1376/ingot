//! The target record `@target` (spec section 24).
//!
//! Each of the three targets pairs one CPU of section 24.1 with one operating system of
//! section 24.2. The values of `@target.arch` and `@target.cpu`, and the feature names
//! of `@target.has(...)`, are the ones section 24.3 lists.

/// The three targets of section 24.
pub const TARGETS: [&str; 3] = [
    "aarch64-macos-m4",
    "x86_64-linux-znver4",
    "x86_64-windows-znver4",
];

/// The fields of `@target` (section 24.3). `has` is the feature query.
pub const TARGET_FIELDS: [&str; 10] = [
    "os",
    "arch",
    "cpu",
    "simd_bits",
    "cache_line",
    "page_size",
    "hybrid_cores",
    "smt",
    "gpu_unified",
    "has",
];

/// The values of `@target.os` (section 24.2).
pub const OS_VARIANTS: [&str; 3] = ["macos", "linux", "windows"];

/// The values of `@target.arch` (section 24.3).
pub const ARCH_VARIANTS: [&str; 2] = ["aarch64", "x86_64"];

/// The values of `@target.cpu` (section 24.3).
pub const CPU_VARIANTS: [&str; 2] = ["m4", "znver4"];

/// The feature names of the M4 Pro: the rows of its column in section 24.1, in lower
/// case.
pub const M4_FEATURES: [&str; 10] = [
    "neon", "sme", "sme2", "lse", "crc32", "aes", "sha2", "sha3", "fp16", "dotprod",
];

/// The feature names of the 7800X3D, the CPU of both x86-64 targets: the rows of its
/// column in section 24.1, in lower case.
pub const ZEN4_FEATURES: [&str; 24] = [
    "avx512f",
    "avx512cd",
    "avx512bw",
    "avx512dq",
    "avx512vl",
    "avx512ifma",
    "avx512vbmi",
    "avx512vbmi2",
    "avx512vnni",
    "avx512bitalg",
    "avx512vpopcntdq",
    "avx512bf16",
    "avx2",
    "fma3",
    "bmi1",
    "bmi2",
    "popcnt",
    "lzcnt",
    "movbe",
    "adx",
    "aesni",
    "vaes",
    "gfni",
    "sha",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub name: &'static str,
    pub os: &'static str,
    pub arch: &'static str,
    pub cpu: &'static str,
    /// The feature names of this target's CPU: `M4_FEATURES` or `ZEN4_FEATURES`.
    pub features: &'static [&'static str],
    pub simd_bits: i128,
    pub cache_line: i128,
    pub page_size: i128,
    pub hybrid_cores: bool,
    pub smt: bool,
    pub gpu_unified: bool,
}

impl Target {
    /// The target called `name`, or `None` if it is not one of the three.
    pub fn by_name(name: &str) -> Option<Target> {
        match name {
            "aarch64-macos-m4" => Some(Target {
                name: TARGETS[0],
                os: "macos",
                arch: "aarch64",
                cpu: "m4",
                features: &M4_FEATURES,
                simd_bits: 128,
                cache_line: 128,
                page_size: 16384,
                hybrid_cores: true,
                smt: false,
                gpu_unified: true,
            }),
            "x86_64-linux-znver4" => Some(Target {
                name: TARGETS[1],
                os: "linux",
                arch: "x86_64",
                cpu: "znver4",
                features: &ZEN4_FEATURES,
                simd_bits: 512,
                cache_line: 64,
                page_size: 4096,
                hybrid_cores: false,
                smt: true,
                gpu_unified: false,
            }),
            "x86_64-windows-znver4" => Some(Target {
                name: TARGETS[2],
                os: "windows",
                arch: "x86_64",
                cpu: "znver4",
                features: &ZEN4_FEATURES,
                simd_bits: 512,
                cache_line: 64,
                page_size: 4096,
                hybrid_cores: false,
                smt: true,
                gpu_unified: false,
            }),
            _ => None,
        }
    }

    /// The default target of `ingot check` (the first of section 24).
    pub fn default_target() -> Target {
        Target::by_name(TARGETS[0]).expect("the first target exists")
    }

    /// Whether this target's CPU has the feature `name` (section 24.3): `Some(true)` for
    /// a feature of this CPU, `Some(false)` for a feature of the other CPU, and `None`
    /// for a name in neither list.
    pub fn has(&self, name: &str) -> Option<bool> {
        if self.features.contains(&name) {
            Some(true)
        } else if M4_FEATURES.contains(&name) || ZEN4_FEATURES.contains(&name) {
            Some(false)
        } else {
            None
        }
    }
}
