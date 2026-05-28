# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2025-05-28

### Added
- Full container management: start, stop, restart, pause, unpause, remove (with force/volumes options), attach, exec shell
- Docker Compose support: project and service up/down, service start/stop/restart, aggregate logs, edit config in `$EDITOR`
- Real-time container log streaming with stdout/stderr color coding and follow/scroll modes
- Live CPU and memory stats with sparkline graphs
- Image, volume, and network browsing with prune support
- Mouse support (click to focus, scroll to navigate)
- Panel-specific filtering (`/`) and hide-stopped toggle (`e`)
- Horizontal scroll (`H`/`L`) and large scroll (`J`/`K`)
- Screen mode toggle (`+`/`_`) for compact/normal/expanded layouts
- Leader key system (`Space`) with confirmation for destructive actions
- Custom commands via `config.toml` (per-panel and global)
- Bulk command menus per panel
- Open in browser (`w`) for exposed container ports
- Theme customization (border, selection, status, error colors)
- Config file path resolution via env var, XDG, or dotfile
- Graceful Docker startup failure with background retry and backoff
- Panic-safe terminal restoration (raw mode, alternate screen, mouse)
- Memory-capped log buffer (line count + byte limit)
- ANSI escape hardening for safe log rendering
- Shell injection prevention for custom command templates

### Security
- `strip_ansi` hardened against malformed sequences (`ESC c`, DCS/APC/PM, `ESC #`, etc.)
- `CustomCommand::render` escapes substitutions with single quotes
- Safe image ID prefix handling (`strip_prefix("sha256:")` everywhere)
- Log partial line cap (10,000 chars) prevents OOM from newline-less containers
- URL validation in `open_browser` rejects non-HTTP(S) schemes
- Zero-width character preservation in text truncation

### Performance
- Eliminated per-frame log wrapping (`hard_wrap_line` moved to ingestion)
- Zero-allocation log buffer iteration when unfiltered
- `PanelContent` made generic over lifetime (`Cow<'a, str>` cells); ~100+ table cell allocations eliminated per frame
- `PanelContent::StyledText` made generic (`Text<'a>`); log line cloning eliminated (~30 clones/frame)
- Docker event-targeted refresh (only refreshes the resource type that emitted the event)
- `DockerFuture<'a, T>` alias cleans up complex trait bounds

[0.1.0]: https://github.com/binertia/erio/releases/tag/v0.1.0
