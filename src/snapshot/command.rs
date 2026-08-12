use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

pub fn read_text(path: &Path, max_bytes: u64) -> Option<String> {
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > max_bytes {
        return None;
    }
    String::from_utf8(bytes).ok()
}

pub fn output(program: &Path, args: &[&str], cwd: &Path) -> Option<String> {
    let result = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if !result.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&result.stdout).trim().to_owned();
    (!value.is_empty()).then_some(value)
}

pub fn find_executable(name: &str) -> Option<PathBuf> {
    find_executables(name).into_iter().next()
}

pub fn find_executables(name: &str) -> Vec<PathBuf> {
    let mut seen = std::collections::BTreeSet::new();
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .filter(|candidate| candidate.is_file())
        .filter(|candidate| seen.insert(candidate.clone()))
        .collect()
}
