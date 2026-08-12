use std::path::Path;

use super::{BuildInfo, ToolInfo, command};

pub fn scan(cwd: &Path) -> BuildInfo {
    BuildInfo {
        gcc: tool("gcc", &["--version"], cwd),
        gxx: tool("g++", &["--version"], cwd),
        clang: tool("clang", &["--version"], cwd),
        clangxx: tool("clang++", &["--version"], cwd),
        cmake: tool("cmake", &["--version"], cwd),
        ninja: tool("ninja", &["--version"], cwd),
        rustc: tool("rustc", &["--version"], cwd),
        node: tool("node", &["--version"], cwd),
    }
}

fn tool(name: &str, args: &[&str], cwd: &Path) -> ToolInfo {
    let candidates = command::find_executables(name);
    let path = candidates.first().cloned();
    let version = path
        .as_deref()
        .and_then(|path| command::output(path, args, cwd))
        .and_then(|output| output.lines().next().map(str::to_owned));
    ToolInfo {
        path,
        version,
        candidates,
    }
}
