use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::{self, IsTerminal, Write},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use crossterm::{
    ExecutableCommand,
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    style::Print,
    terminal::{
        BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate, EnterAlternateScreen,
        LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    },
};

use super::ProcessFilter;

pub fn run(filter: ProcessFilter, tree: bool, interval_seconds: f64) -> Result<()> {
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        anyhow::bail!("process watch requires an interactive terminal");
    }
    let interval = refresh_interval(interval_seconds)?;
    let mut terminal = TerminalGuard::enter()?;
    terminal
        .stdout
        .execute(MoveTo(0, 0))?
        .execute(Print("Scanning processes…"))?;

    let mut histories: BTreeMap<String, VecDeque<f32>> = BTreeMap::new();
    let mut peaks: BTreeMap<String, (f32, u64, u64)> = BTreeMap::new();
    let mut previous: BTreeMap<u32, String> = BTreeMap::new();
    let mut events: VecDeque<String> = VecDeque::new();
    loop {
        let started = Instant::now();
        let report = super::scan(filter.clone());
        update_history(&report, &mut histories, &mut peaks);
        update_events(&report, &mut previous, &mut events);
        terminal
            .stdout
            .execute(BeginSynchronizedUpdate)?
            .execute(MoveTo(0, 0))?
            .execute(Clear(ClearType::All))?;
        crate::output::print_processes(&report, tree);
        print_history(&report, &histories, &peaks, &events);
        println!();
        println!(
            "Watching every {:.1}s · last scan {:.0}ms · q/Esc/Ctrl-C quit",
            interval.as_secs_f64(),
            started.elapsed().as_secs_f64() * 1000.0
        );
        io::stdout().flush()?;
        terminal.stdout.execute(EndSynchronizedUpdate)?;

        let deadline = Instant::now() + interval;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            if !event::poll(remaining.min(Duration::from_millis(100)))? {
                continue;
            }
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let quit = matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
                        || (key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL));
                    if quit {
                        return Ok(());
                    }
                    if key.code == KeyCode::Char('r') {
                        break;
                    }
                }
                Event::Resize(_, _) => break,
                _ => {}
            }
        }
    }
}

fn update_history(
    report: &super::ProcessReport,
    histories: &mut BTreeMap<String, VecDeque<f32>>,
    peaks: &mut BTreeMap<String, (f32, u64, u64)>,
) {
    for project in &report.projects {
        let history = histories.entry(project.project.clone()).or_default();
        history.push_back(project.cpu_percent);
        if history.len() > 24 {
            history.pop_front();
        }
        let peak = peaks.entry(project.project.clone()).or_default();
        peak.0 = peak.0.max(project.cpu_percent);
        peak.1 = peak.1.max(project.memory_bytes);
        peak.2 = peak.2.max(project.gpu_memory_bytes);
    }
}

fn update_events(
    report: &super::ProcessReport,
    previous: &mut BTreeMap<u32, String>,
    events: &mut VecDeque<String>,
) {
    let current = report
        .projects
        .iter()
        .flat_map(|project| {
            project
                .processes
                .iter()
                .map(|process| (process.pid, format!("{}:{}", project.project, process.name)))
        })
        .collect::<BTreeMap<_, _>>();
    let old = previous.keys().copied().collect::<BTreeSet<_>>();
    let new = current.keys().copied().collect::<BTreeSet<_>>();
    for pid in new.difference(&old) {
        push_event(events, format!("started  {} (pid {pid})", current[pid]));
    }
    for pid in old.difference(&new) {
        push_event(events, format!("exited   {} (pid {pid})", previous[pid]));
    }
    *previous = current;
}

fn push_event(events: &mut VecDeque<String>, event: String) {
    events.push_front(event);
    events.truncate(4);
}

fn print_history(
    report: &super::ProcessReport,
    histories: &BTreeMap<String, VecDeque<f32>>,
    peaks: &BTreeMap<String, (f32, u64, u64)>,
    events: &VecDeque<String>,
) {
    if !report.projects.is_empty() {
        println!("\nCPU history and session peaks");
    }
    for project in &report.projects {
        let history = histories.get(&project.project).cloned().unwrap_or_default();
        let peak = peaks.get(&project.project).copied().unwrap_or_default();
        println!(
            "{:<24} {:<24} CPU {:>6.1}%  RAM {:>9}  GPU {:>9}",
            truncate(&project.project, 24),
            sparkline(&history),
            peak.0,
            format_bytes(peak.1),
            if peak.2 == 0 {
                "-".into()
            } else {
                format_bytes(peak.2)
            }
        );
    }
    if !events.is_empty() {
        println!("\nRecent lifecycle events");
        for event in events {
            println!("  {event}");
        }
    }
}

fn sparkline(values: &VecDeque<f32>) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = values.iter().copied().fold(0.0_f32, f32::max).max(1.0);
    values
        .iter()
        .map(|value| {
            let index = ((*value / max) * 7.0).round().clamp(0.0, 7.0) as usize;
            BARS[index]
        })
        .collect()
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.into()
    } else {
        value
            .chars()
            .take(max.saturating_sub(1))
            .collect::<String>()
            + "…"
    }
}

fn refresh_interval(seconds: f64) -> Result<Duration> {
    if !seconds.is_finite() || !(0.5..=60.0).contains(&seconds) {
        anyhow::bail!("watch interval must be between 0.5 and 60 seconds");
    }
    Ok(Duration::from_secs_f64(seconds))
}

struct TerminalGuard {
    stdout: io::Stdout,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("could not enable terminal raw mode")?;
        let mut stdout = io::stdout();
        if let Err(error) = stdout
            .execute(EnterAlternateScreen)
            .and_then(|out| out.execute(Hide))
        {
            let _ = disable_raw_mode();
            return Err(error.into());
        }
        Ok(Self { stdout })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.stdout.execute(Show);
        let _ = self.stdout.execute(LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    #[test]
    fn validates_refresh_interval() {
        assert!(super::refresh_interval(0.5).is_ok());
        assert!(super::refresh_interval(60.0).is_ok());
        assert!(super::refresh_interval(0.49).is_err());
        assert!(super::refresh_interval(f64::NAN).is_err());
    }

    #[test]
    fn creates_bounded_sparkline() {
        let values = VecDeque::from([0.0, 50.0, 100.0]);
        assert_eq!(super::sparkline(&values).chars().count(), 3);
        assert!(super::sparkline(&values).ends_with('█'));
    }
}
