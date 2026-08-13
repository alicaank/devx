use std::io::{self, Write};

use anyhow::Result;
use serde::Serialize;

use crate::{
    diff::{DiffReport, fields},
    disk::{DiskEntry, DiskReport, DuplicateReport, FileQueryReport, FilesystemReport},
    processes::{ProcessInfo, ProcessReport, ProjectProcesses},
    snapshot::Snapshot,
};

pub fn write_json_file<T: Serialize>(
    path: &std::path::Path,
    value: &T,
    compact: bool,
) -> Result<()> {
    let file = std::fs::File::create(path)?;
    if compact {
        serde_json::to_writer(file, value)?;
    } else {
        serde_json::to_writer_pretty(file, value)?;
    }
    Ok(())
}

pub fn print_file_query(report: &FileQueryReport) {
    println!("Files under {}", safe(&report.root.display().to_string()));
    println!();
    for file in &report.files {
        println!(
            "{:>11}  {}",
            format_bytes(file.bytes),
            safe(&file.path.display().to_string())
        );
    }
    println!();
    println!(
        "{} files · {} total",
        report.files.len(),
        format_bytes(report.total_bytes)
    );
    if report.unreadable_entries > 0 {
        println!("⚠ {} unreadable entries", report.unreadable_entries);
    }
}

pub fn print_file_query_csv(report: &FileQueryReport) {
    println!("bytes,modified_unix_seconds,path");
    for file in &report.files {
        println!(
            "{},{},{}",
            file.bytes,
            file.modified_unix_seconds
                .map(|v| v.to_string())
                .unwrap_or_default(),
            csv(&file.path.display().to_string())
        );
    }
}

pub fn print_duplicates(report: &DuplicateReport) {
    println!(
        "Duplicate files under {}",
        safe(&report.root.display().to_string())
    );
    for (index, group) in report.groups.iter().enumerate() {
        println!();
        println!(
            "{}. {} each · {} reclaimable",
            index + 1,
            format_bytes(group.bytes_each),
            format_bytes(group.reclaimable_bytes)
        );
        for path in &group.paths {
            println!("   {}", safe(&path.display().to_string()));
        }
    }
    println!();
    println!(
        "{} groups · {} potentially reclaimable",
        report.groups.len(),
        format_bytes(report.reclaimable_bytes)
    );
}

pub fn print_duplicates_csv(report: &DuplicateReport) {
    println!("group,bytes_each,sha256,path");
    for (index, group) in report.groups.iter().enumerate() {
        for path in &group.paths {
            println!(
                "{},{},{},{}",
                index + 1,
                group.bytes_each,
                group.sha256,
                csv(&path.display().to_string())
            );
        }
    }
}

pub fn print_processes_csv(report: &ProcessReport) {
    println!(
        "project,root,pid,parent_pid,name,cpu_percent,memory_bytes,gpu_memory_bytes,ports,cwd"
    );
    for group in &report.projects {
        for process in &group.processes {
            println!(
                "{},{},{},{},{},{:.2},{},{},{},{}",
                csv(&group.project),
                csv(&group.root.display().to_string()),
                process.pid,
                process
                    .parent_pid
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                csv(&process.name),
                process.cpu_percent,
                process.memory_bytes,
                process
                    .gpu_memory_bytes
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                csv(&join_ports(&process.ports)),
                csv(&process.cwd.display().to_string())
            );
        }
    }
}

fn csv(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn safe(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                '�'
            } else {
                character
            }
        })
        .collect()
}

pub fn print_filesystems(report: &FilesystemReport) {
    if report.filesystems.is_empty() {
        println!("No mounted storage filesystems detected.");
        return;
    }
    println!(
        "{:<24} {:<10} {:>11} {:>11} {:>11} {:>7}  DEVICE",
        "MOUNT", "TYPE", "USED", "AVAILABLE", "TOTAL", "USE%"
    );
    println!("{}", "─".repeat(100));
    for filesystem in &report.filesystems {
        let device = if filesystem.name.is_empty() {
            "-"
        } else {
            &filesystem.name
        };
        println!(
            "{:<24} {:<10} {:>11} {:>11} {:>11} {:>6.1}%  {}{}",
            truncate(&filesystem.mount_point.display().to_string(), 23),
            truncate(&filesystem.file_system, 9),
            format_bytes(filesystem.used_bytes),
            format_bytes(filesystem.available_bytes),
            format_bytes(filesystem.total_bytes),
            filesystem.usage_percent,
            safe(device),
            if filesystem.removable {
                " (removable)"
            } else {
                ""
            }
        );
    }
    println!();
    println!(
        "{} mounted storage filesystem{}",
        report.filesystems.len(),
        if report.filesystems.len() == 1 {
            ""
        } else {
            "s"
        }
    );
    println!("Capacity report only; no recursive scan and no files were modified.");
}

pub fn print_filesystems_csv(report: &FilesystemReport) {
    println!(
        "mount,type,kind,device,removable,used_bytes,available_bytes,total_bytes,usage_percent"
    );
    for item in &report.filesystems {
        println!(
            "{},{},{},{},{},{},{},{},{:.2}",
            csv(&item.mount_point.display().to_string()),
            csv(&item.file_system),
            csv(&item.kind),
            csv(&item.name),
            item.removable,
            item.used_bytes,
            item.available_bytes,
            item.total_bytes,
            item.usage_percent
        );
    }
}

pub fn print_processes(report: &ProcessReport, tree: bool) {
    if report.projects.is_empty() {
        println!("No matching development processes found.");
        if !report.port_inspection_available {
            println!("Port inspection is unavailable on this platform.");
        }
        if !report.gpu_inspection_available {
            println!("NVIDIA GPU process inspection is unavailable.");
        }
        return;
    }

    println!(
        "{:<24} {:>9} {:>11} {:>11}  PORTS",
        "PROJECT", "CPU", "RAM", "GPU MEM"
    );
    println!("{}", "─".repeat(78));
    for group in &report.projects {
        let ports = join_ports(&group.ports);
        println!(
            "{:<24} {:>8.1}% {:>11} {:>11}  {}",
            truncate(&group.project, 23),
            group.cpu_percent,
            format_bytes(group.memory_bytes),
            if group.gpu_memory_bytes > 0 {
                format_bytes(group.gpu_memory_bytes)
            } else {
                "-".into()
            },
            if ports.is_empty() { "-" } else { &ports }
        );
        println!(
            "  Attribution: {}% confidence",
            group.attribution_confidence
        );
        for evidence in &group.attribution_evidence {
            println!("  Detected from: {}", safe(evidence));
        }
        if tree {
            print_process_tree(group);
        } else {
            print_process_summary(group);
        }
        println!();
    }
    println!(
        "{} process{} in {} project{}",
        report.process_count,
        if report.process_count == 1 { "" } else { "es" },
        report.projects.len(),
        if report.projects.len() == 1 { "" } else { "s" }
    );
    if report.unclassified_process_count > 0 {
        println!(
            "{} unclassified process{} hidden (use --all to include)",
            report.unclassified_process_count,
            if report.unclassified_process_count == 1 {
                ""
            } else {
                "es"
            }
        );
    }
}

fn print_process_summary(group: &ProjectProcesses) {
    let mut counts = std::collections::BTreeMap::new();
    for process in &group.processes {
        let entry = counts
            .entry(process.name.as_str())
            .or_insert((0usize, 0u64));
        entry.0 += 1;
        entry.1 = entry.1.max(process.runtime_seconds);
    }
    let len = counts.len();
    for (index, (name, (count, max_runtime))) in counts.into_iter().enumerate() {
        let branch = if index + 1 == len { "└─" } else { "├─" };
        let age = if max_runtime >= 3 * 86_400 {
            format!("  ⚠ running {}d", max_runtime / 86_400)
        } else {
            String::new()
        };
        if count == 1 {
            println!("{branch} {}{age}", safe(name));
        } else {
            println!("{branch} {} × {count}{age}", safe(name));
        }
    }
}

fn print_process_tree(group: &ProjectProcesses) {
    use std::collections::{BTreeMap, BTreeSet};
    let pids = group
        .processes
        .iter()
        .map(|process| process.pid)
        .collect::<BTreeSet<_>>();
    let mut children: BTreeMap<Option<u32>, Vec<&ProcessInfo>> = BTreeMap::new();
    for process in &group.processes {
        let parent = process.parent_pid.filter(|pid| pids.contains(pid));
        children.entry(parent).or_default().push(process);
    }
    let mut visited = BTreeSet::new();
    if let Some(roots) = children.get(&None) {
        for (index, process) in roots.iter().enumerate() {
            print_process_node(
                process,
                "",
                index + 1 == roots.len(),
                &children,
                &mut visited,
            );
        }
    }
}

fn print_process_node(
    process: &ProcessInfo,
    prefix: &str,
    last: bool,
    children: &std::collections::BTreeMap<Option<u32>, Vec<&ProcessInfo>>,
    visited: &mut std::collections::BTreeSet<u32>,
) {
    if !visited.insert(process.pid) {
        return;
    }
    let branch = if last { "└─" } else { "├─" };
    let ports = join_ports(&process.ports);
    let age = if process.runtime_seconds >= 86_400 {
        format!(" · {}d", process.runtime_seconds / 86_400)
    } else {
        String::new()
    };
    println!(
        "{prefix}{branch} {} (PID {} · {:.1}% · {}{}{}{})",
        safe(&process.name),
        process.pid,
        process.cpu_percent,
        format_bytes(process.memory_bytes),
        if ports.is_empty() { "" } else { " · :" },
        if ports.is_empty() { "" } else { &ports },
        age,
    );
    if let Some(nodes) = children.get(&Some(process.pid)) {
        let next_prefix = format!("{prefix}{}", if last { "   " } else { "│  " });
        for (index, child) in nodes.iter().enumerate() {
            print_process_node(
                child,
                &next_prefix,
                index + 1 == nodes.len(),
                children,
                visited,
            );
        }
    }
}

fn join_ports(ports: &[u16]) -> String {
    ports
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

pub fn print_disk(report: &DiskReport) {
    println!("Filesystem");
    println!("{}", safe(&report.filesystem.path.display().to_string()));
    println!();
    match (
        report.filesystem.used_bytes,
        report.filesystem.total_bytes,
        report.filesystem.usage_percent,
    ) {
        (Some(used), Some(total), Some(percent)) => {
            println!(
                "Used: {} / {}    {:.1}%",
                format_bytes(used),
                format_bytes(total),
                percent
            );
        }
        _ => println!("Used: unavailable"),
    }
    println!();
    println!(
        "Potentially reclaimable    {}",
        format_bytes(report.potentially_reclaimable_bytes)
    );
    print_disk_section("SAFE", &report.safe);
    print_disk_section("REVIEW", &report.review);
    print_disk_section("TOP", &report.top);
    if report.unreadable_entries > 0 {
        println!();
        println!(
            "⚠ {} entr{} could not be read; totals may be incomplete",
            report.unreadable_entries,
            if report.unreadable_entries == 1 {
                "y"
            } else {
                "ies"
            }
        );
    }
    println!();
    println!("Read-only analysis; no files were deleted.");
}

pub fn print_disk_csv(report: &DiskReport) {
    println!("section,category,bytes,path,reason,regeneratable");
    for (section, entries) in [
        ("safe", &report.safe),
        ("review", &report.review),
        ("top", &report.top),
    ] {
        for item in entries {
            println!(
                "{},{},{},{},{},{}",
                csv(section),
                csv(&item.category),
                item.bytes,
                csv(&item.path.display().to_string()),
                csv(&item.reason),
                item.regeneratable
                    .map(|value| value.to_string())
                    .unwrap_or_default()
            );
        }
    }
}

fn print_disk_section(title: &str, entries: &[DiskEntry]) {
    if entries.is_empty() {
        return;
    }
    println!();
    println!("{title}");
    for entry in entries {
        println!(
            "  {:>9}   {:<26} {}",
            format_bytes(entry.bytes),
            safe(&entry.category),
            safe(&entry.path.display().to_string())
        );
        println!("              Reason: {}", safe(&entry.reason));
        if let Some(regeneratable) = entry.regeneratable {
            println!(
                "              Regeneratable: {}",
                if regeneratable { "yes" } else { "no" }
            );
        } else {
            println!("              Regeneratable: generally");
        }
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB", "PB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value >= 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

pub fn print_json<T: Serialize>(value: &T, compact: bool) -> Result<()> {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    if compact {
        serde_json::to_writer(&mut lock, value)?;
    } else {
        serde_json::to_writer_pretty(&mut lock, value)?;
    }
    writeln!(lock)?;
    Ok(())
}

pub fn print_diff(left: &Snapshot, right: &Snapshot, report: &DiffReport) {
    let left_name = left.system.hostname.as_deref().unwrap_or("LEFT");
    let right_name = right.system.hostname.as_deref().unwrap_or("RIGHT");
    println!("{:<28} {:<24} {:<24}", "FIELD", left_name, right_name);
    println!("{}", "─".repeat(78));
    let left_fields = fields(left);
    let right_fields = fields(right);
    let keys = left_fields
        .keys()
        .chain(right_fields.keys())
        .collect::<std::collections::BTreeSet<_>>();
    let report_fields = report
        .differences
        .iter()
        .map(|difference| difference.field.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let shared = keys
        .into_iter()
        .filter(|key| !report_fields.contains(key.as_str()))
        .count();
    for (title, relevance) in [
        ("LIKELY RELEVANT", crate::diff::Relevance::LikelyRelevant),
        (
            "EXPECTED / INFORMATIONAL",
            crate::diff::Relevance::Informational,
        ),
    ] {
        let differences = report
            .differences
            .iter()
            .filter(|difference| difference.relevance == relevance)
            .collect::<Vec<_>>();
        if differences.is_empty() {
            continue;
        }
        println!();
        println!("{title}    {}", differences.len());
        for difference in differences {
            let key = &difference.field;
            let a = left_fields
                .get(key)
                .map(String::as_str)
                .unwrap_or("<missing>");
            let b = right_fields
                .get(key)
                .map(String::as_str)
                .unwrap_or("<missing>");
            println!(
                "{:<28} {:<24} {:<24} ≠",
                truncate(key, 27),
                truncate(a, 23),
                truncate(b, 23)
            );
        }
    }
    println!();
    println!(
        "{} difference{}",
        report.differences.len(),
        if report.differences.len() == 1 {
            ""
        } else {
            "s"
        }
    );
    if shared > 0 {
        println!(
            "{shared} unchanged or unselected field{} omitted",
            if shared == 1 { "" } else { "s" }
        );
    }
}

fn truncate(value: &str, max: usize) -> String {
    let value = safe(value);
    if value.chars().count() <= max {
        return value;
    }
    let mut text = value
        .chars()
        .take(max.saturating_sub(1))
        .collect::<String>();
    text.push('…');
    text
}

#[cfg(test)]
mod tests {
    #[test]
    fn neutralizes_terminal_controls() {
        assert_eq!(super::safe("safe\u{1b}[31m\nname"), "safe�[31m�name");
        assert!(!super::truncate("x\u{1b}[2J", 20).contains('\u{1b}'));
    }
}
