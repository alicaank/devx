mod cli;
mod diff;
mod disk;
mod doctor;
mod output;
mod processes;
mod snapshot;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Command};

fn main() {
    if let Err(error) = run() {
        eprintln!("devx: {error:#}");
        std::process::exit(2);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Snapshot(args) => {
            let mut snapshot = snapshot::scan_profile(&args.path, args.minimal)?;
            if args.redact_paths {
                snapshot::redact_paths(&mut snapshot);
            }
            if let Some(path) = args.output {
                snapshot::write(&path, &snapshot, args.compact)?;
            } else {
                output::print_json(&snapshot, args.compact)?;
            }
        }
        Command::Doctor(args) => {
            let snapshot = snapshot::scan_for_doctor(&args.path)?;
            let report = doctor::diagnose(&snapshot);
            if args.json {
                output::print_json(&report, false)?;
            } else if args.markdown {
                output::print_doctor_markdown(&snapshot, &report);
            } else {
                output::print_doctor(&snapshot, &report, args.explain);
            }
            if args.strict && report.has_problems() {
                std::process::exit(1);
            }
        }
        Command::Disk(args) => {
            if args.interactive {
                disk::interactive(&args.path)?;
            } else if args.duplicates {
                let report = disk::duplicates(&args.path, args.older_than, args.larger_than)?;
                if args.json {
                    output::print_json(&report, false)?;
                } else if args.csv {
                    output::print_duplicates_csv(&report);
                } else {
                    output::print_duplicates(&report);
                }
            } else if args.older_than.is_some() || args.larger_than.is_some() {
                let report = disk::query_files(&args.path, args.older_than, args.larger_than)?;
                if args.json {
                    output::print_json(&report, false)?;
                } else if args.csv {
                    output::print_file_query_csv(&report);
                } else {
                    output::print_file_query(&report);
                }
            } else if args.filesystems {
                let report = disk::filesystems();
                if args.json {
                    output::print_json(&report, false)?;
                } else if args.csv {
                    output::print_filesystems_csv(&report);
                } else {
                    output::print_filesystems(&report);
                }
            } else {
                let report = disk::analyze(&args.path, args.project, args.safe, args.top)?;
                if args.json {
                    output::print_json(&report, false)?;
                } else if args.csv {
                    output::print_disk_csv(&report);
                } else {
                    output::print_disk(&report);
                }
            }
        }
        Command::Ps(args) => {
            let filter = processes::ProcessFilter {
                project: args.project,
                ports_only: args.ports,
                gpu_only: args.gpu,
                include_unclassified: args.all,
            };
            if args.watch {
                processes::watch(filter, args.tree, args.interval)?;
            } else {
                let report = processes::scan(filter);
                if let Some(path) = args.snapshot {
                    output::write_json_file(&path, &report, false)?;
                } else if args.json {
                    output::print_json(&report, false)?;
                } else if args.csv {
                    output::print_processes_csv(&report);
                } else {
                    output::print_processes(&report, args.tree);
                }
            }
        }
        Command::Diff(args) => {
            let left = snapshot::read(&args.left)?;
            let right = snapshot::read(&args.right)?;
            let report = diff::compare_with(
                &left,
                &right,
                diff::DiffOptions {
                    only: args.only,
                    ignore_host: args.ignore_host,
                    project_only: args.project,
                },
            );
            if args.json {
                output::print_json(&report, false)?;
            } else {
                output::print_diff(&left, &right, &report);
            }
            if args.strict && report.has_relevant_differences() {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}
