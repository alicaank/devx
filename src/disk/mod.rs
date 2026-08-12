use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::snapshot::DiskInfo;

mod interactive;

pub use interactive::run as interactive;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilesystemEntry {
    pub name: String,
    pub mount_point: PathBuf,
    pub file_system: String,
    pub kind: String,
    pub removable: bool,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub usage_percent: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilesystemReport {
    pub filesystems: Vec<FilesystemEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMatch {
    pub path: PathBuf,
    pub bytes: u64,
    pub modified_unix_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileQueryReport {
    pub root: PathBuf,
    pub files: Vec<FileMatch>,
    pub total_bytes: u64,
    pub unreadable_entries: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateGroup {
    pub bytes_each: u64,
    pub reclaimable_bytes: u64,
    pub sha256: String,
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateReport {
    pub root: PathBuf,
    pub groups: Vec<DuplicateGroup>,
    pub reclaimable_bytes: u64,
    pub unreadable_entries: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskEntry {
    pub category: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub reason: String,
    pub regeneratable: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskReport {
    pub filesystem: DiskInfo,
    pub scanned_path: PathBuf,
    pub potentially_reclaimable_bytes: u64,
    pub safe: Vec<DiskEntry>,
    pub review: Vec<DiskEntry>,
    pub top: Vec<DiskEntry>,
    pub unreadable_entries: u64,
}

pub fn capacity(path: &Path) -> DiskInfo {
    let total = fs2::total_space(path).ok();
    let free = fs2::free_space(path).ok();
    let available = fs2::available_space(path).ok();
    let used = total
        .zip(free)
        .map(|(total, free)| total.saturating_sub(free));
    let usage_percent = used.zip(available).and_then(|(used, available)| {
        let usable = used.saturating_add(available);
        (usable > 0).then_some((used as f64 / usable as f64) * 100.0)
    });
    DiskInfo {
        path: path.to_path_buf(),
        total_bytes: total,
        used_bytes: used,
        available_bytes: available,
        usage_percent,
    }
}

pub fn filesystems() -> FilesystemReport {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut filesystems = disks
        .list()
        .iter()
        .map(|disk| {
            let total_bytes = disk.total_space();
            let available_bytes = disk.available_space();
            let used_bytes = total_bytes.saturating_sub(available_bytes);
            FilesystemEntry {
                name: disk.name().to_string_lossy().into_owned(),
                mount_point: disk.mount_point().to_path_buf(),
                file_system: disk.file_system().to_string_lossy().into_owned(),
                kind: disk.kind().to_string(),
                removable: disk.is_removable(),
                total_bytes,
                used_bytes,
                available_bytes,
                usage_percent: usage_percent(used_bytes, available_bytes),
            }
        })
        .collect::<Vec<_>>();
    filesystems.sort_by(|a, b| {
        b.usage_percent
            .total_cmp(&a.usage_percent)
            .then_with(|| a.mount_point.cmp(&b.mount_point))
    });
    FilesystemReport { filesystems }
}

pub fn query_files(
    path: &Path,
    older_than: Option<u64>,
    larger_than: Option<u64>,
) -> Result<FileQueryReport> {
    let root =
        fs::canonicalize(path).with_context(|| format!("cannot resolve {}", path.display()))?;
    let now = std::time::SystemTime::now();
    let root_device = device_id(&fs::symlink_metadata(&root)?);
    let mut unreadable_entries = 0;
    let mut files = Vec::new();
    walk_files(
        &root,
        root_device,
        &mut unreadable_entries,
        &mut |path, metadata| {
            let bytes = metadata.len();
            if larger_than.is_some_and(|minimum| bytes < minimum) {
                return;
            }
            let modified = metadata.modified().ok();
            if older_than.is_some_and(|minimum_age| {
                modified
                    .and_then(|time| now.duration_since(time).ok())
                    .is_none_or(|age| age.as_secs() < minimum_age)
            }) {
                return;
            }
            files.push(FileMatch {
                path: path.to_path_buf(),
                bytes,
                modified_unix_seconds: modified
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|value| value.as_secs()),
            });
        },
    );
    files.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    let total_bytes = files.iter().map(|file| file.bytes).sum();
    Ok(FileQueryReport {
        root,
        files,
        total_bytes,
        unreadable_entries,
    })
}

pub fn duplicates(
    path: &Path,
    older_than: Option<u64>,
    larger_than: Option<u64>,
) -> Result<DuplicateReport> {
    let root =
        fs::canonicalize(path).with_context(|| format!("cannot resolve {}", path.display()))?;
    let root_device = device_id(&fs::symlink_metadata(&root)?);
    let now = std::time::SystemTime::now();
    let mut unreadable_entries = 0;
    let mut by_size: BTreeMap<u64, Vec<PathBuf>> = BTreeMap::new();
    walk_files(
        &root,
        root_device,
        &mut unreadable_entries,
        &mut |path, metadata| {
            let bytes = metadata.len();
            if bytes == 0 || larger_than.is_some_and(|minimum| bytes < minimum) {
                return;
            }
            if older_than.is_some_and(|minimum_age| {
                metadata
                    .modified()
                    .ok()
                    .and_then(|time| now.duration_since(time).ok())
                    .is_none_or(|age| age.as_secs() < minimum_age)
            }) {
                return;
            }
            by_size.entry(bytes).or_default().push(path.to_path_buf());
        },
    );
    let mut groups = Vec::new();
    for (bytes, paths) in by_size.into_iter().filter(|(_, paths)| paths.len() > 1) {
        let mut by_hash: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
        let mut seen_identities = BTreeSet::new();
        for path in paths {
            if let Ok(metadata) = fs::metadata(&path)
                && let Some(identity) = file_identity(&metadata)
                && !seen_identities.insert(identity)
            {
                continue;
            }
            match sha256_file(&path) {
                Ok(hash) => by_hash.entry(hash).or_default().push(path),
                Err(_) => unreadable_entries += 1,
            }
        }
        for (sha256, paths) in by_hash.into_iter().filter(|(_, paths)| paths.len() > 1) {
            groups.push(DuplicateGroup {
                bytes_each: bytes,
                reclaimable_bytes: bytes.saturating_mul(paths.len().saturating_sub(1) as u64),
                sha256,
                paths,
            });
        }
    }
    groups.sort_by_key(|group| std::cmp::Reverse(group.reclaimable_bytes));
    let reclaimable_bytes = groups.iter().map(|group| group.reclaimable_bytes).sum();
    Ok(DuplicateReport {
        root,
        groups,
        reclaimable_bytes,
        unreadable_entries,
    })
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn file_identity(_: &fs::Metadata) -> Option<(u64, u64)> {
    None
}

fn walk_files(
    path: &Path,
    root_device: Option<u64>,
    unreadable: &mut u64,
    visitor: &mut impl FnMut(&Path, &fs::Metadata),
) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        *unreadable += 1;
        return;
    };
    if metadata.file_type().is_symlink() || device_id(&metadata) != root_device {
        return;
    }
    if metadata.is_file() {
        visitor(path, &metadata);
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        *unreadable += 1;
        return;
    };
    for entry in entries {
        match entry {
            Ok(entry) => walk_files(&entry.path(), root_device, unreadable, visitor),
            Err(_) => *unreadable += 1,
        }
    }
}

#[cfg(unix)]
fn device_id(metadata: &fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.dev())
}

#[cfg(not(unix))]
fn device_id(_: &fs::Metadata) -> Option<u64> {
    None
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn usage_percent(used: u64, available: u64) -> f64 {
    let usable = used.saturating_add(available);
    if usable == 0 {
        0.0
    } else {
        (used as f64 / usable as f64) * 100.0
    }
}

pub fn analyze(
    path: &Path,
    project_only: bool,
    safe_only: bool,
    top_count: Option<usize>,
) -> Result<DiskReport> {
    let path = fs::canonicalize(path)
        .with_context(|| format!("cannot resolve project path {}", path.display()))?;
    let mut unreadable = 0;
    let mut sizes = BTreeMap::new();

    let mut safe = if project_only {
        Vec::new()
    } else {
        cache_candidates()
            .into_iter()
            .filter_map(|candidate| {
                candidate.path.exists().then(|| DiskEntry {
                    category: candidate.category,
                    bytes: cached_size(&candidate.path, &mut unreadable, &mut sizes),
                    path: candidate.path,
                    reason: candidate.reason,
                    regeneratable: candidate.regeneratable,
                })
            })
            .collect::<Vec<_>>()
    };

    let mut review = if safe_only {
        Vec::new()
    } else {
        project_candidates(&path)
            .into_iter()
            .map(|candidate| DiskEntry {
                category: candidate.category,
                bytes: cached_size(&candidate.path, &mut unreadable, &mut sizes),
                path: candidate.path,
                reason: candidate.reason,
                regeneratable: candidate.regeneratable,
            })
            .collect::<Vec<_>>()
    };

    let mut top = if safe_only {
        Vec::new()
    } else if let Some(count) = top_count {
        top_entries(&path, count, &mut unreadable, &mut sizes)
    } else {
        Vec::new()
    };

    sort_largest_first(&mut safe);
    sort_largest_first(&mut review);
    sort_largest_first(&mut top);
    let potentially_reclaimable_bytes = safe.iter().chain(&review).map(|entry| entry.bytes).sum();

    Ok(DiskReport {
        filesystem: capacity(&path),
        scanned_path: path,
        potentially_reclaimable_bytes,
        safe,
        review,
        top,
        unreadable_entries: unreadable,
    })
}

struct ArtifactCandidate {
    category: String,
    path: PathBuf,
    reason: String,
    regeneratable: Option<bool>,
}

trait DiskProvider {
    fn detect(&self, project: &Path) -> Vec<ArtifactCandidate>;
}

struct CacheProvider {
    category: &'static str,
    variable: Option<&'static str>,
    fallback: &'static str,
    reason: &'static str,
    regeneratable: Option<bool>,
}

impl DiskProvider for CacheProvider {
    fn detect(&self, _: &Path) -> Vec<ArtifactCandidate> {
        let configured = self.variable.and_then(std::env::var_os).map(PathBuf::from);
        let fallback = std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(self.fallback));
        configured
            .or(fallback)
            .into_iter()
            .map(|path| ArtifactCandidate {
                category: self.category.into(),
                path,
                reason: self.reason.into(),
                regeneratable: self.regeneratable,
            })
            .collect()
    }
}

fn cache_candidates() -> Vec<ArtifactCandidate> {
    let providers = [
        CacheProvider {
            category: "pip cache",
            variable: Some("PIP_CACHE_DIR"),
            fallback: ".cache/pip",
            reason: "package download and build cache",
            regeneratable: Some(true),
        },
        CacheProvider {
            category: "uv cache",
            variable: Some("UV_CACHE_DIR"),
            fallback: ".cache/uv",
            reason: "downloaded and built Python packages",
            regeneratable: Some(true),
        },
        CacheProvider {
            category: "Hugging Face hub cache",
            variable: None,
            fallback: ".cache/huggingface/hub",
            reason: "downloaded model and dataset artifacts from the Hub",
            regeneratable: Some(true),
        },
        CacheProvider {
            category: "Hugging Face datasets cache",
            variable: None,
            fallback: ".cache/huggingface/datasets",
            reason: "prepared datasets cache",
            regeneratable: Some(true),
        },
        CacheProvider {
            category: "Hugging Face Xet cache",
            variable: None,
            fallback: ".cache/huggingface/xet",
            reason: "downloaded Xet chunks",
            regeneratable: Some(true),
        },
        CacheProvider {
            category: "Cargo registry cache",
            variable: None,
            fallback: ".cargo/registry/cache",
            reason: "downloaded crate archives",
            regeneratable: Some(true),
        },
        CacheProvider {
            category: "npm cache",
            variable: Some("NPM_CONFIG_CACHE"),
            fallback: ".npm/_cacache",
            reason: "downloaded npm package cache",
            regeneratable: Some(true),
        },
        CacheProvider {
            category: "pnpm store",
            variable: None,
            fallback: ".local/share/pnpm/store",
            reason: "content-addressed package store",
            regeneratable: Some(true),
        },
        CacheProvider {
            category: "Yarn cache",
            variable: Some("YARN_CACHE_FOLDER"),
            fallback: ".cache/yarn",
            reason: "downloaded Yarn package cache",
            regeneratable: Some(true),
        },
        CacheProvider {
            category: "Gradle cache",
            variable: None,
            fallback: ".gradle/caches",
            reason: "downloaded dependencies and build cache",
            regeneratable: Some(true),
        },
        CacheProvider {
            category: "Conda package cache",
            variable: None,
            fallback: ".conda/pkgs",
            reason: "downloaded and extracted Conda packages",
            regeneratable: None,
        },
    ];
    let mut candidates = providers
        .iter()
        .flat_map(|provider| provider.detect(Path::new(".")))
        .collect::<Vec<_>>();
    if let Some(prefix) = std::env::var_os("CONDA_PREFIX") {
        candidates.push(ArtifactCandidate {
            category: "Conda package cache".into(),
            path: PathBuf::from(prefix).join("pkgs"),
            reason: "downloaded and extracted Conda packages".into(),
            regeneratable: None,
        });
    }
    if let Some(hf_home) = std::env::var_os("HF_HOME").map(PathBuf::from) {
        for (directory, category, reason) in [
            (
                "hub",
                "Hugging Face hub cache",
                "downloaded model and dataset artifacts from the Hub",
            ),
            (
                "datasets",
                "Hugging Face datasets cache",
                "prepared datasets cache",
            ),
            ("xet", "Hugging Face Xet cache", "downloaded Xet chunks"),
        ] {
            candidates.push(ArtifactCandidate {
                category: category.into(),
                path: hf_home.join(directory),
                reason: reason.into(),
                regeneratable: Some(true),
            });
        }
    }
    let mut seen = BTreeSet::new();
    candidates.retain(|candidate| seen.insert(candidate.path.clone()));
    candidates
}

fn project_candidates(project: &Path) -> Vec<ArtifactCandidate> {
    let mut candidates = Vec::new();
    let mut add = |manifest: &str, directory: &str, category: &str, reason: &str, regeneratable| {
        let path = project.join(directory);
        if project.join(manifest).exists() && path.exists() {
            candidates.push(ArtifactCandidate {
                category: category.into(),
                path,
                reason: reason.into(),
                regeneratable,
            });
        }
    };
    add(
        "Cargo.toml",
        "target",
        "Rust build artifacts",
        "Cargo build output for this manifest",
        Some(true),
    );
    add(
        "package.json",
        "node_modules",
        "Node dependencies",
        "installed dependencies declared by package.json",
        Some(true),
    );
    add(
        "CMakeLists.txt",
        "build",
        "CMake build artifacts",
        "generated output for CMakeLists.txt",
        Some(true),
    );
    if project.join("pyproject.toml").exists() || project.join("requirements.txt").exists() {
        for directory in [".venv", "venv"] {
            let path = project.join(directory);
            if path.exists() {
                candidates.push(ArtifactCandidate {
                    category: "Python virtual environment".into(),
                    path,
                    reason: "environment associated with this Python project".into(),
                    regeneratable: None,
                });
            }
        }
    }
    let wandb = project.join("wandb");
    if wandb.join("latest-run").exists() {
        candidates.push(ArtifactCandidate {
            category: "Weights & Biases runs".into(),
            path: wandb,
            reason: "local W&B run metadata".into(),
            regeneratable: Some(false),
        });
    }
    candidates
}

fn top_entries(
    path: &Path,
    count: usize,
    unreadable: &mut u64,
    sizes: &mut BTreeMap<PathBuf, u64>,
) -> Vec<DiskEntry> {
    let Ok(entries) = fs::read_dir(path) else {
        *unreadable += 1;
        return Vec::new();
    };
    let mut output = entries
        .filter_map(|entry| match entry {
            Ok(entry) => {
                let path = entry.path();
                Some(DiskEntry {
                    category: entry.file_name().to_string_lossy().into_owned(),
                    bytes: cached_size(&path, unreadable, sizes),
                    path,
                    reason: "top-level entry; classification not inferred".into(),
                    regeneratable: None,
                })
            }
            Err(_) => {
                *unreadable += 1;
                None
            }
        })
        .collect::<Vec<_>>();
    sort_largest_first(&mut output);
    output.truncate(count);
    output
}

fn cached_size(path: &Path, unreadable: &mut u64, sizes: &mut BTreeMap<PathBuf, u64>) -> u64 {
    if let Some(size) = sizes.get(path) {
        return *size;
    }
    let size = directory_size(path, unreadable);
    sizes.insert(path.to_path_buf(), size);
    size
}

fn directory_size(root: &Path, unreadable: &mut u64) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(root) else {
        *unreadable += 1;
        return 0;
    };
    if metadata.file_type().is_symlink() {
        return 0;
    }
    if metadata.is_file() {
        return allocated_bytes(&metadata);
    }
    let Ok(entries) = fs::read_dir(root) else {
        *unreadable += 1;
        return 0;
    };
    entries
        .map(|entry| match entry {
            Ok(entry) => directory_size(&entry.path(), unreadable),
            Err(_) => {
                *unreadable += 1;
                0
            }
        })
        .sum()
}

#[cfg(unix)]
fn allocated_bytes(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated_bytes(metadata: &fs::Metadata) -> u64 {
    metadata.len()
}

fn sort_largest_first(entries: &mut [DiskEntry]) {
    entries.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    #[test]
    fn does_not_follow_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("data");
        std::fs::File::create(&file)
            .unwrap()
            .write_all(&vec![0; 8192])
            .unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&file, temp.path().join("link")).unwrap();
        let mut unreadable = 0;
        let total = super::directory_size(temp.path(), &mut unreadable);
        assert!(total >= 8192);
        assert!(total < 16384);
        assert_eq!(unreadable, 0);
    }

    #[test]
    fn project_artifacts_require_matching_manifest() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("target")).unwrap();
        assert!(super::project_candidates(temp.path()).is_empty());
        std::fs::write(temp.path().join("Cargo.toml"), "").unwrap();
        let artifacts = super::project_candidates(temp.path());
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].category, "Rust build artifacts");
    }

    #[test]
    fn calculates_filesystem_usage() {
        assert_eq!(super::usage_percent(75, 25), 75.0);
        assert_eq!(super::usage_percent(0, 0), 0.0);
    }

    #[test]
    fn queries_large_files_and_verifies_duplicates() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("a.bin"), b"same bytes").unwrap();
        std::fs::write(temp.path().join("b.bin"), b"same bytes").unwrap();
        std::fs::write(temp.path().join("small"), b"x").unwrap();
        let query = super::query_files(temp.path(), None, Some(5)).unwrap();
        assert_eq!(query.files.len(), 2);
        let duplicate = super::duplicates(temp.path(), None, Some(5)).unwrap();
        assert_eq!(duplicate.groups.len(), 1);
        assert_eq!(duplicate.groups[0].paths.len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn duplicate_report_does_not_treat_hard_links_as_reclaimable() {
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("original");
        std::fs::write(&original, b"same bytes").unwrap();
        std::fs::hard_link(&original, temp.path().join("link")).unwrap();
        assert!(
            super::duplicates(temp.path(), None, None)
                .unwrap()
                .groups
                .is_empty()
        );
    }
}
