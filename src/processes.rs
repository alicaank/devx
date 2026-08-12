use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
};

use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

mod watch;

pub use watch::run as watch;

#[derive(Debug, Clone, Default)]
pub struct ProcessFilter {
    pub project: Option<String>,
    pub ports_only: bool,
    pub gpu_only: bool,
    pub include_unclassified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub name: String,
    pub cwd: PathBuf,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    pub runtime_seconds: u64,
    pub ports: Vec<u16>,
    pub gpu_memory_bytes: Option<u64>,
    #[serde(default)]
    pub attribution_confidence: u8,
    #[serde(default)]
    pub attribution_evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectProcesses {
    pub project: String,
    pub root: PathBuf,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    pub gpu_memory_bytes: u64,
    pub ports: Vec<u16>,
    pub processes: Vec<ProcessInfo>,
    #[serde(default)]
    pub attribution_confidence: u8,
    #[serde(default)]
    pub attribution_evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessReport {
    pub projects: Vec<ProjectProcesses>,
    pub process_count: usize,
    pub port_inspection_available: bool,
    pub gpu_inspection_available: bool,
    pub unclassified_process_count: usize,
}

pub fn scan(filter: ProcessFilter) -> ProcessReport {
    let mut system = System::new();
    let refresh = ProcessRefreshKind::everything().without_tasks();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh);
    thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh);

    let current_uid = sysinfo::get_current_pid()
        .ok()
        .and_then(|pid| system.process(pid))
        .and_then(|process| process.user_id())
        .cloned();
    let candidate_pids = system
        .processes()
        .iter()
        .filter(|(_, process)| {
            current_uid
                .as_ref()
                .is_none_or(|uid| process.user_id() == Some(uid))
                && process.cwd().is_some_and(|cwd| cwd != Path::new("/"))
        })
        .map(|(pid, _)| pid.as_u32())
        .collect::<BTreeSet<_>>();

    let (ports_by_pid, port_inspection_available) = listening_ports(&candidate_pids);
    let (gpu_by_pid, gpu_inspection_available) = gpu_memory();
    let mut groups: BTreeMap<PathBuf, Vec<ProcessInfo>> = BTreeMap::new();
    let direct_roots = system
        .processes()
        .iter()
        .filter_map(|(pid, process)| {
            process
                .cwd()
                .map(|cwd| (pid.as_u32(), marked_project_root(cwd)))
        })
        .collect::<BTreeMap<_, _>>();
    let parents = system
        .processes()
        .iter()
        .map(|(pid, process)| (pid.as_u32(), process.parent().map(Pid::as_u32)))
        .collect::<BTreeMap<_, _>>();
    let mut unclassified_process_count = 0;

    for (pid, process) in system.processes() {
        let pid = pid.as_u32();
        if !candidate_pids.contains(&pid) {
            continue;
        }
        let Some(cwd) = process.cwd() else { continue };
        let attribution = process_attribution(pid, cwd, &direct_roots, &parents).or_else(|| {
            filter.include_unclassified.then(|| {
                (
                    unclassified_root(cwd),
                    25,
                    format!("cwd {} (no project marker)", cwd.display()),
                )
            })
        });
        let Some((root, attribution_confidence, attribution_evidence)) = attribution else {
            unclassified_process_count += 1;
            continue;
        };
        let ports = ports_by_pid.get(&pid).cloned().unwrap_or_default();
        let gpu = gpu_by_pid.get(&pid).copied();
        if filter.ports_only && ports.is_empty() {
            continue;
        }
        if filter.gpu_only && gpu.is_none() {
            continue;
        }
        groups.entry(root).or_default().push(ProcessInfo {
            pid,
            parent_pid: process.parent().map(Pid::as_u32),
            name: useful_name(process.name(), process.cmd()),
            cwd: cwd.to_path_buf(),
            cpu_percent: process.cpu_usage(),
            memory_bytes: process.memory(),
            runtime_seconds: process.run_time(),
            ports,
            gpu_memory_bytes: gpu,
            attribution_confidence,
            attribution_evidence,
        });
    }

    let project_filter = filter.project.as_deref().map(str::to_lowercase);
    let mut projects = groups
        .into_iter()
        .filter_map(|(root, mut processes)| {
            let project = root
                .file_name()
                .unwrap_or_else(|| root.as_os_str())
                .to_string_lossy()
                .into_owned();
            if let Some(needle) = &project_filter
                && !project.to_lowercase().contains(needle)
                && !root.to_string_lossy().to_lowercase().contains(needle)
            {
                return None;
            }
            processes.sort_by(|a, b| {
                b.cpu_percent
                    .total_cmp(&a.cpu_percent)
                    .then_with(|| b.memory_bytes.cmp(&a.memory_bytes))
                    .then_with(|| a.pid.cmp(&b.pid))
            });
            let cpu_percent = processes.iter().map(|p| p.cpu_percent).sum();
            let memory_bytes = processes.iter().map(|p| p.memory_bytes).sum();
            let gpu_memory_bytes = processes.iter().filter_map(|p| p.gpu_memory_bytes).sum();
            let ports = processes
                .iter()
                .flat_map(|p| p.ports.iter().copied())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let attribution_confidence = processes
                .iter()
                .map(|process| process.attribution_confidence)
                .min()
                .unwrap_or(0);
            let attribution_evidence = processes
                .iter()
                .map(|process| process.attribution_evidence.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            Some(ProjectProcesses {
                project,
                root,
                cpu_percent,
                memory_bytes,
                gpu_memory_bytes,
                ports,
                processes,
                attribution_confidence,
                attribution_evidence,
            })
        })
        .collect::<Vec<_>>();
    projects.sort_by(|a, b| {
        b.cpu_percent
            .total_cmp(&a.cpu_percent)
            .then_with(|| b.memory_bytes.cmp(&a.memory_bytes))
            .then_with(|| a.project.cmp(&b.project))
    });
    let process_count = projects.iter().map(|group| group.processes.len()).sum();
    ProcessReport {
        projects,
        process_count,
        port_inspection_available,
        gpu_inspection_available,
        unclassified_process_count,
    }
}

fn marked_project_root(cwd: &Path) -> Option<PathBuf> {
    const MARKERS: &[&str] = &[
        ".git",
        "Cargo.toml",
        "pyproject.toml",
        "package.json",
        "go.mod",
        "pom.xml",
    ];
    for ancestor in cwd.ancestors() {
        if MARKERS.iter().any(|marker| ancestor.join(marker).exists()) {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

fn inherited_project_root(
    pid: u32,
    direct_roots: &BTreeMap<u32, Option<PathBuf>>,
    parents: &BTreeMap<u32, Option<u32>>,
) -> Option<PathBuf> {
    let mut current = Some(pid);
    let mut visited = BTreeSet::new();
    while let Some(pid) = current {
        if !visited.insert(pid) {
            return None;
        }
        if let Some(Some(root)) = direct_roots.get(&pid) {
            return Some(root.clone());
        }
        current = parents.get(&pid).copied().flatten();
    }
    None
}

fn process_attribution(
    pid: u32,
    cwd: &Path,
    direct_roots: &BTreeMap<u32, Option<PathBuf>>,
    parents: &BTreeMap<u32, Option<u32>>,
) -> Option<(PathBuf, u8, String)> {
    if let Some(Some(root)) = direct_roots.get(&pid) {
        return Some((
            root.clone(),
            100,
            format!(
                "cwd {} → project marker at {}",
                cwd.display(),
                root.display()
            ),
        ));
    }
    inherited_project_root(pid, direct_roots, parents).map(|root| {
        (
            root.clone(),
            80,
            format!("parent process → project marker at {}", root.display()),
        )
    })
}

fn unclassified_root(cwd: &Path) -> PathBuf {
    cwd.to_path_buf()
}

fn useful_name(executable: &OsStr, command: &[std::ffi::OsString]) -> String {
    let base = executable.to_string_lossy();
    let lower = base.to_lowercase();
    if lower.starts_with("python") {
        if let Some(index) = command.iter().position(|arg| arg == "-m")
            && let Some(module) = command.get(index + 1)
        {
            return format!("python -m {}", module.to_string_lossy());
        }
        if let Some(script) = command.iter().skip(1).find(|arg| {
            let path = Path::new(arg);
            path.extension().is_some_and(|ext| ext == "py")
        }) {
            return Path::new(script)
                .file_name()
                .unwrap_or(script)
                .to_string_lossy()
                .into_owned();
        }
    }
    if matches!(lower.as_str(), "node" | "deno" | "bun")
        && let Some(script) = command.iter().skip(1).find(|arg| {
            Path::new(arg).extension().is_some_and(|ext| {
                matches!(ext.to_str(), Some("js" | "mjs" | "cjs" | "ts" | "tsx"))
            })
        })
    {
        return Path::new(script)
            .file_name()
            .unwrap_or(script)
            .to_string_lossy()
            .into_owned();
    }
    base.into_owned()
}

fn gpu_memory() -> (BTreeMap<u32, u64>, bool) {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-compute-apps=pid,used_memory",
            "--format=csv,noheader,nounits",
        ])
        .output();
    let Ok(output) = output else {
        return (BTreeMap::new(), false);
    };
    if !output.status.success() {
        return (BTreeMap::new(), false);
    }
    let mut processes = BTreeMap::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split(',').map(str::trim);
        let (Some(pid), Some(mebibytes)) = (fields.next(), fields.next()) else {
            continue;
        };
        if let (Ok(pid), Ok(mebibytes)) = (pid.parse(), mebibytes.parse::<u64>()) {
            processes.insert(pid, mebibytes.saturating_mul(1024 * 1024));
        }
    }
    (processes, true)
}

#[cfg(target_os = "linux")]
fn listening_ports(pids: &BTreeSet<u32>) -> (BTreeMap<u32, Vec<u16>>, bool) {
    let mut sockets = BTreeMap::new();
    for (path, listen_state) in [
        ("/proc/net/tcp", Some("0A")),
        ("/proc/net/tcp6", Some("0A")),
        ("/proc/net/udp", None),
        ("/proc/net/udp6", None),
    ] {
        let Ok(data) = fs::read_to_string(path) else {
            continue;
        };
        for line in data.lines().skip(1) {
            if let Some((inode, port)) = parse_socket_line(line, listen_state) {
                sockets.insert(inode, port);
            }
        }
    }
    let available = !sockets.is_empty() || Path::new("/proc/net/tcp").exists();
    let mut by_pid = BTreeMap::new();
    for pid in pids {
        let Ok(entries) = fs::read_dir(format!("/proc/{pid}/fd")) else {
            continue;
        };
        let mut ports = BTreeSet::new();
        for entry in entries.flatten() {
            let Ok(target) = fs::read_link(entry.path()) else {
                continue;
            };
            let target = target.to_string_lossy();
            let Some(inode) = target
                .strip_prefix("socket:[")
                .and_then(|v| v.strip_suffix(']'))
                .and_then(|v| v.parse::<u64>().ok())
            else {
                continue;
            };
            if let Some(port) = sockets.get(&inode) {
                ports.insert(*port);
            }
        }
        if !ports.is_empty() {
            by_pid.insert(*pid, ports.into_iter().collect());
        }
    }
    (by_pid, available)
}

#[cfg(target_os = "linux")]
fn parse_socket_line(line: &str, listen_state: Option<&str>) -> Option<(u64, u16)> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 10 || listen_state.is_some_and(|state| fields[3] != state) {
        return None;
    }
    let port_hex = fields[1].rsplit(':').next()?;
    let port = u16::from_str_radix(port_hex, 16).ok()?;
    let inode = fields[9].parse().ok()?;
    (port != 0).then_some((inode, port))
}

#[cfg(not(target_os = "linux"))]
fn listening_ports(_: &BTreeSet<u32>) -> (BTreeMap<u32, Vec<u16>>, bool) {
    (BTreeMap::new(), false)
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_nearest_project_root() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("src/deep");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(temp.path().join("Cargo.toml"), "").unwrap();
        assert_eq!(
            super::marked_project_root(&nested).as_deref(),
            Some(temp.path())
        );
    }

    #[test]
    fn directory_without_marker_is_not_a_project() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(super::marked_project_root(temp.path()), None);
    }

    #[test]
    fn inherits_project_from_parent_process() {
        let mut roots = std::collections::BTreeMap::new();
        roots.insert(10, Some("/work/project".into()));
        roots.insert(11, None);
        let mut parents = std::collections::BTreeMap::new();
        parents.insert(10, None);
        parents.insert(11, Some(10));
        assert_eq!(
            super::inherited_project_root(11, &roots, &parents),
            Some("/work/project".into())
        );
    }

    #[test]
    fn extracts_python_script_without_arguments() {
        let command = [
            "python".into(),
            "/work/train.py".into(),
            "--token=secret".into(),
        ];
        assert_eq!(super::useful_name("python".as_ref(), &command), "train.py");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_listening_tcp_socket() {
        let line = "0: 0100007F:1F40 00000000:0000 0A 00000000:00000000 00:00000000 00000000 1000 0 424242 1";
        assert_eq!(
            super::parse_socket_line(line, Some("0A")),
            Some((424242, 8000))
        );
        assert_eq!(super::parse_socket_line(line, Some("01")), None);
    }
}
