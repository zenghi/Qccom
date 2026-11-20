# QCCOM 1.9.8 acceptance checks

## Long-running local recording

- On the recording settings page, set a small size threshold (for example, 1 MiB). Start a new capture, feed enough generated traffic to make several parts, then stop. Check that parts are ordered, each normal record appears once, and numbered files do not replace files from an earlier session.
- Replay one of the parts with the existing replay command. Confirm timestamps, RX/TX direction and payload are readable and in order. If a single record exceeds the threshold, verify that it stays intact and is handled as documented.
- Set the size threshold to zero and verify that size alone no longer rotates the file. Repeat on an unwritable destination or with a full scratch volume; recording should stop with a clear error and previously written parts should still be usable.

## Crash and hang reporting

- Use an isolated profile and do not aim QA traffic at the public community. Exercise network behavior only through the local test doubles.
- With automatic posting off, verify diagnostics can be retained locally and are not posted. When exercising automatic posting through a test double, verify the sanitized fields, the existing sign-in requirement, failure behavior and behavior across restart.
- Verify that a deliberate UI stall is detected, but leaving the app idle or suspending and resuming the machine does not by itself generate a report. A watchdog does not terminate or restart the application.
- Inspect diagnostics before sharing any: report fields should not contain raw serial/terminal logs, arbitrary panic messages, file paths, device IDs or credentials.

## Windows hardware checks

- Connect and disconnect a known BLE peripheral repeatedly; verify that discovery and notifications recover and that scanning keeps the list stable.
- Move maximized and restored windows across mixed-DPI monitors in both directions. See the [Windows 11 regression check](../vendor/winit/QCCOM-PATCH.md#windows-11-regression-check).
