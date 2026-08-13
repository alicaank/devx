mod build;
pub(crate) mod command;
mod cuda;
mod environment;
mod git;
mod python;
mod system;

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Snapshot {
    pub schema_version: u32,
    pub captured_at: DateTime<Utc>,
    pub system: SystemInfo,
    pub project: ProjectInfo,
    pub git: GitInfo,
    pub python: PythonInfo,
    pub cuda: CudaInfo,
    #[serde(default)]
    pub build: BuildInfo,
    #[serde(default)]
    pub disk: DiskInfo,
    #[serde(default)]
    pub processes: Vec<crate::processes::ProjectProcesses>,
    pub environment: EnvironmentInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SystemInfo {
    pub os: Option<String>,
    pub architecture: String,
    pub hostname: Option<String>,
    pub kernel: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectInfo {
    pub path: PathBuf,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct GitInfo {
    pub root: Option<PathBuf>,
    pub commit: Option<String>,
    pub branch: Option<String>,
    pub dirty: Option<bool>,
    pub detached: Option<bool>,
    pub submodules_dirty: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PythonInfo {
    pub executable: Option<PathBuf>,
    pub version: Option<String>,
    pub prefix: Option<PathBuf>,
    pub base_prefix: Option<PathBuf>,
    pub virtual_env: Option<PathBuf>,
    pub conda_prefix: Option<PathBuf>,
    pub torch_version: Option<String>,
    pub torch_cuda: Option<String>,
    pub pip_executable: Option<PathBuf>,
    pub pip_version: Option<String>,
    pub pip_python_version: Option<String>,
    pub module_pip_version: Option<String>,
    pub executable_candidates: Vec<PathBuf>,
    pub pip_candidates: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CudaInfo {
    pub cuda_home: Option<PathBuf>,
    pub nvcc_path: Option<PathBuf>,
    pub nvcc_version: Option<String>,
    pub driver_version: Option<String>,
    pub driver_cuda: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ToolInfo {
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    pub candidates: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct BuildInfo {
    pub gcc: ToolInfo,
    pub gxx: ToolInfo,
    pub clang: ToolInfo,
    pub clangxx: ToolInfo,
    pub cmake: ToolInfo,
    pub ninja: ToolInfo,
    pub rustc: ToolInfo,
    pub node: ToolInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DiskInfo {
    pub path: PathBuf,
    pub total_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
    pub usage_percent: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EnvironmentInfo {
    pub path_entries: Vec<PathBuf>,
    pub variables: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub provenance: std::collections::BTreeMap<String, EnvironmentSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EnvironmentSource {
    pub file: PathBuf,
    pub line: usize,
    pub confidence: String,
}

pub fn scan(path: &Path) -> Result<Snapshot> {
    scan_with(path, true)
}

pub fn scan_profile(path: &Path, minimal: bool) -> Result<Snapshot> {
    if minimal {
        let mut snapshot = scan_with(path, false)?;
        snapshot.disk = DiskInfo::default();
        snapshot.environment = EnvironmentInfo::default();
        Ok(snapshot)
    } else {
        scan(path)
    }
}

pub fn redact_paths(snapshot: &mut Snapshot) {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let project = snapshot.project.path.clone();
    let project_text = project.to_string_lossy().into_owned();
    let home_text = home
        .as_deref()
        .map(|path| path.to_string_lossy().into_owned());
    let redact_text = |value: &str| {
        let value = value.replace(&project_text, "<project>");
        home_text
            .as_ref()
            .map_or(value.clone(), |home| value.replace(home, "<home>"))
    };
    let redact = |path: &Path| {
        if let Ok(relative) = path.strip_prefix(&project) {
            PathBuf::from("<project>").join(relative)
        } else if let Some(home) = &home
            && let Ok(relative) = path.strip_prefix(home)
        {
            PathBuf::from("<home>").join(relative)
        } else {
            path.to_path_buf()
        }
    };
    snapshot.project.path = redact(&snapshot.project.path);
    snapshot.git.root = snapshot.git.root.as_deref().map(&redact);
    snapshot.python.executable = snapshot.python.executable.as_deref().map(&redact);
    snapshot.python.prefix = snapshot.python.prefix.as_deref().map(&redact);
    snapshot.python.base_prefix = snapshot.python.base_prefix.as_deref().map(&redact);
    snapshot.python.virtual_env = snapshot.python.virtual_env.as_deref().map(&redact);
    snapshot.python.conda_prefix = snapshot.python.conda_prefix.as_deref().map(&redact);
    snapshot.python.pip_executable = snapshot.python.pip_executable.as_deref().map(&redact);
    snapshot.python.executable_candidates = snapshot
        .python
        .executable_candidates
        .iter()
        .map(|path| redact(path))
        .collect();
    snapshot.python.pip_candidates = snapshot
        .python
        .pip_candidates
        .iter()
        .map(|path| redact(path))
        .collect();
    for tool in [
        &mut snapshot.build.gcc,
        &mut snapshot.build.gxx,
        &mut snapshot.build.clang,
        &mut snapshot.build.clangxx,
        &mut snapshot.build.cmake,
        &mut snapshot.build.ninja,
        &mut snapshot.build.rustc,
        &mut snapshot.build.node,
    ] {
        tool.path = tool.path.as_deref().map(&redact);
        tool.candidates = tool.candidates.iter().map(|path| redact(path)).collect();
    }
    snapshot.cuda.cuda_home = snapshot.cuda.cuda_home.as_deref().map(&redact);
    snapshot.cuda.nvcc_path = snapshot.cuda.nvcc_path.as_deref().map(&redact);
    snapshot.disk.path = redact(&snapshot.disk.path);
    snapshot.environment.path_entries = snapshot
        .environment
        .path_entries
        .iter()
        .map(|p| redact(p))
        .collect();
    for source in snapshot.environment.provenance.values_mut() {
        source.file = redact(&source.file);
    }
    for value in snapshot.environment.variables.values_mut() {
        *value = redact_text(value);
    }
    for process in &mut snapshot.processes {
        process.root = redact(&process.root);
        process.attribution_evidence = process
            .attribution_evidence
            .iter()
            .map(|value| redact_text(value))
            .collect();
        for item in &mut process.processes {
            item.cwd = redact(&item.cwd);
            item.attribution_evidence = redact_text(&item.attribution_evidence);
        }
    }
}

pub fn write(path: &Path, snapshot: &Snapshot, compact: bool) -> Result<()> {
    let file =
        fs::File::create(path).with_context(|| format!("cannot create {}", path.display()))?;
    if compact {
        serde_json::to_writer(file, snapshot)?;
    } else {
        serde_json::to_writer_pretty(file, snapshot)?;
    }
    Ok(())
}

fn scan_with(path: &Path, include_processes: bool) -> Result<Snapshot> {
    let path = fs::canonicalize(path)
        .with_context(|| format!("cannot resolve project path {}", path.display()))?;
    let name = path.file_name().map(|v| v.to_string_lossy().into_owned());
    let (system, git, python, cuda, build, disk, processes, environment) =
        std::thread::scope(|scope| {
            let system = scope.spawn(system::scan);
            let git = scope.spawn(|| git::scan(&path));
            let python = scope.spawn(|| python::scan(&path));
            let cuda = scope.spawn(|| cuda::scan(&path));
            let build = scope.spawn(|| build::scan(&path));
            let disk = scope.spawn(|| crate::disk::capacity(&path));
            let processes = scope.spawn(|| {
                if include_processes {
                    crate::processes::scan(Default::default()).projects
                } else {
                    Vec::new()
                }
            });
            let environment = scope.spawn(environment::scan);
            (
                system.join().unwrap_or_default(),
                git.join().unwrap_or_default(),
                python.join().unwrap_or_default(),
                cuda.join().unwrap_or_default(),
                build.join().unwrap_or_default(),
                disk.join().unwrap_or_default(),
                processes.join().unwrap_or_default(),
                environment.join().unwrap_or_default(),
            )
        });
    Ok(Snapshot {
        schema_version: 1,
        captured_at: Utc::now(),
        system,
        project: ProjectInfo {
            path: path.clone(),
            name,
        },
        git,
        python,
        cuda,
        build,
        disk,
        processes,
        environment,
    })
}

pub fn read(path: &Path) -> Result<Snapshot> {
    let data = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    let snapshot: Snapshot = serde_json::from_slice(&data)
        .with_context(|| format!("{} is not a valid devx snapshot", path.display()))?;
    if snapshot.schema_version != 1 {
        anyhow::bail!(
            "unsupported snapshot schema version {}",
            snapshot.schema_version
        );
    }
    Ok(snapshot)
}
