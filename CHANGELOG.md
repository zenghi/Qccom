# Changelog

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
