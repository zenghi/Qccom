# The UART baud rate didn’t change. Why did the console stop making sense?

A board boots after a firmware update, but its serial console now prints garbled text or drops characters. The host still says **115200**. Before replacing the USB adapter or sweeping through baud rates, check what the firmware actually changed: baud rate is only one part of the serial format.

I maintain [QCCOM](https://www.zenghi.com/en/), an open-source serial and embedded debugging tool. This walkthrough uses its visual folder and text comparison to isolate a UART framing change. The two folders below are a **synthetic fixture**, not firmware from a physical board. The point is to make the source comparison reproducible without hardware.

## 1. Make the comparison small enough to trust

Keep the last known-good source tree on the left and the suspect tree on the right. For this exercise, create the following folder layout, or use the two folders included with the fixture:

```text
known-good/
  config/uart_console.h
  docs/bench-notes.txt
candidate/
  config/uart_console.h
  docs/bench-notes.txt
```

Both copies of `bench-notes.txt` say: `Host console setup: 115200 baud, 8N1, no flow control.` In the old header, put:

```c
#define CONSOLE_BAUD      115200u
#define CONSOLE_FRAMING   "8N1"
```

Copy it to the candidate folder and replace only `"8N1"` with `"7E1"`. This changes the data width and parity settings, without changing the file length or the baud rate. In the accompanying ZIP fixture the headers even have the same modification time. A Git checkout may assign different modification times. No build or flashing is needed.

## 2. Compare the source bytes, not just the metadata

In QCCOM v1.9.6, open **Compare** from the activity bar or press **Ctrl+Shift+M**. On the Compare home tab, choose `known-good/` on the left and `candidate/` on the right, then start a folder comparison. In its toolbar, choose **By content**. That checks file bytes even if sizes and timestamps match, which is useful when investigating exported snapshots or an update made in place.

Double-click `config/uart_console.h` in the results to open its text comparison. Turn on **Differences only** if the two files are long. The fixture’s expected difference is:

```diff
  #define CONSOLE_BAUD      115200u
- #define CONSOLE_FRAMING   "8N1"
+ #define CONSOLE_FRAMING   "7E1"
```

The bench instructions are identical on both sides; they still describe the old setting. For a UI reference, see QCCOM’s [documented folder comparison](https://github.com/zenghi/Qccom/blob/v1.9.6/docs/USER_GUIDE.md#compare-and-merge), which includes screenshots. Those screenshots show the feature, not a run of this fixture.

## 3. Confirm the cause before changing firmware

**8N1** means eight data bits, no parity and one stop bit. **7E1** means seven data bits, even parity and one stop bit. A baud-rate match does not make those interchangeable. In a real firmware tree, first check whether the changed setting applies to the active board profile or is overridden during the build. If it does, compare the device’s documented serial settings with the host’s data-bit, parity and stop-bit settings. Close and reopen the port with the documented profile, and capture a fresh boot log. If the host still expects 8N1 and the device intentionally moved to 7E1, update the bench instructions as well as any automated test that opens the port.

If the candidate’s active build still uses 8N1, the change is a lead, not an explanation. Check flow control, baud clock accuracy, or whether you opened the intended port next. Keep one variable fixed at a time so the next log is useful evidence.

**Verification:** A test using the v1.9.6 folder comparison engine found the same-size UART headers different and the two copies of the bench notes identical. The fixture does not validate any physical board or serial adapter.

[QCCOM website](https://www.zenghi.com/en/) · [Source and issue tracker](https://github.com/zenghi/Qccom) · [v1.9.6 user guide](https://github.com/zenghi/Qccom/blob/v1.9.6/docs/USER_GUIDE.md)
