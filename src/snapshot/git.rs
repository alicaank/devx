use std::path::Path;

use super::{GitInfo, command};

pub fn scan(cwd: &Path) -> GitInfo {
    let Some(git) = command::find_executable("git") else {
        return GitInfo::default();
    };
    let root = command::output(&git, &["rev-parse", "--show-toplevel"], cwd).map(Into::into);
    if root.is_none() {
        return GitInfo::default();
    }
    let commit = command::output(&git, &["rev-parse", "--short=12", "HEAD"], cwd);
    let branch = command::output(&git, &["branch", "--show-current"], cwd);
    let detached = Some(branch.is_none());
    let dirty = std::process::Command::new(&git)
        .args(["status", "--porcelain"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !o.stdout.is_empty());
    let submodules_dirty =
        command::output(&git, &["submodule", "status", "--recursive"], cwd).map(|status| {
            status
                .lines()
                .any(|line| matches!(line.as_bytes().first(), Some(b'-' | b'+' | b'U')))
        });
    GitInfo {
        root,
        commit,
        branch,
        dirty,
        detached,
        submodules_dirty,
    }
}
