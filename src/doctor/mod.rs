use serde::{Deserialize, Serialize};

use crate::snapshot::Snapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: String,
    pub severity: Severity,
    pub title: String,
    pub evidence: Vec<String>,
    #[serde(default)]
    pub origins: Vec<String>,
    pub impact: String,
    pub action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub diagnostics: Vec<Diagnostic>,
}

impl DoctorReport {
    pub fn has_problems(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity != Severity::Info)
    }

    pub fn problem_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity != Severity::Info)
            .count()
    }
}

pub fn diagnose(snapshot: &Snapshot) -> DoctorReport {
    let mut diagnostics = Vec::new();
    cuda_mismatch(snapshot, &mut diagnostics);
    missing_cuda_home(snapshot, &mut diagnostics);
    inactive_virtual_env(snapshot, &mut diagnostics);
    pip_interpreter_mismatch(snapshot, &mut diagnostics);
    executable_shadowing(snapshot, &mut diagnostics);
    cuda_home_mismatch(snapshot, &mut diagnostics);
    cuda_driver_too_old(snapshot, &mut diagnostics);
    compiler_mismatch(snapshot, &mut diagnostics);
    missing_build_tools(snapshot, &mut diagnostics);
    project_manifests(snapshot, &mut diagnostics);
    git_state(snapshot, &mut diagnostics);
    missing_environment_paths(snapshot, &mut diagnostics);
    duplicate_path_entries(snapshot, &mut diagnostics);
    DoctorReport { diagnostics }
}

fn project_manifests(s: &Snapshot, out: &mut Vec<Diagnostic>) {
    let root = &s.project.path;

    if root.join("Cargo.toml").is_file() && !root.join("Cargo.lock").is_file() {
        missing_lockfile(out, "Rust", "Cargo.lock", "cargo generate-lockfile");
    }
    if root.join("package.json").is_file()
        && ![
            "package-lock.json",
            "pnpm-lock.yaml",
            "yarn.lock",
            "bun.lock",
            "bun.lockb",
        ]
        .iter()
        .any(|name| root.join(name).is_file())
    {
        missing_lockfile(
            out,
            "Node",
            "a package-manager lockfile",
            "run the chosen package manager's install command",
        );
    }
    if root.join("pyproject.toml").is_file()
        && !["uv.lock", "poetry.lock", "pdm.lock", "requirements.txt"]
            .iter()
            .any(|name| root.join(name).is_file())
    {
        out.push(Diagnostic {
            code: "project.python-lockfile-missing".into(), severity: Severity::Info,
            title: "Python project has no detected dependency lock or requirements file".into(),
            evidence: vec![root.join("pyproject.toml").display().to_string()], origins: Vec::new(),
            impact: "Dependency resolution can produce different environments over time.".into(),
            action: "Generate the lockfile used by this project's package manager, if reproducible application installs are intended.".into(),
        });
    }

    if let Some(value) = read_project_text(root, "rust-toolchain.toml")
        .or_else(|| read_project_text(root, "rust-toolchain"))
        && let Some(required) = first_version(&value)
        && let Some(actual) = s.build.rustc.version.as_deref().and_then(first_version)
        && version_pair(actual).0 != version_pair(required).0
    {
        version_mismatch(
            out,
            "rust",
            required,
            actual,
            "Activate the declared Rust toolchain with rustup.",
        );
    }

    if let Some(value) = read_project_text(root, "package.json")
        && let Some(required) = json_engine_node(&value)
        && let Some(actual) = s.build.node.version.as_deref().and_then(first_version)
        && let Some(minimum) = minimum_version(&required)
        && version_pair(actual) < version_pair(minimum)
    {
        version_mismatch(
            out,
            "node",
            &required,
            actual,
            "Install or activate a Node version satisfying package.json engines.node.",
        );
    }

    if let Some(value) = read_project_text(root, "pyproject.toml")
        && let Some(required) = toml_string(&value, "requires-python")
        && let Some(actual) = s.python.version.as_deref()
        && let Some(minimum) = minimum_version(required)
        && version_pair(actual) < version_pair(minimum)
    {
        version_mismatch(
            out,
            "python",
            required,
            actual,
            "Create or activate an environment satisfying requires-python.",
        );
    }

    let declares_cuda_extension = ["setup.py", "pyproject.toml", "CMakeLists.txt"]
        .iter()
        .filter_map(|name| read_project_text(root, name))
        .any(|text| {
            text.contains("CUDAExtension")
                || text.contains("CUDAToolkit")
                || text.contains("LANGUAGES CUDA")
        });
    if declares_cuda_extension && s.cuda.nvcc_path.is_none() {
        out.push(Diagnostic {
            code: "project.cuda-build-toolkit-missing".into(),
            severity: Severity::Error,
            title: "Project declares CUDA compilation but nvcc is unavailable".into(),
            evidence: vec![format!("project: {}", root.display())],
            origins: origin(s, "CUDA_HOME"),
            impact: "Native CUDA extensions cannot be compiled in this environment.".into(),
            action: "Install a compatible CUDA toolkit and expose nvcc through PATH or CUDA_HOME."
                .into(),
        });
    }
}

fn read_project_text(root: &std::path::Path, name: &str) -> Option<String> {
    crate::snapshot::command::read_text(&root.join(name), 1024 * 1024)
}

fn missing_lockfile(out: &mut Vec<Diagnostic>, ecosystem: &str, expected: &str, action: &str) {
    out.push(Diagnostic {
        code: format!("project.{}-lockfile-missing", ecosystem.to_lowercase()),
        severity: Severity::Warning,
        title: format!("{ecosystem} project is missing {expected}"),
        evidence: Vec::new(),
        origins: Vec::new(),
        impact: "Dependency resolution can change between machines or over time.".into(),
        action: format!("To make installs reproducible, {action}."),
    });
}

fn version_mismatch(
    out: &mut Vec<Diagnostic>,
    tool: &str,
    required: &str,
    actual: &str,
    action: &str,
) {
    out.push(Diagnostic {
        code: format!("project.{tool}-version"),
        severity: Severity::Error,
        title: format!("Active {tool} does not satisfy the project requirement"),
        evidence: vec![format!("required: {required}"), format!("active: {actual}")],
        origins: Vec::new(),
        impact: "Builds or dependency installation may fail or behave differently than expected."
            .into(),
        action: action.into(),
    });
}

fn minimum_version(value: &str) -> Option<&str> {
    value
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .find(|part| part.chars().next().is_some_and(|c| c.is_ascii_digit()))
}

fn toml_string<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let line = line.trim();
        let value = line
            .strip_prefix(key)?
            .trim_start()
            .strip_prefix('=')?
            .trim();
        value.strip_prefix(['\"', '\''])?.split(['\"', '\'']).next()
    })
}

fn json_engine_node(text: &str) -> Option<String> {
    let engines = text.find("\"engines\"")?;
    let node = text[engines..].find("\"node\"")? + engines;
    let value = text[node + 6..].split_once(':')?.1.trim_start();
    Some(
        value
            .trim_start_matches('\"')
            .split('\"')
            .next()?
            .to_owned(),
    )
}

fn cuda_mismatch(s: &Snapshot, out: &mut Vec<Diagnostic>) {
    let (Some(torch), Some(nvcc)) = (&s.python.torch_cuda, &s.cuda.nvcc_version) else {
        return;
    };
    if major_minor(torch) != major_minor(nvcc) {
        out.push(Diagnostic {
            code: "cuda.toolkit-mismatch".into(), severity: Severity::Error,
            title: "CUDA toolkit mismatch".into(),
            evidence: vec![format!("PyTorch CUDA: {torch}"), format!("nvcc: {nvcc}")],
            origins: origin(s, "CUDA_HOME"),
            impact: "Compiling CUDA extensions may fail or produce binaries for the wrong toolkit.".into(),
            action: "Activate a CUDA toolkit matching PyTorch, or install a PyTorch build matching nvcc.".into(),
        });
    }
}

fn missing_cuda_home(s: &Snapshot, out: &mut Vec<Diagnostic>) {
    if let Some(home) = &s.cuda.cuda_home
        && !home.is_dir()
    {
        out.push(Diagnostic {
            code: "cuda.home-missing".into(),
            severity: Severity::Error,
            title: "CUDA_HOME does not exist".into(),
            evidence: vec![format!("CUDA_HOME: {}", home.display())],
            origins: origin(s, "CUDA_HOME"),
            impact: "Build tools that rely on CUDA_HOME cannot locate the toolkit.".into(),
            action: "Unset CUDA_HOME or point it at an installed CUDA toolkit.".into(),
        });
    }
}

fn inactive_virtual_env(s: &Snapshot, out: &mut Vec<Diagnostic>) {
    let Some(active) = &s.python.virtual_env else {
        return;
    };
    let Some(prefix) = &s.python.prefix else {
        return;
    };
    if normalize(active) != normalize(prefix) {
        out.push(Diagnostic {
            code: "python.venv-mismatch".into(), severity: Severity::Error,
            title: "VIRTUAL_ENV and active Python disagree".into(),
            evidence: vec![format!("VIRTUAL_ENV: {}", active.display()), format!("Python prefix: {}", prefix.display())],
            origins: origin(s, "VIRTUAL_ENV"),
            impact: "Python and pip commands may install into or run from different environments.".into(),
            action: "Reactivate the intended virtual environment and verify `command -v python` and `command -v pip`.".into(),
        });
    }
}

fn missing_environment_paths(s: &Snapshot, out: &mut Vec<Diagnostic>) {
    for name in ["HF_HOME", "TORCH_HOME", "WAN_CKPT"] {
        let Some(value) = s.environment.variables.get(name) else {
            continue;
        };
        if !std::path::Path::new(value).exists() {
            out.push(Diagnostic {
                code: format!("environment.{}-missing", name.to_lowercase()),
                severity: Severity::Warning,
                title: format!("{name} points to a missing path"),
                evidence: vec![format!("{name}: {value}")],
                origins: origin(s, name),
                impact: "Tools using this variable may fail or silently use a fallback location."
                    .into(),
                action: format!(
                    "Create the path, correct {name}, or unset it if it is no longer needed."
                ),
            });
        }
    }
}

fn duplicate_path_entries(s: &Snapshot, out: &mut Vec<Diagnostic>) {
    use std::collections::BTreeSet;
    let mut seen = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    for path in &s.environment.path_entries {
        if !seen.insert(path) {
            duplicates.insert(path);
        }
    }
    if !duplicates.is_empty() {
        out.push(Diagnostic {
            code: "environment.path-duplicates".into(),
            severity: Severity::Info,
            title: "PATH contains duplicate entries".into(),
            evidence: duplicates.iter().map(|p| p.display().to_string()).collect(),
            origins: Vec::new(),
            impact: "Usually harmless, but duplicate entries make executable resolution harder to reason about.".into(),
            action: "Remove repeated PATH additions from shell startup files.".into(),
        });
    }
}

fn pip_interpreter_mismatch(s: &Snapshot, out: &mut Vec<Diagnostic>) {
    let (Some(python), Some(pip_python)) = (&s.python.version, &s.python.pip_python_version) else {
        return;
    };
    if major_minor(python) != major_minor(pip_python) {
        out.push(Diagnostic {
            code: "python.pip-interpreter-mismatch".into(),
            severity: Severity::Error,
            title: "python and pip use different interpreters".into(),
            evidence: vec![
                format!("python: {python} ({})", display_path(s.python.executable.as_deref())),
                format!("pip Python: {pip_python} ({})", display_path(s.python.pip_executable.as_deref())),
            ],
            origins: Vec::new(),
            impact: "Packages may be installed into an environment different from the one running your code.".into(),
            action: "Use `python -m pip` or activate the intended environment before installing packages.".into(),
        });
    }
}

fn executable_shadowing(s: &Snapshot, out: &mut Vec<Diagnostic>) {
    let tools = [
        ("python", &s.python.executable_candidates),
        ("pip", &s.python.pip_candidates),
        ("gcc", &s.build.gcc.candidates),
        ("g++", &s.build.gxx.candidates),
        ("clang", &s.build.clang.candidates),
    ];
    for (name, candidates) in tools {
        let distinct = candidates
            .iter()
            .map(|path| std::fs::canonicalize(path).unwrap_or_else(|_| path.clone()))
            .collect::<std::collections::BTreeSet<_>>();
        if distinct.len() > 1 {
            out.push(Diagnostic {
                code: format!("environment.{name}-shadowed"),
                severity: Severity::Info,
                title: format!("multiple {name} executables are visible"),
                evidence: candidates.iter().map(|p| p.display().to_string()).collect(),
                origins: Vec::new(),
                impact: format!("Different shells or PATH ordering may select a different {name}."),
                action: format!("Verify `command -v {name}` before builds that depend on it."),
            });
        }
    }
}

fn cuda_home_mismatch(s: &Snapshot, out: &mut Vec<Diagnostic>) {
    let (Some(home), Some(torch)) = (&s.cuda.cuda_home, &s.python.torch_cuda) else {
        return;
    };
    let home_text = home.to_string_lossy();
    let Some(home_version) = version_from_path(&home_text) else {
        return;
    };
    if major_minor(&home_version) != major_minor(torch) {
        out.push(Diagnostic {
            code: "cuda.home-mismatch".into(), severity: Severity::Error,
            title: "CUDA_HOME and PyTorch CUDA disagree".into(),
            evidence: vec![format!("torch CUDA: {torch}"), format!("CUDA_HOME: {}", home.display())],
            origins: origin(s, "CUDA_HOME"),
            impact: "CUDA extensions may compile against a toolkit different from the PyTorch runtime.".into(),
            action: "Point CUDA_HOME at the toolkit matching PyTorch, or install a matching PyTorch build.".into(),
        });
    }
}

fn cuda_driver_too_old(s: &Snapshot, out: &mut Vec<Diagnostic>) {
    let (Some(capability), Some(torch)) = (&s.cuda.driver_cuda, &s.python.torch_cuda) else {
        return;
    };
    if version_pair(capability) < version_pair(torch) {
        out.push(Diagnostic {
            code: "cuda.driver-capability".into(),
            severity: Severity::Error,
            title: "NVIDIA driver CUDA capability is too old".into(),
            evidence: vec![
                format!("driver capability: CUDA {capability}"),
                format!("PyTorch CUDA: {torch}"),
            ],
            origins: Vec::new(),
            impact: "CUDA initialization can fail even when the toolkit is installed correctly."
                .into(),
            action:
                "Upgrade the NVIDIA driver or install a PyTorch build supported by this driver."
                    .into(),
        });
    }
}

fn compiler_mismatch(s: &Snapshot, out: &mut Vec<Diagnostic>) {
    let pairs = [
        ("gcc", &s.build.gcc, "g++", &s.build.gxx),
        ("clang", &s.build.clang, "clang++", &s.build.clangxx),
    ];
    for (left_name, left, right_name, right) in pairs {
        let (Some(a), Some(b)) = (&left.version, &right.version) else {
            continue;
        };
        if first_version(a).and_then(|v| v.split('.').next())
            != first_version(b).and_then(|v| v.split('.').next())
        {
            out.push(Diagnostic {
                code: format!("build.{left_name}-{right_name}-mismatch"),
                severity: Severity::Warning,
                title: format!("{left_name} and {right_name} major versions differ"),
                evidence: vec![format!("{left_name}: {a}"), format!("{right_name}: {b}")],
                origins: Vec::new(),
                impact: "C and C++ objects may be compiled with incompatible toolchain versions."
                    .into(),
                action: "Select matching compiler binaries through CC/CXX and PATH.".into(),
            });
        }
    }
}

fn missing_build_tools(s: &Snapshot, out: &mut Vec<Diagnostic>) {
    let cmake_project = s.project.path.join("CMakeLists.txt").exists();
    if cmake_project && s.build.cmake.path.is_none() {
        out.push(Diagnostic {
            code: "build.cmake-missing".into(),
            severity: Severity::Error,
            title: "CMake project detected but cmake is missing".into(),
            evidence: vec![format!("project: {}", s.project.path.display())],
            origins: Vec::new(),
            impact: "The project cannot be configured with its declared build system.".into(),
            action: "Install CMake and ensure it is visible in PATH.".into(),
        });
    }
}

fn git_state(s: &Snapshot, out: &mut Vec<Diagnostic>) {
    if s.git.detached == Some(true) {
        out.push(Diagnostic {
            code: "git.detached-head".into(),
            severity: Severity::Warning,
            title: "Git is in detached HEAD state".into(),
            evidence: vec![format!(
                "commit: {}",
                s.git.commit.as_deref().unwrap_or("unknown")
            )],
            origins: Vec::new(),
            impact: "New commits can become difficult to find unless a branch is created.".into(),
            action: "Create or switch to a branch before committing work.".into(),
        });
    }
    if s.git.submodules_dirty == Some(true) {
        out.push(Diagnostic {
            code: "git.submodule-mismatch".into(),
            severity: Severity::Warning,
            title: "Git submodules do not match the recorded checkout".into(),
            evidence: Vec::new(),
            origins: Vec::new(),
            impact: "Builds may use missing, modified, or unexpected dependency revisions.".into(),
            action: "Inspect `git submodule status --recursive` and synchronize intentionally."
                .into(),
        });
    }
    if s.git.dirty == Some(true) {
        out.push(Diagnostic {
            code: "git.dirty".into(),
            severity: Severity::Info,
            title: "Git working tree has uncommitted changes".into(),
            evidence: Vec::new(),
            origins: Vec::new(),
            impact:
                "The current environment may not be reproducible from the recorded commit alone."
                    .into(),
            action: "Commit, stash, or record the local changes when sharing a snapshot.".into(),
        });
    }
}

fn origin(s: &Snapshot, name: &str) -> Vec<String> {
    s.environment
        .provenance
        .get(name)
        .map(|source| {
            vec![format!(
                "{}:{} ({})",
                source.file.display(),
                source.line,
                source.confidence
            )]
        })
        .unwrap_or_default()
}

fn display_path(path: Option<&std::path::Path>) -> String {
    path.map(|p| p.display().to_string())
        .unwrap_or_else(|| "not detected".into())
}

fn version_from_path(value: &str) -> Option<String> {
    value
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .find(|part| part.contains('.') && part.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .map(str::to_owned)
}

fn first_version(value: &str) -> Option<&str> {
    value
        .split_whitespace()
        .find(|part| part.chars().next().is_some_and(|c| c.is_ascii_digit()))
}

fn version_pair(value: &str) -> (u32, u32) {
    let mut parts = value.split('.').filter_map(|part| part.parse().ok());
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

fn major_minor(value: &str) -> &str {
    let end = value
        .match_indices('.')
        .nth(1)
        .map(|(i, _)| i)
        .unwrap_or(value.len());
    &value[..end]
}

fn normalize(path: &std::path::Path) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_cuda_mismatch() {
        let mut s = Snapshot::default();
        s.python.torch_cuda = Some("12.8".into());
        s.cuda.nvcc_version = Some("12.4".into());
        let report = diagnose(&s);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == "cuda.toolkit-mismatch")
        );
    }

    #[test]
    fn accepts_matching_cuda_patch_versions() {
        let mut s = Snapshot::default();
        s.python.torch_cuda = Some("12.8".into());
        s.cuda.nvcc_version = Some("12.8.1".into());
        assert!(
            !diagnose(&s)
                .diagnostics
                .iter()
                .any(|d| d.code == "cuda.toolkit-mismatch")
        );
    }

    #[test]
    fn duplicate_path_is_informational() {
        let mut s = Snapshot::default();
        s.environment.path_entries = vec!["/bin".into(), "/bin".into()];
        let report = diagnose(&s);
        assert_eq!(report.diagnostics.len(), 1);
        assert_eq!(report.diagnostics[0].severity, Severity::Info);
        assert!(!report.has_problems());
        assert_eq!(report.problem_count(), 0);
    }

    #[test]
    fn detects_pip_interpreter_mismatch() {
        let mut s = Snapshot::default();
        s.python.version = Some("3.12.4".into());
        s.python.pip_python_version = Some("3.11".into());
        assert!(
            diagnose(&s)
                .diagnostics
                .iter()
                .any(|d| d.code == "python.pip-interpreter-mismatch")
        );
    }

    #[test]
    fn detects_driver_capability_mismatch() {
        let mut s = Snapshot::default();
        s.python.torch_cuda = Some("12.8".into());
        s.cuda.driver_cuda = Some("12.4".into());
        assert!(
            diagnose(&s)
                .diagnostics
                .iter()
                .any(|d| d.code == "cuda.driver-capability")
        );
    }
}
