# XF chord display and LCD segment IDs

## Current finding

The firmware's common logical LCD segment setter is `set_lcd_segment_by_id` at `0x60077d00` in Ghidra program `merged.bin` (formerly `FUN_60077d00`). It looks up each logical ID in a four-byte table at `0x600e3a24` (the decompiler obtains the pointer through `DAT_600783fc`). Each entry begins with a 16-bit display-buffer slot and a one-byte bit mask. A nonzero value sets that mask; zero clears it. The table is shared by the LCD generally; this alone does not identify which IDs represent chord display elements.

The first five table entries are directly confirmed from bytes at `0x600e3a24`:

| Logical ID | Buffer slot | Mask | Status |
|---:|---:|---:|---|
| `0x00` | 28 | `0x08` | Mapping verified; visual meaning unknown |
| `0x01` | 39 | `0x01` | Mapping verified; visual meaning unknown |
| `0x02` | 61 | `0x01` | Mapping verified; visual meaning unknown |
| `0x03` | 60 | `0x01` | Mapping verified; visual meaning unknown |
| `0x04` | 47 | `0x02` | Mapping verified; visual meaning unknown |

Slots `0x40` in later entries appear to be invalid/sentinel mappings; do not treat them as writable display locations. The setter indexes by ID without an internal range check, so only IDs backed by confirmed table entries should be used in any emulator or diagnostic command.

## XF chord-event trace status

Yamaha XF chord-name meta-events use the sequencer-specific meta-event payload beginning `FF 7F 07 43 7B 01` (the remaining bytes encode chord fields). That is the event signature to follow from the SMF parser. A Ghidra byte search of the firmware found no literal complete signature, which is expected if the handler compares parsed bytes or firmware does not contain a sample event.

The sequencer track loop `seq_psmf_process_track_events` at `0x60092c38` calls external event parser routines and routes some events through `FUN_6005f488`; this path has not yet been shown to handle the XF chord meta-event. A separate function `FUN_01002a2c` branches on MIDI event/controller numbers; its `param_1 == 0x7b` case is ordinary MIDI Channel Mode handling (All Notes Off), not the XF meta-event. Do not confuse those values.

No chord-specific logical segment IDs have been confirmed yet. The candidate segment table and setter are known, but IDs must be attributed to the chord display by tracing the XF event dispatch to a call of `set_lcd_segment_by_id` or by controlled observation of display-buffer deltas. Until then, the mappings above are verified logical LCD elements, not confirmed chord-display IDs.

## Additional callers inspected

The four direct caller functions have been renamed/commented in Ghidra:

- `UpdateLcdSegmentsDfE0FromMode` (`0x60078138`) updates IDs `0xDF` and `0xE0` from two state predicates.
- `UpdateLcdSegmentA9FromState` (`0x60078150`) updates ID `0xA9` from a state byte.
- `SetLcdSegmentB0Active` (`0x60078160`) asserts ID `0xB0`.
- `UpdateFivePartLcdStatusSegments` (`0x60078300`) iterates over five indexed part states and conditionally updates two groups of segment IDs. The primary IDs are `E7,E8,E9,EA,EB`; the secondary IDs are `EF,E1,E2,E3,E4`, with `EF` explicitly used as a skip sentinel.

The five-part updater confirms these segment-table mappings:

| Logical ID | Buffer slot | Mask |
|---:|---:|---:|
| `0xE7` | 29 | `0x01` |
| `0xE8` | 45 | `0x04` |
| `0xE9` | 21 | `0x02` |
| `0xEA` | 53 | `0x04` |
| `0xEB` | 21 | `0x01` |
| `0xE1` | 44 | `0x04` |
| `0xE2` | 20 | `0x02` |
| `0xE3` | 52 | `0x04` |
| `0xE4` | 20 | `0x01` |

These are proven LCD element mappings, but the current call path associates them with five part-status calculations, not with XF chord-name handling. Their physical legends and any relationship to chord display remain unverified.

## Hardware access

This investigation used Ghidra reads only for firmware analysis. Keep PSR diagnostic-shell activity read-only while identifying mappings. Do not write arbitrary memory or LCD GPIO addresses; the setter's lack of bounds checks and the multiplexed LCD driver make direct writes unsafe without a verified path.
