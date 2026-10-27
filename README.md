<p align="center">
  <img src="docs/logo.png" width="72" alt="QCCOM logo">
</p>

<h1 align="center">QCCOM</h1>

<p align="center">
  A fast, lightweight serial / TCP / UDP / MQTT debug tool for embedded developers,<br>
  with a built-in terminal, code viewer and Git history browser.
</p>

<p align="center">
  <a href="https://github.com/zenghi/QCCOM/releases"><img alt="Release" src="https://img.shields.io/github/v/release/zenghi/QCCOM?color=2dd4bf"></a>
  <a href="https://github.com/zenghi/QCCOM/actions"><img alt="Build" src="https://img.shields.io/github/actions/workflow/status/zenghi/QCCOM/build.yml?branch=main"></a>
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-blue"></a>
  <img alt="Platforms" src="https://img.shields.io/badge/platform-Windows%20%7C%20Linux-555">
</p>

<p align="center">
  English · <a href="README.zh-CN.md">简体中文</a>
</p>

![QCCOM log view](docs/screenshot.png)

QCCOM is a single portable executable (about 4 MB on Windows) written in Rust with [egui](https://github.com/emilk/egui). No installer, no runtime, no admin rights. It starts in well under a second and stays smooth with hundreds of thousands of log lines.

## Highlights

- **Serial ports that just work.** Ports are rescanned every second and shown with their chip name and VID:PID (`COM5 · CH340 (1a86:7523)`). A newly plugged device is selected for you, and an unplugged one is reconnected automatically when it comes back.
- **Automatic baud-rate detection.** Tries 18 common rates, scores the received text for readability and connects with the best one.
- **Linux-terminal style logs.** ANSI colors (16 / 256 / true color), automatic level coloring for `[ERROR]`, `E (123) tag:`, `E/tag:`, `<err>` and similar formats, millisecond timestamps, keyword highlighting, search, level filter, text / HEX / mixed views.
- **Encodings:** UTF-8, GBK / GB2312, ASCII and Latin-1. Switching re-decodes the existing history instantly.
- **Full terminal mode.** VT100 / xterm emulation for U-Boot and Linux shells. vi, top, less and menuconfig all render correctly, with scrollback, mouse selection and copy / paste.
- **Network transports.** TCP client, TCP server (multiple clients), UDP and MQTT 3.1.1 (TLS, user / password, QoS 0–2, retain, multiple subscriptions). Every log, terminal and quick-command feature works the same over the network.
- **Quick commands.** Groups of one-click commands (text or HEX) with per-command line endings, `Alt+1…9` hotkeys and "send the whole group" sequencing.
- **Debugging tools.** Live plots of numbers in the log, filter / highlight rules, binary protocol framing with checksum checks, auto-replies, [Rhai](https://rhai.rs) test scripts, a Modbus RTU / TCP master, XMODEM / YMODEM file transfer, CRC on send, an AT command assistant, multi-line sending, log replay with the original timing, statistics, markers and bookmarks.
- **Serial extras.** DTR / RTS / Break and live CTS / DSR / RI / CD, one-click reset (including the ESP32 bootloader sequence), sharing a port over TCP, a second sniffing port, port aliases, profiles and multiple windows.
- **Code viewer.** Open your firmware folder next to the log: file tree, tabs, syntax highlighting for 12 language families, quick open, workspace-wide search, and for C / C++ go to definition, find references and macro expansion with the computed value.
- **A real code editor.** Multiple cursors, column selection, folding, bracket matching, sticky scroll, line commands, and Source Insight-style context window, call hierarchy, bookmarks and semantic colors. Uses clangd automatically when it is installed and the project has a `compile_commands.json`.
- **Built-in terminal, find / replace, split view.** A terminal panel (PowerShell, cmd, Git Bash, bash…) with clickable `file:line`, regex find and replace in a file or across the project with preview, split editor, Git change bars and inline blame, auto-reload, crash recovery and portable mode.
- **Firmware tools.** Map / ELF / HEX / BIN viewers with memory-usage bars and a byte inspector, text and binary file compare, a bit calculator with field extraction, number hovers, code snippets, a TODO panel, Kconfig / device tree / linker-script highlighting and optional Vim keys.
- **Git viewer.** Branches, tags and remotes, a commit graph across all branches, commit details, line-numbered diffs, working-tree status, checkout and fetch.
- **Recording and export.** Save or copy the log, or record every new line to a file (split by size or time) for long unattended captures.
- **Self-update** from your own server, with SHA-256 verification. See [docs/UPDATE_SERVER.md](docs/UPDATE_SERVER.md).
- **Dark and light themes** (or follow the system), a searchable **command palette** (`Ctrl+Shift+P`), a settings page, and **fully customizable keyboard shortcuts** including two-step sequences.
- **10 UI languages**, picked automatically from the system locale: English, 简体中文, 繁體中文, 日本語, 한국어, Deutsch, Français, Español, Русский, Português.

| Debugging tools | Firmware tools |
|---|---|
| ![Plot, rules and auto-reply](docs/screenshot-debug-tools.png) | ![Map viewer and bit calculator](docs/screenshot-embedded.png) |

| Code viewer | Git history |
|---|---|
| ![Code viewer](docs/screenshot-code.png) | ![Git viewer](docs/screenshot-git.png) |

| Terminal mode | Light theme |
|---|---|
| ![Terminal](docs/screenshot-terminal.png) | ![Light theme](docs/screenshot-light.png) |

## Download

Grab the latest build from the [Releases page](https://github.com/zenghi/QCCOM/releases).

| Platform | File | Notes |
|---|---|---|
| Windows 10 / 11 x64 | `qccom-windows-x64.exe` | Double-click to run. Install the USB-serial driver for your adapter (CH340, CP210x, FTDI…) if Windows does not have it. |
| Linux x64 | `qccom-linux-x64` | `chmod +x qccom-linux-x64 && ./qccom-linux-x64`. To open serial ports without root, add yourself to the `dialout` group (`uucp` on Arch): `sudo usermod -aG dialout $USER`, then log out and back in. |

Each release also ships a `SHA256SUMS.txt` so you can verify the download.

> **Windows SmartScreen** may warn that the app is unrecognized because the executable is not code-signed yet. Click **More info → Run anyway**, or right-click the file → **Properties** → tick **Unblock**.

## Quick start

1. Pick a port in the left sidebar.
2. Click **Connect** or press **F5**. If you do not know the baud rate, click **⚡ Auto** while the device is printing.
3. Read the log. Type a command at the bottom and press **Enter** to send it.

To talk to a shell interactively (like PuTTY or minicom), switch to **Terminal** in the top bar or press **Ctrl+\`**. For TCP, UDP or MQTT, pick the tab above the device list.

The full manual is in [docs/USER_GUIDE.md](docs/USER_GUIDE.md).

## Build from source

You need a recent stable Rust toolchain (`rustup`, Rust 1.81 or newer).

```bash
git clone https://github.com/zenghi/QCCOM.git
cd QCCOM

# Linux only: system headers for serial-port enumeration
sudo apt install pkg-config libudev-dev      # Debian / Ubuntu
# sudo dnf install pkgconf-pkg-config systemd-devel   # Fedora
# sudo pacman -S pkgconf systemd-libs                 # Arch

cargo build --release
# -> target/release/qccom  (qccom.exe on Windows)

cargo test
```

Cross-compiling the Windows build from Linux, release profiles, CI and other details are in [docs/BUILDING.md](docs/BUILDING.md).

## Project layout

| Path | What it does |
|---|---|
| `src/main.rs` | Entry point, window setup, settings migration |
| `src/app.rs` | Main window: sidebar, log view, command bar, activity bar, update card |
| `src/serial.rs` | Port scanning, I/O thread, auto-baud detection |
| `src/net.rs` | TCP client / server, UDP, a small MQTT 3.1.1 client |
| `src/logbuf.rs` | Line splitting, decoding, ANSI parsing, log-level detection |
| `src/term.rs` | VT100 / xterm terminal emulator |
| `src/cmds.rs` | Quick-command panel |
| `src/app/logx.rs`, `src/app/plot.rs` | Log rules, replay, statistics, protocol framing UI; live plot |
| `src/app/dock.rs`, `src/script.rs` | Auto-reply and script panels; Rhai script runner |
| `src/app/dbgtools.rs`, `src/modbus.rs`, `src/xfer.rs`, `src/checksum.rs` | Modbus master, XMODEM / YMODEM, checksum calculator |
| `src/app/serialx.rs`, `src/bridge.rs`, `src/atcmds.rs` | Signal lines, reset, port B, aliases, profiles; serial-to-TCP bridge; AT command list |
| `src/codeview.rs`, `src/cindex.rs`, `src/highlight.rs` | Code viewer, C symbol index, syntax highlighter |
| `src/editor.rs`, `src/codeview/insight.rs`, `src/lsp.rs` | Text editor widget, context window / call hierarchy, optional clangd client |
| `src/shell.rs`, `src/termview.rs`, `src/pattern.rs` | Terminal panel (pseudo-terminals), shared terminal widget, find / replace patterns |
| `src/binview.rs`, `src/binview/parse.rs`, `src/tools.rs` | Map / ELF / HEX / BIN viewers and parsers, file compare, bit calculator |
| `src/snippets.rs`, `src/vim.rs` | Code snippets, Vim key emulation |
| `src/gitview.rs`, `src/git.rs` | Git viewer UI and the `git` CLI wrapper |
| `src/update.rs` | Self-update (version check, download, verify, replace) |
| `src/i18n.rs`, `assets/i18n.tsv` | Translations |
| `src/theme.rs` | Dark / light palettes, fonts |
| `src/keymap.rs` | Commands, default shortcuts, user overrides |
| `src/app/palette.rs`, `src/app/prefs.rs` | Command palette, settings page and shortcut editor |
| `assets/` | App icon, bundled fonts (Inter, JetBrains Mono, SIL OFL 1.1) |
| `build.rs` | Embeds the icon and version info into the Windows executable |

Source comments are mostly in Chinese. Identifiers and the public docs are in English.

## Contributing

Bug reports, feature ideas, translations and pull requests are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) first. Adding or fixing a UI language only needs edits to one TSV file.

## License and attribution

QCCOM is licensed under the [Apache License 2.0](LICENSE). Copyright © 2026 [zenghi](https://github.com/zenghi).

You may use, modify and redistribute it, including commercially. If you redistribute QCCOM or a work based on it, the license requires you to:

- include a copy of the [LICENSE](LICENSE),
- include the [NOTICE](NOTICE) file, which carries the attribution **"Based on QCCOM by zenghi – https://github.com/zenghi/QCCOM"**,
- keep existing copyright and attribution notices, and mark files you changed.

Please do not remove the author and repository information shown in the app's About box.

Bundled fonts: Inter and JetBrains Mono, both under the SIL Open Font License 1.1 (see [assets/licenses](assets/licenses)).
