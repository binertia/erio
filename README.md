# erio

[![Rust](https://img.shields.io/badge/rust-2024 edition-blue.svg)](https://www.rust-lang.org)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

A terminal UI for Docker, written in Rust. Inspired by [lazydocker](https://github.com/jesseduffield/lazydocker).

**erio** lets you view containers, images, volumes, networks, and Docker Compose projects — all from a fast, keyboard-driven terminal interface with real-time log streaming and resource monitoring.

---

## Features

- **Container Management** — Start, stop, restart, pause, remove, attach, and exec into containers
- **Docker Compose Support** — Browse projects and services; up/down individual services or entire projects
- **Real-time Logs** — Stream container logs with stdout/stderr color coding; follow or scroll
- **Resource Monitoring** — Live CPU and memory usage graphs
- **Interactive TUI** — Mouse support, panel filtering, horizontal scrolling, and multiple layout modes
- **Custom Commands** — Define your own per-panel commands via config
- **Safe & Robust** — Graceful handling of Docker disconnections, panic-safe terminal restoration, ANSI-hardened log rendering

---

## Installation

### From Source

```bash
git clone https://github.com/binertia/erio
cd erio
cargo build --release
```

The binary will be at `target/release/erio`.

### Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) 1.85+
- Docker daemon (local or remote via `DOCKER_HOST`)

---

## Quick Start

```bash
# Run the application
cargo run

# Run with a custom config file
ERIO_CONFIG=~/.config/erio/config.toml cargo run
```

---

## Keybindings

### Navigation
| Key | Action |
|-----|--------|
| `Tab` / `]` | Focus next panel |
| `Shift+Tab` / `[` | Focus previous panel |
| `1`–`6` | Jump to Projects / Services / Containers / Images / Volumes / Networks |
| `j` / `k` or `↓` / `↑` | Move selection down / up |
| `J` / `K` | Scroll main panel by larger increments |
| `H` / `L` | Horizontal scroll |
| `g` / `G` | Scroll to top / bottom |
| `+` / `_` | Next / previous screen layout mode |

### Container Actions (Containers panel)
| Key | Action |
|-----|--------|
| `u` | Start container |
| `s` | Stop container |
| `r` | Restart container |
| `p` | Pause / unpause |
| `d` | Remove menu (with force/volume options) |
| `a` | Attach to container |
| `E` | Exec shell |
| `w` | Open exposed port in browser |
| `c` | Custom commands (from config) |
| `b` | Bulk commands |
| `x` | Options menu |

### Compose Actions
| Key | Action |
|-----|--------|
| `U` / `D` | Project up / down |
| `u` / `d` | Service up / down |
| `S` | Start service |
| `m` | View project/service logs |
| `e` | Edit `docker-compose.yml` in `$EDITOR` |

### Global
| Key | Action |
|-----|--------|
| `Space` | Leader key (prefix for actions) |
| `/` | Search / filter current panel |
| `e` | Toggle hide stopped containers |
| `?` | Help |
| `q` | Quit |

> Press `Space` then any action key for leader-mode confirmation on destructive actions.

---

## Configuration

erio looks for config in the following order:

1. `ERIO_CONFIG` environment variable
2. `~/.config/erio/config.toml`
3. `~/.erio/config.toml`

### Example `config.toml`

```toml
app_name = "erio"
confirm_on_quit = true
scroll_past_bottom = true
log_buffer_lines = 5000
log_buffer_max_bytes = 1_000_000

[theme]
border_color = "blue"
selection_color = "cyan"
status_color = "green"
error_color = "red"

[custom_commands.containers]
name = "Shell (bash)"
command = "docker exec -it {{ .Container.ID }} bash"
```

See `src/config.rs` for the full config schema and defaults.

---

## Development

```bash
# Run unit tests
cargo test --lib

# Run integration tests (requires Docker)
ERIO_DOCKER_TESTS=1 cargo test

# Run with logging
RUST_LOG=debug cargo run
```

### Project Structure

- `src/app.rs` — Application loop, event handling, action dispatch
- `src/docker.rs` — Docker client, supervisor, background event streaming
- `src/state.rs` — Centralized state with filtered views and selection management
- `src/ui/` — TUI layout, panels, input mapping, rendering
- `src/config.rs` — Configuration loading and defaults
- `src/events.rs` — Async event bus

---

## License

[MIT](LICENSE)
