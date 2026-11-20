# QCCOM firmware comparison fixture

These folders contain original synthetic data for a QCCOM whole-folder comparison. They're not complete firmware and aren't meant to be compiled or flashed. The only content difference is CONSOLE_FRAMING: 8N1 versus 7E1. The two headers have equal byte lengths and intentionally matching modification times in the source fixture; ZIP extraction tools may change those timestamps. The same directory trees work regardless when compared **By content**.

Open QCCOM’s **Compare** page. Compare the two sibling folders, not the entire `diff/` directory.
