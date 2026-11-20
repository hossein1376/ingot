//! The back-end half of `ingot build` and `ingot run`: code generation and linking.

use crate::pipeline::Analysis;
use ingot_codegen::{BuildError, BuildOptions};
use std::path::{Path, PathBuf};

/// A linked executable and the IR text it came from.
#[derive(Clone, Debug)]
pub struct Built {
    pub artifact: PathBuf,
    pub ir: String,
}

/// The default artifact: a package-relative build directory.
pub fn default_artifact(root: &Path, package: &str) -> PathBuf {
    root.join("target").join("ingot").join(package)
}

/// Generate and link the program in an analyzed package. The caller has already
/// printed diagnostics; this function only runs when the front end accepted.
pub fn build_analysis(
    analysis: &mut Analysis,
    root: &Path,
    output: Option<&Path>,
    fast: bool,
) -> Result<Built, BuildError> {
    build_analysis_as(analysis, root, output, fast, false)
}

/// The default artifact of `ingot test`: next to the program's, under a name the
/// program's executable cannot have.
pub fn default_test_artifact(root: &Path, package: &str) -> PathBuf {
    default_artifact(root, &format!("{package}-test"))
}

/// `build_analysis`, or with `tests` the executable of the `test` blocks among the
/// analysis's instances (`Roots::Test`): its entry calls `std.sys.rt.test_start`.
pub fn build_analysis_as(
    analysis: &mut Analysis,
    root: &Path,
    output: Option<&Path>,
    fast: bool,
    tests: bool,
) -> Result<Built, BuildError> {
    let Some(front) = analysis.front_end.as_mut() else {
        return Err(BuildError::Internal(
            "the front end produced no result to build".to_string(),
        ));
    };
    let package = front
        .resolved
        .sema
        .packages
        .get(front.checked)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "app".to_string());
    let path = output.map(Path::to_path_buf).unwrap_or_else(|| {
        if tests {
            default_test_artifact(root, &package)
        } else {
            default_artifact(root, &package)
        }
    });
    let options = BuildOptions {
        target: analysis.target.clone(),
        fast,
        output: path,
        root: root.to_path_buf(),
    };
    let result = {
        let program = ingot_codegen::Program {
            sources: &analysis.sources,
            sema: &front.resolved.sema,
            decls: &mut front.declarations,
            cfgs: &analysis.removed_cfgs,
            extra_cfgs: &analysis.extra_cfgs,
            instances: &analysis.instances,
            checked: front.checked,
            checked_packages: &front.checked_packages,
            target: &analysis.target,
            test_build: tests,
        };
        ingot_codegen::build(program, &options)?
    };
    Ok(Built {
        artifact: result.path,
        ir: result.ir,
    })
}

/// The directory of the package `path` belongs to: `path` itself for a directory, the
/// nearest manifest's directory for a file, else the file's own directory.
pub fn package_root(path: &Path) -> PathBuf {
    if path.is_dir() {
        return path.to_path_buf();
    }
    let files = crate::files::Files::default();
    crate::loader::manifest_dir(path, &files)
        .unwrap_or_else(|| path.parent().unwrap_or(Path::new(".")).to_path_buf())
}
