use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::snapshot::Snapshot;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Difference {
    pub field: String,
    pub left: Option<String>,
    pub right: Option<String>,
    pub domain: String,
    pub relevance: Relevance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relevance {
    LikelyRelevant,
    Informational,
}

#[derive(Debug, Clone, Default)]
pub struct DiffOptions {
    pub only: Vec<String>,
    pub ignore_host: bool,
    pub project_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffReport {
    pub differences: Vec<Difference>,
}

impl DiffReport {
    pub fn has_relevant_differences(&self) -> bool {
        self.differences
            .iter()
            .any(|difference| difference.relevance == Relevance::LikelyRelevant)
    }
}

pub fn compare_with(left: &Snapshot, right: &Snapshot, options: DiffOptions) -> DiffReport {
    let a = fields(left);
    let b = fields(right);
    let keys = a.keys().chain(b.keys()).cloned().collect::<BTreeSet<_>>();
    let differences = keys
        .into_iter()
        .filter(|field| include_field(field, &options))
        .filter_map(|field| {
            let left = a.get(&field).cloned();
            let right = b.get(&field).cloned();
            let domain = domain(&field).to_owned();
            let relevance = relevance(&field);
            (left != right).then_some(Difference {
                field,
                left,
                right,
                domain,
                relevance,
            })
        })
        .collect();
    DiffReport { differences }
}

fn domain(field: &str) -> &str {
    let prefix = field.split('.').next().unwrap_or("");
    if prefix.eq_ignore_ascii_case("PyTorch") || prefix.eq_ignore_ascii_case("Python") {
        "python"
    } else if prefix.eq_ignore_ascii_case("Environment") {
        "environment"
    } else if prefix.eq_ignore_ascii_case("CUDA") {
        "cuda"
    } else if prefix.eq_ignore_ascii_case("Build") {
        "build"
    } else if prefix.eq_ignore_ascii_case("Git") {
        "git"
    } else if prefix.eq_ignore_ascii_case("System") {
        "system"
    } else if prefix.eq_ignore_ascii_case("Disk") {
        "disk"
    } else if prefix.eq_ignore_ascii_case("Project") {
        "project"
    } else {
        "other"
    }
}

fn relevance(field: &str) -> Relevance {
    if matches!(
        field,
        "System.hostname" | "System.kernel" | "Disk.total_bytes" | "Project.path"
    ) {
        Relevance::Informational
    } else {
        Relevance::LikelyRelevant
    }
}

fn include_field(field: &str, options: &DiffOptions) -> bool {
    if options.ignore_host && matches!(field, "System.hostname" | "System.kernel") {
        return false;
    }
    let field_domain = domain(field);
    if options.project_only {
        return matches!(
            field_domain,
            "project" | "git" | "python" | "cuda" | "build" | "environment"
        );
    }
    options.only.is_empty()
        || options
            .only
            .iter()
            .any(|value| value.eq_ignore_ascii_case(field_domain))
}

pub(crate) fn fields(s: &Snapshot) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    put(&mut out, "System.OS", s.system.os.as_deref());
    put(&mut out, "System.hostname", s.system.hostname.as_deref());
    put(
        &mut out,
        "System.architecture",
        Some(&s.system.architecture),
    );
    put(&mut out, "System.kernel", s.system.kernel.as_deref());
    put(&mut out, "Python.version", s.python.version.as_deref());
    put(
        &mut out,
        "Python.pip_version",
        s.python.pip_version.as_deref(),
    );
    put_path(
        &mut out,
        "Python.executable",
        s.python.executable.as_deref(),
    );
    put(
        &mut out,
        "PyTorch.version",
        s.python.torch_version.as_deref(),
    );
    put(&mut out, "PyTorch.CUDA", s.python.torch_cuda.as_deref());
    put(&mut out, "CUDA.nvcc", s.cuda.nvcc_version.as_deref());
    put(&mut out, "CUDA.driver", s.cuda.driver_version.as_deref());
    put(
        &mut out,
        "CUDA.driver_capability",
        s.cuda.driver_cuda.as_deref(),
    );
    put_path(&mut out, "CUDA.home", s.cuda.cuda_home.as_deref());
    put_path(&mut out, "Project.path", Some(&s.project.path));
    for (name, tool) in [
        ("gcc", &s.build.gcc),
        ("g++", &s.build.gxx),
        ("clang", &s.build.clang),
        ("clang++", &s.build.clangxx),
        ("cmake", &s.build.cmake),
        ("ninja", &s.build.ninja),
    ] {
        put(&mut out, &format!("Build.{name}"), tool.version.as_deref());
    }
    if let Some(total) = s.disk.total_bytes {
        out.insert("Disk.total_bytes".into(), total.to_string());
    }
    put(&mut out, "Git.commit", s.git.commit.as_deref());
    put(&mut out, "Git.branch", s.git.branch.as_deref());
    if let Some(dirty) = s.git.dirty {
        out.insert("Git.dirty".into(), dirty.to_string());
    }
    for (name, value) in &s.environment.variables {
        out.insert(format!("Environment.{name}"), value.clone());
    }
    out
}

fn put(map: &mut BTreeMap<String, String>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        map.insert(key.into(), value.into());
    }
}

fn put_path(map: &mut BTreeMap<String, String>, key: &str, value: Option<&std::path::Path>) {
    if let Some(value) = value {
        map.insert(key.into(), value.display().to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_capture_time_and_reports_version() {
        let a = Snapshot::default();
        let mut b = a.clone();
        b.python.version = Some("3.12.1".into());
        let report = compare_with(&a, &b, DiffOptions::default());
        assert_eq!(report.differences.len(), 1);
        assert_eq!(report.differences[0].field, "Python.version");
    }

    #[test]
    fn filters_domains_and_host_noise() {
        let mut a = Snapshot::default();
        let mut b = Snapshot::default();
        a.python.version = Some("3.12".into());
        b.python.version = Some("3.11".into());
        a.system.hostname = Some("laptop".into());
        b.system.hostname = Some("server".into());
        let report = compare_with(
            &a,
            &b,
            DiffOptions {
                only: vec!["python".into()],
                ..Default::default()
            },
        );
        assert_eq!(report.differences.len(), 1);
        assert_eq!(report.differences[0].domain, "python");
        assert_eq!(report.differences[0].relevance, Relevance::LikelyRelevant);
    }
}
