use std::collections::BTreeMap;

use super::{EnvironmentInfo, EnvironmentSource};

const CAPTURED: &[&str] = &[
    "CC",
    "CXX",
    "CUDA_HOME",
    "CUDA_PATH",
    "CONDA_DEFAULT_ENV",
    "CONDA_PREFIX",
    "CUDNN_PATH",
    "HF_HOME",
    "LD_LIBRARY_PATH",
    "PYTHONPATH",
    "TORCH_HOME",
    "VIRTUAL_ENV",
    "WAN_CKPT",
];

pub fn scan() -> EnvironmentInfo {
    let variables = CAPTURED
        .iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| ((*name).to_owned(), value))
        })
        .collect::<BTreeMap<_, _>>();
    let path_entries = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    let provenance = provenance(&variables);
    EnvironmentInfo {
        path_entries,
        variables,
        provenance,
    }
}

fn provenance(variables: &BTreeMap<String, String>) -> BTreeMap<String, EnvironmentSource> {
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return BTreeMap::new();
    };
    let files = [
        home.join(".profile"),
        home.join(".bash_profile"),
        home.join(".bashrc"),
        home.join(".zprofile"),
        home.join(".zshrc"),
        home.join(".config/fish/config.fish"),
    ];
    let mut sources = BTreeMap::new();
    for file in files {
        let Some(contents) = super::command::read_text(&file, 1024 * 1024) else {
            continue;
        };
        for (index, line) in contents.lines().enumerate() {
            let trimmed = line
                .trim_start()
                .strip_prefix("export ")
                .unwrap_or(line.trim_start());
            for name in variables.keys() {
                if trimmed.starts_with(name) && trimmed[name.len()..].trim_start().starts_with('=')
                {
                    sources.insert(
                        name.clone(),
                        EnvironmentSource {
                            file: file.clone(),
                            line: index + 1,
                            confidence: "heuristic".into(),
                        },
                    );
                }
            }
        }
    }
    sources
}
