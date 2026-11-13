//! The target record `@target` (spec section 24) as far as the specification
//! defines it.
//!
//! The facts below are the ones the specification states (section 24.1 and 24.2).
//! The specification does not name the values of `@target.arch` and `@target.cpu`,
//! and it names only one feature of `.has(...)`, `sme`. Those are modelled as
//! "not defined": reading them is a compiler failure (`not implemented`), never a
//! guess.

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub name: &'static str,
    pub os: &'static str,
    pub simd_bits: i128,
    pub cache_line: i128,
    pub page_size: i128,
    pub hybrid_cores: bool,
    pub smt: bool,
    pub gpu_unified: bool,
    /// Whether the CPU has SME (section 24.1), the one feature the specification
    /// names as a spelling.
    pub sme: bool,
}

impl Target {
    /// The target called `name`, or `None` if it is not one of the three.
    pub fn by_name(name: &str) -> Option<Target> {
        match name {
            "aarch64-macos-m4" => Some(Target {
                name: TARGETS[0],
                os: "macos",
                simd_bits: 128,
                cache_line: 128,
                page_size: 16384,
                hybrid_cores: true,
                smt: false,
                gpu_unified: true,
                sme: true,
            }),
            "x86_64-linux-znver4" => Some(Target {
                name: TARGETS[1],
                os: "linux",
                simd_bits: 512,
                cache_line: 64,
                page_size: 4096,
                hybrid_cores: false,
                smt: true,
                gpu_unified: false,
                sme: false,
            }),
            "x86_64-windows-znver4" => Some(Target {
                name: TARGETS[2],
                os: "windows",
                simd_bits: 512,
                cache_line: 64,
                page_size: 4096,
                hybrid_cores: false,
                smt: true,
                gpu_unified: false,
                sme: false,
            }),
            _ => None,
        }
    }

    /// The default target of `ingot check` (the first of section 24).
    pub fn default_target() -> Target {
        Target::by_name(TARGETS[0]).expect("the first target exists")
    }
}
