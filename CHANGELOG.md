# Changelog

## 1.4.0

The network tools move into the Toolbox, and every connection can be open at the same time.

- **All connections at once**: the serial port, TCP, UDP, MQTT, BLE and WebSocket each keep their own connection, log, terminal, statistics and recording. Background connections keep receiving and reconnecting; the status bar lists every live connection (a dot flashes on new data) and a click jumps to it.
- **Toolbox reorganized**: a *Communication* group at the top with TCP (client / server), UDP, MQTT, Bluetooth LE, WebSocket, HTTP API and Modbus slave; the Debug page is now just the serial port. Search the tool list (Enter opens the first match), right-click a tool to pin it to *Favorites*, and open any tool from the command palette.
- **WebSocket client**: `ws://` and `wss://`, text or binary frames, keep-alive pings, extra headers (e.g. `Authorization`).
- **Modbus slave**: simulate a Modbus TCP or RTU device; edit holding / input registers, coils and discrete inputs in a grid and see every request from the master.
- **MQTT broker**: run a broker on this computer (MQTT 3.1.1, wildcards, retained messages, last will, optional user name / password). Lists clients and their subscriptions, shows every message, publishes from the page, and connects the MQTT tool to it with one click.
- **Syslog receiver**: RFC 3164 / 5424 over UDP with severity colors, host and program filters.
- **Ping / ports**: continuous ping with a latency chart and loss rate; TCP port check over lists and ranges.
- **Subnet calculator**, **timestamp converter**, **regex tester** (highlighting, groups, replace), **color converter** (RGB888 / RGB565 / HSV), **ASCII table**.
- **Checksums**: SUM / XOR / LRC and 14 CRC presets at once, plus a fully custom CRC (width, polynomial, init, reflect in / out, xor out).
- **Font & image to C**: text in any TTF / TTC font or an image (PNG / JPG / BMP / GIF) to a monochrome bitmap or RGB565 / RGB888 / gray array, with row, column and page (SSD1306) scan modes, bit order, inverse, dithering and a live preview.

## 1.3.0

Code editor upgrades for reading and writing firmware, and one shared project folder.

- **Shared project folder**: the Code page, Git page, terminal panel and the TFTP / HTTP servers now follow the same project. Switching projects remembers each one's open tabs, cursor positions and bookmarks. *Open project folder* is in the command palette.
- **Navigation**: navigation history (`Ctrl+Alt+-`, or type `<` in quick open), last edit location (`Ctrl+Q`), header / source switch (`Alt+O`), matching `#if / #else / #endif` (`Ctrl+Alt+]`), previous / next function (`Alt+PageUp / PageDown`), go to declaration (`Ctrl+F12`), open the definition to the side (`Ctrl+K F12`). Mouse back / forward buttons work.
- **Highlights**: `F8` marks the word under the cursor in one of 8 colors (press again for the next color), `Shift+F8` removes it, `Ctrl+Shift+F8` clears all, `F3 / Shift+F3` jump between marks. Marks also show on the scrollbar.
- **Inactive code**: `#if 0`, `#ifdef` of undefined macros and the dead branch of `#if / #else` are greyed out, using the project's `#define`s plus extra macros you can add in settings. The current function body gets a faint background.
- **Comments**: `Ctrl+Shift+/` toggles a block comment, `Ctrl+Alt+/` wraps the selection in `#if 0` (or unwraps it). Typing `/**` and Enter above a function writes a Doxygen comment with its parameters.
- **Macro actions**: a right-click *Code generation* menu and 16 customizable actions: `#ifdef` / `#if-#else` wrap, printf of the variable under the cursor with a format matched to its type (`%u`, `%ld`, `%p`, `%f`…), hex dump, Doxygen function comment, file header, include guard + `extern "C"`, TODO, separator. Templates use `${SELECTION}`, `${WORD}`, `${FUNC}`, `${FILE}`, `${DATE}`, `${AUTHOR}`, `${INPUT:…}` and more, and are edited in Settings → Code Editor. *Remove debug prints* deletes every line they inserted.
- **More editing**: enum → `switch`, add the declaration to the header (or a stub to the source), align on `=` / `,`, cycle naming style (`Ctrl+Alt+U`), new C / H files get a file header.
- **Key recording**: `Shift+Alt+R` records, `Shift+Alt+P` plays back, and a recording can be saved as a named macro.
- **Keymap schemes**: QCCOM / VS Code / Source Insight / Keil in Settings → Keyboard Shortcuts; your own bindings stay on top.
- **Context window**: click a symbol in it to preview, double-click to jump; a pin keeps the content. Call hierarchy entries preview on click and jump on double-click.

## 1.2.1

- Animated demos in the README and user guide (`docs/gif/`).
- `qccom <folder>` opens that folder as a project on the Code page.
- The log view draws the line-drawing characters used by `top`, `dialog` and similar programs instead of `lqqk`.
- API tab: a query typed into the URL (`?device=1`) is moved into the Params table, so it is no longer sent twice when the same parameter is already there.

## 1.2.0

A **Toolbox** page in the activity bar (`Ctrl+Shift+B`), with the small utilities embedded work keeps needing:

- **TFTP server**: share a folder for U-Boot, switches and IP phones (RFC 1350 with blksize / tsize / timeout options, optional uploads). Shows the U-Boot command to copy.
- **HTTP file server**: directory listing, resumable downloads (Range) for OTA tests, optional PUT uploads, a live request log.
- **Device discovery**: mDNS query plus a scan of common ports (SSH, Telnet, HTTP, Modbus, MQTT, ADB, ESPHome…) on the local /24. Click a port to open it in the API, MQTT or TCP tab.
- **Data converter**: HEX, text (UTF-8 / GBK with `\r \n \xNN` escapes), Base64, C arrays, and every integer / float interpretation in little and big endian.
- **Baud calculator**: divider register values and real error for STM32 (OVER16 / OVER8), AVR (U2X), 16550 and ESP32, plus the error of common baud rates at a given clock.
- **CAN**: SLCAN adapters (CANable, USBtin…): bitrate, listen-only, send and repeat frames, a per-ID view with period and changed-byte highlighting, and a chronological trace.
- **USB HID**: list HID devices, receive input reports, send output reports, get / set feature reports.

## 1.1.0

Two new tabs next to MQTT in the sidebar.

- **BLE**: scan nearby Bluetooth LE devices (name, address, service / manufacturer hint, signal bars), connect, browse services and characteristics, subscribe to notifications, read values, pick the write target and send from the command bar like any other connection. Nordic UART, HM-10 and similar modules work out of the box. Built on btleplug (WinRT on Windows, BlueZ on Linux).
- **API**: a Postman-style HTTP client: methods, params, headers, JSON / form / text bodies, Bearer / Basic auth, `{{variables}}`, saved requests and history, colored JSON responses, response headers, cURL import (paste into the URL bar) and export. The serial or network connection keeps logging in the background.
- The sidebar keeps the same width on every tab.

## 1.0.0

Debugging tools. Everything rarely used sits in the new **⋯** menu, the command palette or the right-hand panel, so the main window looks the same as before.

- **Plot** (`Ctrl+Shift+W`): numbers in the log (`temp=25.3`, `vbat: 3.71`, CSV lines, or your own regex) drawn as live curves above the log. Constant values are listed but not drawn; zoom with the wheel, hover for values, freeze, resize.
- **Filter & highlight rules** (`Ctrl+Shift+H`): text or regex rules that highlight, show only or hide lines.
- **Protocol framing**: split binary data by header + fixed length or length field, check SUM / XOR / CRC-8 / CRC-16 / CRC-32 and show each frame in colored HEX; bad frames turn red.
- **Auto-reply** rules (right panel → Replies): reply with text or HEX after a delay, notify, pause, or run a script when a line matches.
- **Scripts** (right panel → Scripts): Rhai scripts with `send`, `wait`, `wait_re`, `read_line`, `sleep`, `dtr`, `rts`…
- **Modbus master**: RTU on serial, Modbus TCP on TCP; read / write coils and registers, 16 / 32-bit and float display, polling.
- **Send file** with XMODEM, XMODEM-1K and YMODEM.
- **Checksum calculator**, and HEX sending can append a checksum automatically.
- **AT command assistant**: suggestions with descriptions while you type `AT…`, `Tab` to complete.
- **Multi-line sending**: whole block or line by line with a gap.
- **Open and replay log files** with the original timing (0.5×–10× or as fast as possible); drag a log into the window, or `qccom --open` / `--replay`.
- **Statistics** window, **time delta** column, **markers** (`Ctrl+M`) and **log bookmarks** (`Ctrl+F2`, `F2`, `Shift+F2`), *Copy as HEX* and *New auto-reply rule from this line* in the context menu.
- **Serial extras**: DTR / RTS toggles and live CTS / DSR / RI / CD, reset pulses including the ESP32 bootloader sequence, Break, **share the port over TCP**, a second **sniffing port B**, port aliases.
- **Profiles**: save and switch connection settings, commands, rules, replies and scripts together.
- **New window** (`Ctrl+Shift+N`): run several QCCOM windows, each with its own settings.
- Recording can start a new file every N minutes or N MB.
- Files dropped on the code page open in the editor.

## 0.9.0

- Firmware files open in dedicated viewers (toggle back to the source with the pill in the breadcrumb bar):
  - **Map files** (GNU ld): Flash / RAM usage bars per memory region, sortable and filterable tables of symbols, object files and output sections. Click a symbol to jump to it.
  - **ELF files**: text / data / bss totals, sections, symbols and program headers.
  - **HEX / BIN / S-record**: hex grid with a byte inspector (u8…u64, float, LE / BE), go to address, sum of a selection, export to BIN.
- Compare two files: `Compare with…` on a tab, or *Select for Compare* in the explorer. Text files get a side-by-side diff with an overview bar (`F7` / `Shift+F7`); binaries get a byte-level compare.
- Hover a number in code to see it in hex, decimal, binary and its set bits. `Ctrl+click` opens it in the new **bit calculator**: type a number, C expression or macro, toggle bits, drag to select a field and get its read expression, mask and a new value.
- Code snippets: type a prefix and press `Tab`, or `Ctrl+Alt+J` to search. Ships with C, Kconfig, device tree and linker-script snippets; edit them in Settings → Code Editor.
- TODO / FIXME panel listing every marker in the project.
- Syntax highlighting for Kconfig, device tree (`.dts`, `.dtsi`, `.overlay`) and linker scripts (`.ld`, `.lds`, `.icf`, `.sct`); `.config` / `defconfig` / `sdkconfig` are shown as key = value files.
- Optional Vim keys (Settings → Code Editor): Normal / Insert / Visual / Visual Line modes, counts, operators with motions and text objects, `.` repeat, registers, `/` search with `n` / `N`, and `:w`, `:q`, `:wq`, `:bn`, `:vs`, line numbers.

## 0.8.0

- Integrated terminal panel (`` Ctrl+Shift+` `` / `Alt+F12`): cmd, PowerShell, Git Bash and WSL on Windows; bash, zsh, fish on Linux. Multiple tabs, maximize, and `Ctrl+click` on `file:line` (compiler errors, grep output) opens the file at that line. Starts in the project folder.
- Find and replace in the file (`Ctrl+H`): regular expressions with `$1` groups, match case, whole word, a live preview of the replacement and one-step undo for Replace All.
- Replace across the project from the search panel, with a strikethrough / green preview per result and a two-click confirm. Files that are not open are written back keeping their encoding (GBK, BOM, CRLF).
- Split editor (`Ctrl+\`) with a draggable divider; drag tabs to reorder; tab context menu (close others / to the right / saved, split right, copy path, reveal).
- Explorer: new file / new folder / rename / delete from the context menu (rename follows open tabs). Right-click the folder button for recent projects.
- Git in the editor: added / modified / deleted bars in the gutter, updated as you type, and an inline blame on the current line (author, time, commit message).
- Files changed outside QCCOM reload automatically; if you also have unsaved edits a bar asks whether to reload or keep yours.
- Crash recovery: unsaved files are backed up two seconds after you stop typing. After a crash or power loss QCCOM offers to restore them.
- Optional auto save (1 s after you stop typing).
- Portable mode: put a `qccom-data` folder next to the program (or click Enable in Settings → General) and settings live there.
## 0.7.0

- New code editor: undo / redo, multiple cursors (`Alt+click`, `Ctrl+D`, `Ctrl+Shift+L`, `Ctrl+Alt+↑/↓`), column selection, auto-closing brackets and quotes, bracket matching, auto-indent, indent guides, word highlight, code folding (`{}`, `#if`, comments), sticky scroll, `Ctrl+/` comments and line move / copy / delete. Files are editable without a separate edit mode; unsaved tabs show a dot.
- Correct spacing for CJK text and wide characters in the editor.
- Semantic colors: functions, macros, types, enum constants and globals are colored by kind from the project index.
- Context window (`Ctrl+Shift+Y`): the definition of the symbol under the cursor, with its comment, below the editor.
- Call hierarchy (`Ctrl+Shift+R`): calls / called-by tree for the current function, with pinning.
- Bookmarks (`Ctrl+F2`, `F2`, `Shift+F2`), listed at the top of the Outline panel.
- Optional clangd: when clangd and a `compile_commands.json` are found, Go to Definition uses clangd (local variables, struct members, same-name static functions). Falls back to the built-in index otherwise.
- 23 new editor commands in the keyboard shortcut editor and command palette.

## 0.6.0

- Light theme, and a **System default** option that follows the OS theme live. Fixed popups turning white with unreadable text when the OS used a light theme.
- All text colors are checked against at least 4.5:1 contrast in both themes, including code highlighting, macro hovers and log levels.
- Free text selection in the log: drag, double-click for a word, triple-click for a line, auto-scroll at the edges. Copy without timestamps by default (configurable).
- Command palette (`Ctrl+Shift+P` / `F1`) with fuzzy search over every command, theme, language and settings page.
- Settings page (`Ctrl+,`) with search: General, Appearance (theme, UI scale, font size), Log, Connection, Code Editor, Keyboard Shortcuts, About.
- Keyboard shortcut editor: rebind any command, multiple bindings, two-step sequences, conflict warnings, JSON import / export.
- Activity bar: one gear menu replaces the language and update icons; the update icon only appears when there is something to do.

## 0.5.0

- First public release: serial / TCP / UDP / MQTT, terminal mode, quick commands, code viewer, Git viewer, self-update, 10 languages.
