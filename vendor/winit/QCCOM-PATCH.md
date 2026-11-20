# QCCOM’s winit 0.30.13 backport

This is the published [winit 0.30.13 crate](https://crates.io/crates/winit/0.30.13)
(Apache-2.0, see `LICENSE`; archive SHA-256
`a6755fa58a9f8350bd1e472d4c3fcc25f824ec358933bba33306d0b63df5978d`).
Apart from this note, QCCOM only changed two files in `src/platform_impl/windows/`:
`dark_mode.rs` exposes the existing Windows build number and initializes the
version query's structure size; `event_loop.rs`
uses the Windows-supplied `WM_DPICHANGED` rectangle unmodified during live
move/resize on Windows 11 (build 22000 and later).

The previous adjustment can return a dragged window to the old monitor and
cause repeated DPI changes. The backport combines the scope of [winit #4694](https://github.com/rust-windowing/winit/pull/4694)
with the Windows version check from the merged [winit #4341](https://github.com/rust-windowing/winit/pull/4341).
It leaves Windows 10 and non-drag transitions on the original path. Native DPI
notifications still reach eframe, and the OS still handles title bar dragging,
resizing and snapping. Remove the Cargo override and this folder once QCCOM
uses a released winit with the fix.

## Windows 11 regression check

1. In Windows Display Settings, use three extended displays. Set the primary
   display to 125% and the display to its left to 100%; place the third
   display above or to the right with a different vertical offset. A display
   to the left of the primary has a negative desktop X coordinate.
2. Maximize QCCOM on the primary display. Grab the title bar at the left,
   middle, and right in separate passes. Slowly drag across the seam to the
   100% display; pause briefly while the window straddles the seam, then
   continue. Repeat from a non-maximized window and in both directions.
3. The window should remain under the cursor and settle at the destination's
   scale with no back-and-forth changes, repeated growth, or jump back to the
   previous monitor. Move to the third display and repeat. Try Windows Snap
   on each display, then maximize and restore; also make sure mouse hit
   targets still match their on-screen positions after a move.

This needs a real mixed-DPI Windows desktop. A successful cross-build verifies
compilation, not the native drag loop.
