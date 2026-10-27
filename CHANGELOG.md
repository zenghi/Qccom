# Changelog

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
