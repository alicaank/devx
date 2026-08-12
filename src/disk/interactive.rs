use std::{
    collections::VecDeque,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use crossterm::{
    ExecutableCommand, QueueableCommand,
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEventKind},
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{
        BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate, EnterAlternateScreen,
        LeaveAlternateScreen, disable_raw_mode, enable_raw_mode, size as terminal_size,
    },
};

#[derive(Clone)]
struct Entry {
    path: PathBuf,
    name: String,
    directory: bool,
    symlink: bool,
    bytes: Option<u64>,
    unreadable: u64,
}

struct ScanResult {
    generation: u64,
    path: PathBuf,
    bytes: u64,
    unreadable: u64,
}

struct Browser {
    path: PathBuf,
    entries: Vec<Entry>,
    selected: usize,
    offset: usize,
    generation: u64,
    completed: usize,
    sort_by_size: bool,
    show_hidden: bool,
    common_exclusions: bool,
    type_filter: TypeFilter,
    search: Option<String>,
    message: Option<String>,
    message_expires_at: Option<Instant>,
    cancel: Arc<AtomicBool>,
    sender: Sender<ScanResult>,
    receiver: Receiver<ScanResult>,
}

#[derive(Clone, Copy)]
enum TypeFilter {
    All,
    Directories,
    Files,
}

pub fn run(path: &Path) -> Result<()> {
    let path = fs::canonicalize(path)
        .with_context(|| format!("cannot resolve browser path {}", path.display()))?;
    if !path.is_dir() {
        anyhow::bail!(
            "interactive disk browser requires a directory: {}",
            path.display()
        );
    }

    let mut terminal = TerminalGuard::enter()?;
    let (sender, receiver) = mpsc::channel();
    let mut browser = Browser {
        path,
        entries: Vec::new(),
        selected: 0,
        offset: 0,
        generation: 0,
        completed: 0,
        sort_by_size: false,
        show_hidden: false,
        common_exclusions: false,
        type_filter: TypeFilter::All,
        search: None,
        message: None,
        message_expires_at: None,
        cancel: Arc::new(AtomicBool::new(false)),
        sender,
        receiver,
    };
    browser.reload();
    let mut dirty = true;

    loop {
        dirty |= browser.receive_sizes();
        dirty |= browser.expire_message();
        if dirty {
            browser.render(&mut terminal.stdout)?;
            dirty = false;
        }
        if !event::poll(Duration::from_millis(80))? {
            continue;
        }
        let event = event::read()?;
        if matches!(event, Event::Resize(_, _)) {
            dirty = true;
            continue;
        }
        let Event::Key(key) = event else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break,
            KeyCode::Up | KeyCode::Char('k') => browser.move_up(1),
            KeyCode::Down | KeyCode::Char('j') => browser.move_down(1),
            KeyCode::PageUp => browser.move_up(browser.page_height()),
            KeyCode::PageDown => browser.move_down(browser.page_height()),
            KeyCode::Home => browser.select(0),
            KeyCode::End => browser.select(browser.entries.len().saturating_sub(1)),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => browser.open_selected(),
            KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => browser.open_parent(),
            KeyCode::Char('r') => browser.reload(),
            KeyCode::Char('s') => browser.toggle_sort(),
            KeyCode::Char('d') => browser.trash_selected(&mut terminal.stdout)?,
            KeyCode::Char('.') => {
                browser.show_hidden = !browser.show_hidden;
                browser.reload();
            }
            KeyCode::Char('f') => {
                browser.type_filter = browser.type_filter.next();
                browser.reload();
            }
            KeyCode::Char('x') => {
                browser.common_exclusions = !browser.common_exclusions;
                browser.reload();
            }
            KeyCode::Char('/') => {
                browser.search = prompt_search(&mut terminal.stdout)?;
                browser.reload();
            }
            KeyCode::Char('i') => browser.show_details(&mut terminal.stdout)?,
            KeyCode::Char('c') => {
                browser.cancel.store(true, Ordering::Relaxed);
                browser.set_transient_message("Scan cancelled", Duration::from_secs(2));
            }
            _ => continue,
        }
        dirty = true;
    }
    browser.cancel.store(true, Ordering::Relaxed);
    Ok(())
}

impl Browser {
    fn reload(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.generation = self.generation.wrapping_add(1);
        self.completed = 0;
        self.selected = 0;
        self.offset = 0;
        self.message = None;
        self.message_expires_at = None;
        self.entries = match read_entries(
            &self.path,
            self.show_hidden,
            self.common_exclusions,
            self.type_filter,
            self.search.as_deref(),
        ) {
            Ok(entries) => entries,
            Err(error) => {
                self.message = Some(error.to_string());
                Vec::new()
            }
        };
        self.sort_entries();
        spawn_scanner(
            self.generation,
            &self.entries,
            self.cancel.clone(),
            self.sender.clone(),
        );
    }

    fn receive_sizes(&mut self) -> bool {
        let mut changed = false;
        while let Ok(result) = self.receiver.try_recv() {
            if result.generation != self.generation {
                continue;
            }
            if let Some(entry) = self
                .entries
                .iter_mut()
                .find(|entry| entry.path == result.path)
            {
                entry.bytes = Some(result.bytes);
                entry.unreadable = result.unreadable;
                self.completed += 1;
                changed = true;
            }
        }
        if changed && self.sort_by_size {
            self.sort_preserving_selection();
        }
        changed
    }

    fn render(&mut self, stdout: &mut io::Stdout) -> Result<()> {
        let (width, height) = terminal_size()?;
        let page_height = height.saturating_sub(8).max(1) as usize;
        self.keep_visible(page_height);
        stdout
            .queue(BeginSynchronizedUpdate)?
            .queue(MoveTo(0, 0))?
            .queue(Clear(ClearType::All))?;
        stdout
            .queue(SetForegroundColor(Color::Cyan))?
            .queue(SetAttribute(Attribute::Bold))?
            .queue(Print("devx disk browser"))?
            .queue(ResetColor)?
            .queue(SetAttribute(Attribute::Reset))?
            .queue(Print("  [confirmed trash enabled]\r\n"))?;
        stdout.queue(Print(format!(
            "Path: {}\r\n",
            truncate(
                &self.path.display().to_string(),
                width.saturating_sub(6) as usize
            )
        )))?;

        let capacity = super::capacity(&self.path);
        if let (Some(used), Some(total), Some(percent)) = (
            capacity.used_bytes,
            capacity.total_bytes,
            capacity.usage_percent,
        ) {
            let bar_width = usize::from(width.saturating_sub(48).clamp(10, 30));
            let filled = ((percent / 100.0) * bar_width as f64).round() as usize;
            let bar = format!(
                "{}{}",
                "█".repeat(filled.min(bar_width)),
                "░".repeat(bar_width.saturating_sub(filled))
            );
            stdout.queue(Print(format!(
                "Disk: {} / {}  {:>5.1}% [{bar}]\r\n",
                format_bytes(used),
                format_bytes(total),
                percent
            )))?;
        } else {
            stdout.queue(Print("Disk: capacity unavailable\r\n"))?;
        }
        stdout
            .queue(SetAttribute(Attribute::Bold))?
            .queue(Print(format!(
                "{:>11}  {:<10} {:<9} NAME\r\n",
                "SIZE", "SHARE", "TYPE"
            )))?
            .queue(SetAttribute(Attribute::Reset))?
            .queue(Print("─".repeat(width as usize)))?
            .queue(Print("\r\n"))?;

        let largest = self
            .entries
            .iter()
            .filter_map(|entry| entry.bytes)
            .max()
            .unwrap_or(0);
        for (visible_index, entry) in self
            .entries
            .iter()
            .skip(self.offset)
            .take(page_height)
            .enumerate()
        {
            let index = self.offset + visible_index;
            if index == self.selected {
                stdout.queue(SetAttribute(Attribute::Reverse))?;
            }
            let size = entry
                .bytes
                .map(format_bytes)
                .unwrap_or_else(|| "scanning…".into());
            let kind = entry_type(entry);
            let suffix = if entry.directory {
                "/"
            } else if entry.symlink {
                "@"
            } else {
                ""
            };
            let warning = if entry.unreadable > 0 { " ⚠" } else { "" };
            let bar = entry
                .bytes
                .map(|bytes| size_bar(bytes, largest, 8))
                .unwrap_or_else(|| "        ".into());
            let reserved = 39usize;
            let name_width = (width as usize).saturating_sub(reserved).max(1);
            stdout.queue(Print(format!(
                "{:>11}  {:<10} {:<9} {}{}{}\r\n",
                size,
                bar,
                kind,
                truncate(&entry.name, name_width),
                suffix,
                warning
            )))?;
            if index == self.selected {
                stdout.queue(SetAttribute(Attribute::Reset))?;
            }
        }

        let status_row = height.saturating_sub(2);
        stdout.queue(MoveTo(0, status_row))?;
        if let Some(message) = &self.message {
            stdout
                .queue(SetForegroundColor(Color::Red))?
                .queue(Print(truncate(message, width as usize)))?
                .queue(ResetColor)?;
        } else {
            stdout.queue(Print(format!(
                "{} entries · sizes {}/{} · sort: {} · filter: {}{}{}",
                self.entries.len(),
                self.completed,
                self.entries.len(),
                if self.sort_by_size { "size" } else { "name" },
                self.type_filter.label(),
                if self.show_hidden { " · hidden" } else { "" },
                self.search
                    .as_ref()
                    .map(|q| format!(" · /{q}"))
                    .unwrap_or_default()
            )))?;
        }
        stdout
            .queue(MoveTo(0, height.saturating_sub(1)))?
            .queue(SetForegroundColor(Color::DarkGrey))?
            .queue(Print(truncate(
                "↑↓ move  Enter open  / search  . hidden  f type  x exclude  i info  c cancel  d trash  q quit",
                width as usize,
            )))?
            .queue(ResetColor)?
            .queue(EndSynchronizedUpdate)?;
        stdout.flush()?;
        Ok(())
    }

    fn page_height(&self) -> usize {
        terminal_size()
            .map(|(_, height)| height.saturating_sub(8).max(1) as usize)
            .unwrap_or(10)
    }

    fn move_up(&mut self, amount: usize) {
        self.select(self.selected.saturating_sub(amount));
    }

    fn move_down(&mut self, amount: usize) {
        self.select(
            self.selected
                .saturating_add(amount)
                .min(self.entries.len().saturating_sub(1)),
        );
    }

    fn select(&mut self, index: usize) {
        self.selected = index.min(self.entries.len().saturating_sub(1));
    }

    fn keep_visible(&mut self, height: usize) {
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset.saturating_add(height) {
            self.offset = self.selected.saturating_add(1).saturating_sub(height);
        }
    }

    fn open_selected(&mut self) {
        let Some(entry) = self.entries.get(self.selected) else {
            return;
        };
        if !entry.directory || entry.symlink {
            return;
        }
        self.path = entry.path.clone();
        self.reload();
    }

    fn open_parent(&mut self) {
        let Some(parent) = self.path.parent() else {
            return;
        };
        let previous = self.path.clone();
        self.path = parent.to_path_buf();
        self.reload();
        if let Some(index) = self.entries.iter().position(|entry| entry.path == previous) {
            self.selected = index;
        }
    }

    fn toggle_sort(&mut self) {
        self.sort_by_size = !self.sort_by_size;
        self.sort_preserving_selection();
    }

    fn show_details(&mut self, stdout: &mut io::Stdout) -> Result<()> {
        let Some(entry) = self.entries.get(self.selected) else {
            return Ok(());
        };
        let metadata = fs::symlink_metadata(&entry.path)?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|value| value.as_secs().to_string())
            .unwrap_or_else(|| "unknown".into());
        let accessed = metadata
            .accessed()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|value| value.as_secs().to_string())
            .unwrap_or_else(|| "unknown".into());
        let permissions = permissions(&metadata);
        let owner = owner(&metadata);
        let mime = detected_type(&entry.path, entry);
        let git = git_status(&entry.path).unwrap_or_else(|| "not tracked / unavailable".into());
        let link = if entry.symlink {
            fs::read_link(&entry.path)
                .ok()
                .map(|path| format!("\r\nSymlink target: {}", path.display()))
                .unwrap_or_default()
        } else {
            String::new()
        };
        let (_, height) = terminal_size()?;
        stdout.queue(MoveTo(0, height.saturating_sub(12)))?.queue(Clear(ClearType::FromCursorDown))?
            .queue(SetForegroundColor(Color::Cyan))?.queue(SetAttribute(Attribute::Bold))?.queue(Print("Item details\r\n"))?
            .queue(SetAttribute(Attribute::Reset))?.queue(Print(format!("Path: {}\r\nType: {}\r\nContent type: {}\r\nLogical size: {}\r\nAllocated tree size: {}\r\nModified (Unix): {}\r\nAccessed (Unix): {}\r\nOwner: {}\r\nPermissions: {}\r\nGit: {}{}\r\n\r\nPress any key to close", safe_text(&entry.path.display().to_string()), entry_type(entry), mime, format_bytes(metadata.len()), entry.bytes.map(format_bytes).unwrap_or_else(|| "scanning".into()), modified, accessed, owner, permissions, safe_text(&git), safe_text(&link))))?.queue(ResetColor)?;
        stdout.flush()?;
        loop {
            if let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                return Ok(());
            }
        }
    }

    fn trash_selected(&mut self, stdout: &mut io::Stdout) -> Result<()> {
        let Some(entry) = self.entries.get(self.selected).cloned() else {
            return Ok(());
        };
        if !safe_trash_target(&self.path, &entry.path) {
            self.message = Some("Refusing to trash a path outside the current directory".into());
            self.message_expires_at = None;
            return Ok(());
        }
        let original_identity = fs::symlink_metadata(&entry.path)
            .ok()
            .and_then(|metadata| file_identity(&metadata));
        let (_, height) = terminal_size()?;
        stdout
            .queue(MoveTo(0, height.saturating_sub(3)))?
            .queue(Clear(ClearType::FromCursorDown))?
            .queue(SetForegroundColor(Color::Yellow))?
            .queue(SetAttribute(Attribute::Bold))?
            .queue(Print(format!(
                "Move to trash? {}\r\n",
                safe_text(&entry.path.display().to_string())
            )))?
            .queue(SetAttribute(Attribute::Reset))?
            .queue(Print("Press y to confirm; any other key cancels."))?
            .queue(ResetColor)?;
        stdout.flush()?;

        loop {
            let event = event::read()?;
            let Event::Key(key) = event else { continue };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if key.code != KeyCode::Char('y') {
                self.set_transient_message("Trash operation cancelled", Duration::from_secs(2));
                return Ok(());
            }
            if !safe_trash_target(&self.path, &entry.path)
                || original_identity
                    != fs::symlink_metadata(&entry.path)
                        .ok()
                        .and_then(|metadata| file_identity(&metadata))
            {
                self.set_transient_message(
                    "Selected item changed while awaiting confirmation; rescan and try again",
                    Duration::from_secs(4),
                );
                return Ok(());
            }
            self.cancel.store(true, Ordering::Relaxed);
            match trash::delete(&entry.path) {
                Ok(()) => {
                    let name = entry.name;
                    self.reload();
                    self.set_transient_message(
                        format!("Moved {name} to the system trash"),
                        Duration::from_secs(3),
                    );
                }
                Err(error) => {
                    self.message = Some(format!(
                        "Could not move {} to trash: {error}",
                        entry.path.display()
                    ));
                    self.message_expires_at = None;
                }
            }
            return Ok(());
        }
    }

    fn set_transient_message(&mut self, message: impl Into<String>, duration: Duration) {
        self.message = Some(message.into());
        self.message_expires_at = Some(Instant::now() + duration);
    }

    fn expire_message(&mut self) -> bool {
        if self
            .message_expires_at
            .is_some_and(|expires_at| Instant::now() >= expires_at)
        {
            self.message = None;
            self.message_expires_at = None;
            true
        } else {
            false
        }
    }

    fn sort_preserving_selection(&mut self) {
        let selected = self
            .entries
            .get(self.selected)
            .map(|entry| entry.path.clone());
        self.sort_entries();
        if let Some(selected) = selected
            && let Some(index) = self.entries.iter().position(|entry| entry.path == selected)
        {
            self.selected = index;
        }
    }

    fn sort_entries(&mut self) {
        if self.sort_by_size {
            self.entries.sort_by(|a, b| {
                b.bytes
                    .unwrap_or(0)
                    .cmp(&a.bytes.unwrap_or(0))
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
        } else {
            self.entries.sort_by(|a, b| {
                b.directory
                    .cmp(&a.directory)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
        }
    }
}

impl TypeFilter {
    fn next(self) -> Self {
        match self {
            Self::All => Self::Directories,
            Self::Directories => Self::Files,
            Self::Files => Self::All,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Directories => "dirs",
            Self::Files => "files",
        }
    }
    fn matches(self, directory: bool) -> bool {
        match self {
            Self::All => true,
            Self::Directories => directory,
            Self::Files => !directory,
        }
    }
}

fn read_entries(
    path: &Path,
    show_hidden: bool,
    exclusions: bool,
    type_filter: TypeFilter,
    search: Option<&str>,
) -> Result<Vec<Entry>> {
    fs::read_dir(path)
        .with_context(|| format!("cannot read {}", path.display()))?
        .filter_map(|result| {
            let entry = match result {
                Ok(entry) => entry,
                Err(error) => return Some(Err(error)),
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if (!show_hidden && name.starts_with('.'))
                || (exclusions && common_excluded(&name))
                || search.is_some_and(|query| !name.to_lowercase().contains(&query.to_lowercase()))
            {
                return None;
            }
            let metadata = match fs::symlink_metadata(entry.path()) {
                Ok(value) => value,
                Err(error) => return Some(Err(error)),
            };
            if !type_filter.matches(metadata.is_dir()) {
                return None;
            }
            Some(Ok(Entry {
                path: entry.path(),
                name,
                directory: metadata.is_dir(),
                symlink: metadata.file_type().is_symlink(),
                bytes: None,
                unreadable: 0,
            }))
        })
        .collect::<io::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn common_excluded(name: &str) -> bool {
    matches!(
        name,
        ".git" | "node_modules" | "target" | ".venv" | "venv" | "__pycache__"
    )
}

fn prompt_search(stdout: &mut io::Stdout) -> Result<Option<String>> {
    let (_, height) = terminal_size()?;
    let mut query = String::new();
    loop {
        stdout
            .queue(MoveTo(0, height.saturating_sub(2)))?
            .queue(Clear(ClearType::FromCursorDown))?
            .queue(Print(format!("Search: {query}_  (Enter apply, Esc clear)")))?;
        stdout.flush()?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Enter => return Ok((!query.is_empty()).then_some(query)),
            KeyCode::Esc => return Ok(None),
            KeyCode::Backspace => {
                query.pop();
            }
            KeyCode::Char(character) => query.push(character),
            _ => {}
        }
    }
}

fn size_bar(bytes: u64, largest: u64, width: usize) -> String {
    if largest == 0 {
        return " ".repeat(width);
    }
    let filled = ((bytes as f64 / largest as f64) * width as f64).ceil() as usize;
    format!(
        "{}{}",
        "█".repeat(filled.min(width)),
        "░".repeat(width.saturating_sub(filled))
    )
}

#[cfg(unix)]
fn permissions(metadata: &fs::Metadata) -> String {
    use std::os::unix::fs::PermissionsExt;
    format!("{:o}", metadata.permissions().mode() & 0o7777)
}

#[cfg(unix)]
fn owner(metadata: &fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    format!("uid {} / gid {}", metadata.uid(), metadata.gid())
}

#[cfg(not(unix))]
fn owner(_: &fs::Metadata) -> String {
    "unavailable".into()
}

fn detected_type(path: &Path, entry: &Entry) -> String {
    if entry.directory {
        return "directory".into();
    }
    if entry.symlink {
        return "symbolic link".into();
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "rs" => "Rust source",
        "py" => "Python source",
        "js" | "mjs" | "cjs" => "JavaScript source",
        "ts" | "tsx" => "TypeScript source",
        "json" => "JSON data",
        "toml" => "TOML data",
        "yaml" | "yml" => "YAML data",
        "md" => "Markdown document",
        "png" | "jpg" | "jpeg" | "gif" | "webp" => "image",
        "mp4" | "mkv" | "mov" | "avi" => "video",
        "zip" | "gz" | "xz" | "zst" | "tar" => "archive",
        "so" | "dll" | "dylib" => "shared library",
        "" => "file",
        other => return format!("{other} file"),
    }
    .into()
}

fn git_status(path: &Path) -> Option<String> {
    let parent = path.parent()?;
    let name = path.file_name()?;
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain", "--"])
        .arg(name)
        .current_dir(parent)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    if text.trim().is_empty() {
        Some("clean or untracked outside repository".into())
    } else {
        Some(text.lines().next().unwrap_or("changed").trim().into())
    }
}
#[cfg(not(unix))]
fn permissions(metadata: &fs::Metadata) -> String {
    if metadata.permissions().readonly() {
        "read-only".into()
    } else {
        "read/write".into()
    }
}

fn entry_type(entry: &Entry) -> String {
    if entry.symlink {
        return "link".into();
    }
    if entry.directory {
        return "directory".into();
    }
    let extension = entry
        .path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("rs") => "Rust".into(),
        Some("py") => "Python".into(),
        Some("js" | "mjs" | "cjs") => "JavaScript".into(),
        Some("ts" | "tsx") => "TypeScript".into(),
        Some("json" | "jsonl") => "JSON".into(),
        Some("toml") => "TOML".into(),
        Some("yaml" | "yml") => "YAML".into(),
        Some("md" | "markdown") => "Markdown".into(),
        Some("txt" | "log" | "csv" | "tsv") => "text".into(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff") => "image".into(),
        Some("mp4" | "mkv" | "mov" | "avi" | "webm") => "video".into(),
        Some("mp3" | "wav" | "flac" | "ogg" | "m4a") => "audio".into(),
        Some("zip" | "tar" | "gz" | "bz2" | "xz" | "zst" | "7z" | "rar") => "archive".into(),
        Some("pdf") => "PDF".into(),
        Some("so" | "dll" | "dylib") => "library".into(),
        Some("exe" | "bin") => "binary".into(),
        Some(value) => truncate(value, 9),
        None => "file".into(),
    }
}

fn safe_trash_target(current_directory: &Path, target: &Path) -> bool {
    target != current_directory
        && target.parent() == Some(current_directory)
        && fs::symlink_metadata(target).is_ok()
}

fn spawn_scanner(
    generation: u64,
    entries: &[Entry],
    cancel: Arc<AtomicBool>,
    sender: Sender<ScanResult>,
) {
    let paths = entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<VecDeque<_>>();
    if paths.is_empty() {
        return;
    }
    let queue = Arc::new(Mutex::new(paths));
    let seen_files = Arc::new(Mutex::new(std::collections::BTreeSet::new()));
    let workers = thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(2)
        .clamp(2, 4)
        .min(entries.len());
    for _ in 0..workers {
        let queue = queue.clone();
        let cancel = cancel.clone();
        let sender = sender.clone();
        let seen_files = seen_files.clone();
        thread::spawn(move || {
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let path = {
                    let Ok(mut queue) = queue.lock() else { return };
                    queue.pop_front()
                };
                let Some(path) = path else { return };
                let mut unreadable = 0;
                let bytes = scan_size_shared(&path, &cancel, &mut unreadable, &seen_files);
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                if sender
                    .send(ScanResult {
                        generation,
                        path,
                        bytes,
                        unreadable,
                    })
                    .is_err()
                {
                    return;
                }
            }
        });
    }
}

#[cfg(test)]
fn scan_size(path: &Path, cancel: &AtomicBool, unreadable: &mut u64) -> u64 {
    scan_size_shared(
        path,
        cancel,
        unreadable,
        &Mutex::new(std::collections::BTreeSet::new()),
    )
}

fn scan_size_shared(
    path: &Path,
    cancel: &AtomicBool,
    unreadable: &mut u64,
    seen_files: &Mutex<std::collections::BTreeSet<(u64, u64)>>,
) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        *unreadable += 1;
        return 0;
    };
    let device = device_id(&metadata);
    scan_size_on_device(path, cancel, unreadable, device, seen_files)
}

fn scan_size_on_device(
    path: &Path,
    cancel: &AtomicBool,
    unreadable: &mut u64,
    device: Option<u64>,
    seen_files: &Mutex<std::collections::BTreeSet<(u64, u64)>>,
) -> u64 {
    if cancel.load(Ordering::Relaxed) {
        return 0;
    }
    let Ok(metadata) = fs::symlink_metadata(path) else {
        *unreadable += 1;
        return 0;
    };
    if metadata.file_type().is_symlink() || device_id(&metadata) != device {
        return 0;
    }
    if metadata.is_file() {
        if let Some(identity) = file_identity(&metadata)
            && let Ok(mut seen) = seen_files.lock()
            && !seen.insert(identity)
        {
            return 0;
        }
        return super::allocated_bytes(&metadata);
    }
    let Ok(entries) = fs::read_dir(path) else {
        *unreadable += 1;
        return 0;
    };
    entries
        .map(|entry| match entry {
            Ok(entry) => scan_size_on_device(&entry.path(), cancel, unreadable, device, seen_files),
            Err(_) => {
                *unreadable += 1;
                0
            }
        })
        .sum()
}

#[cfg(unix)]
fn device_id(metadata: &fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.dev())
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn device_id(_: &fs::Metadata) -> Option<u64> {
    None
}

#[cfg(not(unix))]
fn file_identity(_: &fs::Metadata) -> Option<(u64, u64)> {
    None
}

struct TerminalGuard {
    stdout: io::Stdout,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("interactive mode requires a terminal")?;
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

fn truncate(value: &str, max: usize) -> String {
    let value = safe_text(value);
    if value.chars().count() <= max {
        return value;
    }
    let mut output = value
        .chars()
        .take(max.saturating_sub(1))
        .collect::<String>();
    output.push('…');
    output
}

fn safe_text(value: &str) -> String {
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, atomic::AtomicBool};

    #[test]
    fn scanner_does_not_follow_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("file");
        std::fs::write(&file, vec![0; 8192]).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&file, temp.path().join("link")).unwrap();
        let mut unreadable = 0;
        let size = super::scan_size(temp.path(), &AtomicBool::new(false), &mut unreadable);
        assert!(size >= 8192);
        assert!(size < 16384);
        assert_eq!(unreadable, 0);
    }

    #[test]
    fn scanner_honors_cancellation() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("file"), vec![0; 8192]).unwrap();
        let cancelled = Arc::new(AtomicBool::new(true));
        let mut unreadable = 0;
        assert_eq!(
            super::scan_size(temp.path(), &cancelled, &mut unreadable),
            0
        );
    }

    #[test]
    fn trash_target_must_be_an_existing_direct_child() {
        let temp = tempfile::tempdir().unwrap();
        let child = temp.path().join("child");
        std::fs::write(&child, "data").unwrap();
        assert!(super::safe_trash_target(temp.path(), &child));
        assert!(!super::safe_trash_target(temp.path(), temp.path()));
        assert!(!super::safe_trash_target(
            temp.path(),
            &temp.path().join("missing")
        ));
        assert!(!super::safe_trash_target(
            temp.path(),
            &temp.path().join("nested/child")
        ));
    }

    #[test]
    fn identifies_file_types_from_extensions() {
        let entry = |name: &str| super::Entry {
            path: std::path::PathBuf::from(name),
            name: name.into(),
            directory: false,
            symlink: false,
            bytes: None,
            unreadable: 0,
        };
        assert_eq!(super::entry_type(&entry("main.rs")), "Rust");
        assert_eq!(super::entry_type(&entry("data.json")), "JSON");
        assert_eq!(super::entry_type(&entry("photo.png")), "image");
        assert_eq!(
            super::entry_type(&entry("weights.safetensors")),
            "safetens…"
        );
        assert_eq!(super::entry_type(&entry("LICENSE")), "file");
    }

    #[test]
    fn neutralizes_terminal_controls_in_names() {
        assert_eq!(super::truncate("bad\u{1b}[2J", 20), "bad�[2J");
    }
}
