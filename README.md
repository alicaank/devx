# devx

`devx` is a fast, local-only CLI for understanding a development machine. It compares portable environment snapshots, groups running processes by project, and explains where disk space is going.

No daemon, Python package, account, or AI service is required.

## Install

Requirements: Rust 1.92 or newer. Linux is fully supported; macOS and Windows support the core commands with reduced process inspection.

From a local checkout:

```bash
git clone https://github.com/alicaank/devx.git
cd devx
cargo install --locked --path .
```

Upgrade after pulling new changes:

```bash
cargo install --locked --path . --force
```

The executable is normally installed at `~/.cargo/bin/devx`. Ensure that directory is in `PATH`:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

## Quick start

```bash
# Browse storage interactively
devx disk --interactive -C ~

# Find large, old, or duplicate files (read-only)
devx disk -C ~/projects --larger-than 2GB
devx disk -C ~/projects --older-than 180d
devx disk -C ~/projects --duplicates --larger-than 100MB

# Group processes by development project
devx ps
devx ps --watch

# Capture and compare environments
devx snapshot --minimal --redact-paths -o laptop.json
devx diff laptop.json server.json
```

Use `-C PATH` with `snapshot` and `disk` to inspect a directory other than the current one. Run `devx COMMAND --help` for every option.

## Commands

### `devx snapshot` and `devx diff`

Snapshots use a versioned JSON schema and are suitable for comparing a laptop, container, and remote server.

```bash
devx snapshot -o local.json
ssh server devx snapshot > server.json
devx diff local.json server.json
devx diff local.json server.json --only python,cuda
devx diff local.json server.json --ignore-host
devx diff local.json server.json --project --strict
```

For sharing, prefer:

```bash
devx snapshot --minimal --redact-paths -o shareable.json
```

`--minimal` omits process, disk, and shell-environment details. `--redact-paths` replaces home/project prefixes in captured paths and attribution text. Always review a snapshot before publishing it: hostnames, versions, project names, and selected environment values may still be sensitive.

### `devx disk`

The default analyzer identifies known caches and project build artifacts without deleting anything.

```console
$ devx disk --safe
Filesystem
/home/alice/projects/vision-lab

Used: 901.2 GB / 1.00 TB    90.1%
Potentially reclaimable    75.3 GB

SAFE
    61.1 GB   Conda package cache        /home/alice/miniconda3/pkgs
              Reason: downloaded and extracted Conda packages
    14.2 GB   pip cache                  /home/alice/.cache/pip
              Reason: package download and build cache

Read-only analysis; no files were deleted.
```

Useful reports:

```bash
devx disk --filesystems                 # every mounted filesystem
devx disk --project --top 20            # largest project entries
devx disk --safe                        # known cache locations
devx disk -C ~ --larger-than 5GB        # large files
devx disk -C ~ --older-than 365d        # old files
devx disk -C ~ --duplicates             # verified duplicate contents
devx disk -C ~ --duplicates --larger-than 100MB --csv
```

Duplicate detection groups by size before hashing, skips hard-link aliases, does not follow symlinks, and stays on the selected filesystem.

#### Interactive browser

```bash
devx disk --interactive -C ~
```

| Key | Action |
|---|---|
| `↑`/`↓`, `j`/`k` | Move selection |
| `Enter`, `l` | Open folder |
| `Backspace`, `h` | Parent folder |
| `/` | Search names |
| `f` | Cycle all/files/folders |
| `.` | Toggle hidden entries |
| `x` | Hide common generated folders |
| `s` | Sort by name/size |
| `i` | Show metadata and Git status |
| `c` | Cancel the active scan |
| `r` | Rescan |
| `d` | Review and move one item to system trash |
| `q`, `Esc` | Quit |

Sizes are measured asynchronously with at most four workers. The display uses synchronized terminal updates to avoid flicker. Deletion is never permanent: `d` requires explicit confirmation and moves exactly one revalidated direct child to the operating system trash.

### `devx ps`

Groups user-owned processes by the nearest project marker (`.git`, `Cargo.toml`, `pyproject.toml`, `package.json`, and others).

```console
$ devx ps
PROJECT                        CPU         RAM     GPU MEM  PORTS
──────────────────────────────────────────────────────────────────────────────
vision-lab                  830.2%     41.0 GB     22.4 GB  6006
├─ train.py
├─ torchrun
└─ dataloader × 8

11 processes in 1 project
```

```bash
devx ps my-project
devx ps --ports
devx ps --gpu
devx ps --tree
devx ps --watch --interval 1
devx ps --all
devx ps --snapshot processes.json
devx ps --csv
```

Watch mode adds bounded CPU history, CPU/RAM/GPU session peaks, and recent start/exit events. Process arguments and complete process environments are deliberately excluded to avoid capturing tokens and secrets.

## Export formats

| Command | Human | JSON | CSV |
|---|:---:|:---:|:---:|
| `snapshot` | — | ✓ | — |
| `diff` | ✓ | ✓ | — |
| `disk` | ✓ | ✓ | ✓ |
| `ps` | ✓ | ✓ | ✓ |

## Platform support

| Capability | Linux | macOS | Windows |
|---|:---:|:---:|:---:|
| Snapshot and diff | ✓ | unverified | unverified |
| Disk capacity and scanning | ✓ | unverified | unverified |
| Process grouping | ✓ | unverified | unverified |
| Listening-port discovery | ✓ | — | — |
| NVIDIA process memory | `nvidia-smi` | `nvidia-smi` | `nvidia-smi` |

## Development

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

Licensed under the [MIT License](LICENSE).
