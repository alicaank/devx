use std::path::Path;

use super::{SystemInfo, command};

pub fn scan() -> SystemInfo {
    let root = Path::new("/");
    SystemInfo {
        os: os_name(),
        architecture: std::env::consts::ARCH.to_owned(),
        hostname: command::find_executable("hostname").and_then(|p| command::output(&p, &[], root)),
        kernel: command::find_executable("uname").and_then(|p| command::output(&p, &["-sr"], root)),
    }
}

fn os_name() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let data = command::read_text(Path::new("/etc/os-release"), 64 * 1024)?;
        for line in data.lines() {
            if let Some(value) = line.strip_prefix("PRETTY_NAME=") {
                return Some(value.trim_matches('"').to_owned());
            }
        }
    }
    Some(std::env::consts::OS.to_owned())
}
