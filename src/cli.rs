use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "devx", version, about = "Explain your development machine")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Capture a normalized, portable description of this environment
    Snapshot(SnapshotArgs),
    /// Diagnose suspicious settings in the current environment
    Doctor(DoctorArgs),
    /// Analyze storage and optionally manage files interactively
    Disk(DiskArgs),
    /// Group running processes by development project
    Ps(PsArgs),
    /// Compare two snapshots
    Diff(DiffArgs),
}

#[derive(Debug, Args)]
pub struct PsArgs {
    /// Filter by project name or path
    pub project: Option<String>,
    /// Show only processes with listening ports
    #[arg(long)]
    pub ports: bool,
    /// Show only NVIDIA GPU compute processes
    #[arg(long)]
    pub gpu: bool,
    /// Show parent/child process relationships and PIDs
    #[arg(long)]
    pub tree: bool,
    /// Refresh continuously until q, Esc, or Ctrl-C is pressed
    #[arg(short = 'w', long, conflicts_with = "json")]
    pub watch: bool,
    /// Refresh interval in seconds
    #[arg(long, default_value_t = 2.0, requires = "watch", value_parser = parse_watch_interval)]
    pub interval: f64,
    /// Include processes whose working directory has no project marker
    #[arg(long)]
    pub all: bool,
    /// Emit the report as JSON
    #[arg(long)]
    pub json: bool,
    /// Write the process report to a JSON file
    #[arg(long, value_name = "FILE", conflicts_with_all = ["json", "watch"])]
    pub snapshot: Option<PathBuf>,
    /// Emit a CSV report
    #[arg(long, conflicts_with_all = ["json", "watch", "snapshot"])]
    pub csv: bool,
}

fn parse_watch_interval(value: &str) -> Result<f64, String> {
    let seconds = value
        .parse::<f64>()
        .map_err(|_| "interval must be a number".to_owned())?;
    if seconds.is_finite() && (0.5..=60.0).contains(&seconds) {
        Ok(seconds)
    } else {
        Err("interval must be between 0.5 and 60 seconds".into())
    }
}

#[derive(Debug, Args)]
pub struct DiskArgs {
    /// Project directory to inspect
    #[arg(long, short = 'C', default_value = ".")]
    pub path: PathBuf,
    /// Browse storage interactively with confirmed move-to-trash support
    #[arg(short = 'i', long, conflicts_with_all = ["filesystems", "project", "safe", "top", "duplicates", "older_than", "larger_than", "json", "csv"])]
    pub interactive: bool,
    /// Report capacity and usage for every detected mounted filesystem
    #[arg(long, visible_alias = "all-filesystems", conflicts_with_all = ["project", "safe", "top", "duplicates", "older_than", "larger_than"])]
    pub filesystems: bool,
    /// Restrict analysis to the selected project
    #[arg(long, conflicts_with = "safe")]
    pub project: bool,
    /// Show only known cache locations
    #[arg(long)]
    pub safe: bool,
    /// Include the largest entries in the selected project (default: 20)
    #[arg(long, value_name = "N", num_args = 0..=1, default_missing_value = "20")]
    pub top: Option<usize>,
    /// Find duplicate files using size grouping and SHA-256 verification
    #[arg(long, conflicts_with_all = ["interactive", "filesystems", "project", "safe", "top"])]
    pub duplicates: bool,
    /// Include files at least this old (for example 30d, 12h)
    #[arg(long, value_name = "AGE", value_parser = parse_duration)]
    pub older_than: Option<u64>,
    /// Include files at least this large (for example 500MB, 2GB)
    #[arg(long, value_name = "SIZE", value_parser = parse_bytes)]
    pub larger_than: Option<u64>,
    /// Emit the report as JSON
    #[arg(long)]
    pub json: bool,
    /// Emit a CSV report
    #[arg(long, conflicts_with = "json")]
    pub csv: bool,
}

#[derive(Debug, Args)]
pub struct SnapshotArgs {
    /// Project directory to inspect
    #[arg(long, short = 'C', default_value = ".")]
    pub path: PathBuf,
    /// Emit compact JSON
    #[arg(long)]
    pub compact: bool,
    /// Capture only portable system, project, Git, Python, CUDA, and build fields
    #[arg(long)]
    pub minimal: bool,
    /// Replace home/project path prefixes with portable placeholders
    #[arg(long)]
    pub redact_paths: bool,
    /// Write the snapshot directly to a file
    #[arg(long, short = 'o', value_name = "FILE")]
    pub output: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Project directory to inspect
    #[arg(long, short = 'C', default_value = ".")]
    pub path: PathBuf,
    /// Include evidence, impact, and suggested actions
    #[arg(long)]
    pub explain: bool,
    /// Emit the diagnostic report as JSON
    #[arg(long)]
    pub json: bool,
    /// Exit with status 1 when warnings or errors are found
    #[arg(long)]
    pub strict: bool,
    /// Emit a Markdown report suitable for an issue or CI artifact
    #[arg(long, conflicts_with = "json")]
    pub markdown: bool,
}

fn parse_bytes(value: &str) -> Result<u64, String> {
    let split = value
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(value.len());
    let number = value[..split]
        .parse::<f64>()
        .map_err(|_| "invalid size".to_owned())?;
    let unit = value[split..].trim().to_ascii_lowercase();
    let multiplier = match unit.as_str() {
        "" | "b" => 1.0,
        "k" | "kb" | "kib" => 1024.0,
        "m" | "mb" | "mib" => 1024.0_f64.powi(2),
        "g" | "gb" | "gib" => 1024.0_f64.powi(3),
        "t" | "tb" | "tib" => 1024.0_f64.powi(4),
        _ => return Err("size unit must be B, KB, MB, GB, or TB".into()),
    };
    let bytes = number * multiplier;
    if !bytes.is_finite() || bytes < 0.0 || bytes > u64::MAX as f64 {
        Err("size is out of range".into())
    } else {
        Ok(bytes as u64)
    }
}

fn parse_duration(value: &str) -> Result<u64, String> {
    let split = value
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(value.len());
    let number = value[..split]
        .parse::<f64>()
        .map_err(|_| "invalid duration".to_owned())?;
    let multiplier = match value[split..].trim().to_ascii_lowercase().as_str() {
        "s" => 1.0,
        "m" | "min" => 60.0,
        "h" => 3600.0,
        "d" => 86_400.0,
        "w" => 604_800.0,
        _ => return Err("duration unit must be s, m, h, d, or w".into()),
    };
    let seconds = number * multiplier;
    if !seconds.is_finite() || seconds < 0.0 || seconds > u64::MAX as f64 {
        Err("duration is out of range".into())
    } else {
        Ok(seconds as u64)
    }
}

#[derive(Debug, Args)]
pub struct DiffArgs {
    pub left: PathBuf,
    pub right: PathBuf,
    /// Compare only comma-separated domains (python,cuda,build,git,environment,system,disk)
    #[arg(long, value_delimiter = ',')]
    pub only: Vec<String>,
    /// Ignore hostname and kernel differences
    #[arg(long)]
    pub ignore_host: bool,
    /// Compare only project, Git, and development environment fields
    #[arg(long, conflicts_with = "only")]
    pub project: bool,
    /// Emit the diff report as JSON
    #[arg(long)]
    pub json: bool,
    /// Exit with status 1 when likely relevant differences are found
    #[arg(long)]
    pub strict: bool,
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_human_sizes() {
        assert_eq!(super::parse_bytes("1.5GB").unwrap(), 1_610_612_736);
        assert_eq!(super::parse_bytes("512MB").unwrap(), 536_870_912);
        assert!(super::parse_bytes("12watts").is_err());
    }

    #[test]
    fn parses_human_durations() {
        assert_eq!(super::parse_duration("2d").unwrap(), 172_800);
        assert_eq!(super::parse_duration("1.5h").unwrap(), 5_400);
        assert!(super::parse_duration("10").is_err());
    }
}
