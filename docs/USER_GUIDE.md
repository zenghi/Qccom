# QCCOM User Guide

English · [简体中文](USER_GUIDE.zh-CN.md)

- [The window](#the-window)
- [Serial connection](#serial-connection)
- [Reading the log](#reading-the-log)
- [Sending data](#sending-data)
- [Quick commands](#quick-commands)
- [Terminal mode](#terminal-mode)
- [TCP, UDP and MQTT](#tcp-udp-and-mqtt)
- [Code viewer](#code-viewer)
- [Git viewer](#git-viewer)
- [Language](#language)
- [Updates](#updates)
- [Keyboard shortcuts](#keyboard-shortcuts)
- [Troubleshooting](#troubleshooting)

## The window

The thin **activity bar** on the far left switches between the three pages:

| Icon | Page |
|---|---|
| Pulse | **Debug**: connections, log, terminal, quick commands. A green dot means a connection is open. |
| `</>` | **Code**: browse source code next to the log. |
| Branch | **Git**: branches, commit graph and diffs. |

The **gear** at the bottom of the activity bar holds everything you need less often: the command palette, settings, keyboard shortcuts, theme, language, update check and About. A **download arrow** appears above it only while an update needs your attention.

### Command palette

Press `Ctrl+Shift+P` (or `F1`) and type a few letters of what you want, for example *theme*, *clear* or *shortcut*. Arrow keys move, Enter runs, Esc closes. Every command, theme, language and settings page is listed, and the ones you used recently come first.

### Settings

`Ctrl+,` opens the settings page: General, Appearance, Log, Connection, Code Editor, Keyboard Shortcuts and About. The search box at the top filters every option. Press Esc or click × to go back to where you were.

The Debug page has three columns:

- **Left sidebar** (`Ctrl+B` hides it): transport tabs (Serial / TCP / UDP / MQTT), device list, baud rate, the Connect button, a live link monitor (RX / TX bytes and a 60-second rate graph), display settings and advanced settings.
- **Center**: the top bar (Log / Terminal switch, search, level filter, pause, clear, copy, save, record), the log or terminal itself, and the command bar.
- **Right**: the quick-command panel (`Ctrl+J` shows or hides it).

The status bar at the bottom shows the connection, RX / TX activity lights, the encoding, the line count and the version. Click **Shortcuts** for the full key list. Click the version pill at the top of the sidebar to open the About box.

## Serial connection

1. **Choose a port.** Ports are rescanned every second. Each card shows the chip name and VID:PID, and the connected one is marked **LIVE**. A newly plugged device is selected for you.
2. **Choose a baud rate**, either by typing it or picking it from the list. If you don't know the rate, let the device print something and click **⚡ Auto**. QCCOM tries 18 common rates, scores the received data for readability and connects with the best one.
3. Click **Connect** or press **F5**. Press F5 again to disconnect.

If the device is unplugged while connected, QCCOM waits and reconnects when it comes back.

**Advanced settings** (bottom of the sidebar) cover data bits, parity, stop bits, flow control, DTR / RTS (many ESP32 and Arduino boards use these for reset), auto-reconnect, auto-select of new ports, the font size, the maximum number of lines kept, and the idle frame gap (for binary protocols, it starts a new line after N ms of silence).

A port that the scanner cannot see, such as a virtual `/dev/pts/3`, can be passed on the command line: `qccom /dev/pts/3`.

## Reading the log

- **Colors.** ANSI escape codes are rendered (16 / 256 / true color), so ESP-IDF, Zephyr and Linux kernel logs look as they would in a terminal. Lines are also colored by level: errors get a red tint and a red left bar, warnings get yellow, and level tags such as `[ERROR]`, `E (123) tag:`, `E/tag:`, `<err>`, `<wrn>` and `[INFO]` are highlighted.
- **Encoding.** UTF-8, GBK / GB2312, ASCII or Latin-1. Changing it re-decodes the whole history.
- **View.** Text, HEX, or Mixed (text with HEX below).
- **Display switches:** timestamp (milliseconds), line numbers, echo of sent data, ANSI colors, level colors, auto scroll.
- **Keywords.** Enter comma-separated words, and each gets its own highlight color.
- **Search** (`Ctrl+F`). All matches are highlighted; Enter and Shift+Enter jump between them. **Matches only** hides every other line.
- **Level filter.** The `ALL` / `ERR` / `WRN` / `INF` / `DBG` chips show live counts. Click one to show that level and above.
- **Pause** (`Ctrl+P`) freezes the view while data keeps arriving in the background, so nothing is lost.
- **Scrolling** up stops auto scroll. A **Back to latest** button appears to resume it.
- **Selecting.** Drag to select any text, double-click for a word, triple-click for a whole line. Click the line-number gutter to select whole lines (Shift+click for a range). Dragging past the top or bottom edge scrolls. `Ctrl+C` copies without timestamps by default; turn on *Include timestamps when copying* in Settings → Log, or right-click and pick **Copy with timestamps**.
- **Save** (`Ctrl+S`) writes the current log to a file. **Copy** puts it on the clipboard. **Record** appends every new line to a file as it arrives, which is ideal for overnight captures.
- **Clear** with `Ctrl+L`.

## Sending data

Type in the command bar at the bottom (`Ctrl+K` focuses it) and press **Enter**.

- **↑ / ↓** browse the command history, which is saved.
- **Line ending:** none, LF, CR or CRLF.
- **HEX:** `AA 55 01`, `0xAA,0x55`, and `AA5501` are all accepted.
- **Repeat** sends the same data every N ms until you turn it off.

Sent text is encoded with the encoding selected in the sidebar.

## Quick commands

The right-hand panel holds groups of one-click commands. Three example groups are included: AT commands, Shell, and HEX samples.

- Click a command card to send it. `Alt+1` … `Alt+9` send the first nine commands in the current group.
- Click **+** to add a group. Double-click a group name to rename it, and right-click it to duplicate or delete it.
- **+ Add command** creates a command. Right-click a command to edit, move or delete it. Each command can be text or HEX and can have its own line ending, or follow the command bar's setting.
- **Send whole group** sends every command in order, with the interval shown next to it.

Everything is saved automatically.

## Terminal mode

Switch to **Terminal** in the top bar or press `Ctrl+\``. Click inside the terminal and your keyboard goes straight to the device, just like PuTTY or minicom.

- Every key is sent through: Ctrl+C, Tab completion, arrow-key history, Ctrl+R, Ctrl+D, F1–F12, Home / End / PgUp / PgDn. The only exceptions are F5 and `Ctrl+\``.
- It supports VT100 / xterm sequences: cursor movement, erase, 16 / 256 / true color, scroll regions and the alternate screen. Full-screen programs such as vi, top, less and menuconfig work and restore the screen when they exit.
- CJK characters take two cells. Box-drawing and block characters are drawn as vectors, so frames line up exactly.
- Drag to select text, which is copied when you release the mouse. `Ctrl+Shift+C` copies and `Ctrl+V` pastes. Right-click to copy the screen, paste or reset.
- The mouse wheel scrolls back through the last 3000 lines. Press any key or click **Back to latest** to return.
- **Sync window size** sends `stty rows R cols C` so programs on the device use the whole window. Run it at a shell prompt.
- The log and the terminal share the same connection, so you can switch at any time without losing data. Quick commands work in terminal mode too.

The terminal always decodes UTF-8, which is what Linux boards use.

## TCP, UDP and MQTT

Pick a tab above the device list. All log, terminal, quick-command and recording features work on network connections too.

| Mode | Settings | Notes |
|---|---|---|
| **TCP client** | host, port | Connects to a server such as a Wi-Fi module or a test rig. |
| **TCP server** | listen port | Accepts any number of clients. Data you send goes to all of them, and the sidebar shows how many are connected. |
| **UDP** | local port, remote host and port | If you leave the remote host empty, replies go to whoever sent the last packet. Broadcast addresses are allowed. Each received datagram becomes a log entry tagged with the sender's address. |
| **MQTT** | broker host and port, TLS, client ID, user name, password, keep-alive, clean session | MQTT 3.1.1. Add any number of subscriptions (wildcards `+` and `#` are fine). Choose a publish topic, QoS 0 / 1 / 2 and retain, then type in the command bar to publish. Incoming messages are shown with their topic. |

TLS uses the operating system's certificate store on Windows and the Mozilla root store on Linux.

## Code viewer

Click `</>` in the activity bar, then **Open folder** to open your firmware project. QCCOM remembers it, along with your open tabs.

- **Explorer** (`Ctrl+Shift+E`): the file tree with Git status colors (M modified, U untracked), a filter box, refresh and collapse-all. Symbol indexing and search skip folders such as `.git`, `node_modules`, `target` and `dist`.
- **Tabs**: `Ctrl+Tab` switches tabs and `Ctrl+W` closes one. Middle-click also closes a tab.
- **Highlighting** for C / C++, Rust, Python, JavaScript / TypeScript, Go, Java / Kotlin, shell, JSON, Makefile / CMake, INI / TOML / YAML, Markdown and assembly.
- **Quick open** (`Ctrl+P`): type part of a file name. You can also prefix the query:
  - `#` searches symbols across the workspace (`Ctrl+T`).
  - `@` lists the symbols in the current file (`Ctrl+Shift+O`).
  - `:` goes to a line (`Ctrl+G`).
- **Search everywhere** (`Ctrl+Shift+F`): full-text search across the project with a case-sensitive option. Click a result to jump to it.
- **Find in file** (`Ctrl+F`).
- **C / C++ navigation**: hover an identifier to see its definition. Hovering a macro shows the `#define`, the full expansion and the computed value (for example `BRR_VALUE` → `((72 * 1000 * 1000) / 115200)` → `0x271 (625)`). Hovering an enum constant shows its value. `Ctrl+click` or `F12` goes to the definition, `Shift+F12` finds references, and `Alt+←` / `Alt+→` go back and forward.
- **Editing**: files open read-only so you can't change them by accident. Click the pencil icon to enable editing, then `Ctrl+S` saves.
- `Ctrl+=` and `Ctrl+-` change the font size. A minimap is on the right.

## Git viewer

Click the branch icon in the activity bar. It opens the repository that contains the code-viewer folder, or you can pick another one. It needs `git` to be installed and in your `PATH`.

- **Branch card**: the current branch, its upstream, ahead / behind counts and the number of uncommitted changes. The buttons fetch from all remotes, refresh, and open a different repository.
- **Working tree changes**: staged, unstaged and untracked files. Click one to see its diff.
- **Commits on all branches**: a commit graph with colored lanes, branch and tag labels, author, relative time and short hash. Use ↑ / ↓ to move, and scroll to load more history. The search box filters by commit message (case-insensitive).
- **Branches, remotes and tags** in the sidebar: click one to show only its history, double-click to check it out, and right-click for more actions. Checking out a remote branch creates a local tracking branch. Checking out a tag (or a commit, from its context menu) gives a detached HEAD. QCCOM warns you first when you have uncommitted changes and explains in plain language why a checkout failed.
- **Commit details**: message, author, date, parent links and changed files with add / delete bars. Click a file for a line-numbered, highlighted diff. Double-click a diff line to open that file at that line in the code viewer.

## Theme

Dark, Light or **System default** (follows the operating system and switches live). Change it in Settings → Appearance, from the gear menu, or with `Ctrl+K Ctrl+T`. Every color, including popups, code highlighting and log levels, is tuned for at least 4.5:1 contrast in both themes. Settings → Appearance also has a UI scale for high-DPI screens.

## Language

QCCOM follows the system language on first start. To change it, open the gear menu → Language, or search *language* in the command palette. Choosing **Follow system** returns to automatic detection. The change takes effect immediately.

Supported: English, 简体中文, 繁體中文, 日本語, 한국어, Deutsch, Français, Español, Русский, Português.

## Updates

A few seconds after start, QCCOM checks for a newer version. If one exists, a card in the bottom-right corner shows the version, date and release notes, with **Update now**, **Later** and **Skip this version**. After the download is verified, **Restart and update** replaces the program and starts the new version. Your settings are kept.

- Gear menu → **Check for updates** checks at any time.
- Settings → General, or the switch on the card, turns the automatic check off.
- On Linux, an update is downloaded automatically only if the publisher provides a Linux build. Otherwise **Open download page** takes you to the Releases page.

Publishers: see [UPDATE_SERVER.md](UPDATE_SERVER.md).

## Keyboard shortcuts

Every shortcut can be changed in Settings → Keyboard Shortcuts (`Ctrl+K Ctrl+S`): click a key to record a new one, right-click to reset or remove, and add more than one binding per command. Two combos pressed in a row make a sequence such as `Ctrl+K Ctrl+T`; the status bar shows the first half while it waits. Conflicts are flagged, and the whole set can be exported to or imported from a JSON file. The tables below list the defaults.

**Everywhere**

| Keys | Action |
|---|---|
| Ctrl+Shift+P / F1 | Command palette |
| Ctrl+, | Settings |
| Ctrl+K Ctrl+S | Keyboard shortcuts |
| Ctrl+K Ctrl+T | Toggle dark / light theme |
| Ctrl+Shift+D / E / G | Debug / Code / Git page |

**Debug page**

| Keys | Action |
|---|---|
| F5 | Connect / disconnect |
| Ctrl+F | Search the log |
| Enter / Shift+Enter | Next / previous match |
| Ctrl+L | Clear |
| Ctrl+S | Save the log |
| Ctrl+P | Pause / resume |
| Ctrl+K | Focus the command bar |
| Ctrl+B | Show / hide the sidebar |
| Ctrl+J | Show / hide quick commands |
| Alt+1 … 9 | Send quick command 1–9 of the current group |
| Ctrl+A | Select the whole log |
| Ctrl+C | Copy the selection |
| Esc | Clear the selection |
| Ctrl+\` | Switch between Log and Terminal |

**Code viewer**

| Keys | Action |
|---|---|
| Ctrl+P | Quick open |
| Ctrl+T | Go to a symbol in the workspace |
| Ctrl+Shift+O | Go to a symbol in the file |
| Ctrl+G | Go to a line |
| Ctrl+O | Open a file |
| Ctrl+Shift+F | Search everywhere |
| Ctrl+Shift+E | Show the explorer |
| Ctrl+F | Find in the file |
| F12 / Ctrl+click | Go to definition |
| Shift+F12 | Find references |
| Alt+← / Alt+→ | Back / forward |
| Ctrl+Tab / Ctrl+W | Next tab / close tab |
| Ctrl+S | Save (when editing is enabled) |
| Ctrl+= / Ctrl+- | Font size |

On macOS-style keyboards, Ctrl may be shown as ⌘. QCCOM uses the platform's command key.

## Troubleshooting

**No ports on Linux, or "Permission denied".** Add yourself to the `dialout` group (`uucp` on Arch): `sudo usermod -aG dialout $USER`, then log out and back in. Some distributions also need `brltty` removed for CH340 adapters.

**No ports on Windows.** Install the driver for your USB-serial chip (CH340, CP210x, FTDI or PL2303) and check Device Manager.

**Garbled text.** Try **⚡ Auto** for the baud rate, or switch the encoding (GBK for many Chinese-language firmwares).

**Windows SmartScreen warning.** The executable is not code-signed yet. Click **More info → Run anyway**, or unblock the file in its Properties.

**The Git page says Git was not found.** Install Git from https://git-scm.com and restart QCCOM.

**Update failed: no permission to replace the program.** The executable is in a protected folder such as `Program Files`. Move it to a folder you own, or run the new version manually.

**Where are my settings?** On Windows they are in `%APPDATA%\qccom\data\app.ron`, and on Linux in `~/.local/share/qccom/app.ron`. Delete the file to reset everything.

Found a bug? Please open an issue at https://github.com/zenghi/QCCOM/issues.
