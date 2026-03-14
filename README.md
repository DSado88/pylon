# Pylon

A Metal-accelerated terminal emulator built for Claude Code. Track sessions, monitor usage, and manage everything from one window.

## Why Pylon?

If you use Claude Code heavily, you know the pain: multiple terminal windows, one for each session, with no way to see which is doing what. A separate TUI to check your token usage. Another tool to track active sessions. Tab-switching chaos.

Pylon puts it all in one place. A fast terminal with a built-in side panel that shows your active Claude sessions and real-time API usage across all your accounts. Click a session to jump to it. See at a glance who's working, who's idle, and how much capacity you have left.

## Features

**Terminal**
- Metal GPU-accelerated rendering with Retina-resolution glyph rasterization
- Pixel-perfect box-drawing characters (40+ rendered programmatically, not from fonts)
- Hack Nerd Font by default (same as Warp)
- Native macOS tabs (Cmd+T, Cmd+W, Cmd+1-9)
- Click-to-position cursor on the prompt line
- Solarized Dark theme

**Sessions Panel**
- Auto-discovers running Claude Code sessions via process scanning (every 5s)
- Shows status: green dot = idle, yellow dot = working (based on JSONL activity)
- Displays session topic, project folder, and session ID
- Click to switch tabs, bring windows to front, or unminimize
- Double-click to rename a session
- Correctly handles multiple sessions in the same directory

**Usage Panel**
- Tracks 5-hour and 7-day utilization across all your Claude accounts
- Color-coded progress bars (green/yellow/red)
- Live countdown timers until rate limit resets
- Polls the Anthropic API every 3 minutes
- Supports OAuth and session key authentication

**Polish**
- Dark title bar that matches the terminal theme
- Per-window sidebar width (drag to resize)
- Sidebar scrolling for long content
- Consistent rendering across Retina and non-Retina displays
- GPU frame synchronization to prevent scroll ghosting

## Install

```bash
# Clone and build
git clone https://github.com/DSado88/pylon.git
cd pylon
cargo build --release

# Install as macOS app
./install.sh
```

Then open **Pylon** from Spotlight, Launchpad, or `/Applications/Pylon.app`.

### Requirements

- macOS 14+
- Rust toolchain (`rustup`)
- [Hack Nerd Font](https://github.com/ryanoasis/nerd-fonts) (install via `brew install font-hack-nerd-font`)

## Usage Tracking Setup

Pylon reads account credentials from the same config as [claude-tracker](https://github.com/DSado88/claude-tracker). To set up usage monitoring:

1. Create the config file:

```toml
# ~/.config/claude-tracker/config.toml

[settings]
poll_interval_secs = 180

[[accounts]]
name = "your-email@example.com"
org_id = "your-org-uuid"
auth_method = "oauth"
```

2. Store your OAuth token in the macOS Keychain:

```bash
# If you have claude-tracker installed, use its login flow:
# Press 'L' in claude-tracker to authenticate via OAuth

# Or import from Claude Code's existing keychain entry:
# Press 'i' in claude-tracker to import
```

The `org_id` is your Anthropic organization UUID. The OAuth token is stored in the macOS Keychain under the `claude-tracker` service name.

Without this config, the Usage panel will show "Loading..." — the terminal works fine without it, you just won't see usage stats.

## Keyboard Shortcuts

| Shortcut | Action |
|----------|--------|
| Cmd+T | New tab |
| Cmd+W | Close tab |
| Cmd+N | New window |
| Cmd+1-9 | Switch to tab N |
| Cmd+B | Toggle sidebar |
| Cmd+Shift+B | Cycle sidebar panel (Sessions/Usage/Shortcuts) |
| Cmd+Shift+R | Rename tab |
| Cmd+Plus/Minus | Adjust font size |
| Cmd+0 | Reset font size |
| Cmd+C | Copy selection (or Ctrl+C if no selection) |
| Cmd+V | Paste (with bracketed paste support) |

## Architecture

Pylon is written in Rust with these key components:

- **GPU rendering**: Metal vertex/fragment shaders with a glyph texture atlas
- **VT parsing**: SIMD-accelerated via the `vte` crate with a fast ASCII path
- **PTY management**: Direct `fork`/`exec` with proper signal handling
- **Async polling**: Tokio runtime for session discovery and API queries
- **Native macOS integration**: `objc2` bindings for window appearance, tab management, and clipboard

The terminal grid uses a dirty-row bitfield to minimize GPU uploads — only changed rows are re-written each frame. Scrollback is stored in a ring buffer with configurable capacity (default 10,000 lines).

## License

MIT
