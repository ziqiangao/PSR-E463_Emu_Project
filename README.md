# PSR-E463 Emulator Project

An experimental emulator and diagnostic toolkit for the Yamaha PSR-E463. The
project combines a native GTK panel UI with an ARMv7-M firmware runtime backed
by Unicorn, plus hardware research notes and partial models of the keyboard's
peripherals.

## Project status

The panel UI supports keyboard and mouse control events, two-contact piano
keys, knobs, an encoder, named LED outputs, and addressable LCD segments. The
runtime loads the supplied firmware images and provides smoke-test and
debugger modes. It models the ML9040A character LCD path and a subset of the
external flash commands.

This is not yet a complete hardware emulator. Normal firmware execution still
stalls at the Yamaha boot screen and has not reached the `GrandPno` milestone.
The correct RTOS task wake/interrupt source is unresolved, and boot from
erased flash has not been verified. Several startup peripheral stand-ins are
approximations, not hardware models. Physical input matrix wiring, USB device
behavior, and audio output remain incomplete.

The seven LCD track-status artwork elements now have stable IDs:
`LCDTrack1`–`LCDTrack6` and `LCDTrackA` (`A` is the accompaniment track).
They can be driven through generic LCD segment outputs, but firmware LCD bus
writes are not yet decoded automatically into those indicators.

## Build and run

Install Rust, GTK 3 development libraries, librsvg, CMake, a C compiler, and
Clang/libclang (needed by Unicorn's Rust bindings). Then, from the repository
root:

```sh
cargo run
```

Common diagnostic modes:

```sh
cargo run -- --debug-firmware
cargo run -- --test-firmware
cargo run -- --test-display
cargo run -- --force-test-mode
```

`--force-test-mode` injects the firmware's observed C#2, F2, and G#2 startup
scan samples; it does not guarantee test-mode entry while task waking is
unresolved. The debugger also supports an opt-in one-shot event-4 wait bypass:

```sh
cargo run -- --debug-firmware --diagnostic-wake-event4
```

That diagnostic is not an RTOS event or hardware model and has not reached
`GrandPno`. See [README_EMULATOR_UI.md](./README_EMULATOR_UI.md) for full
run instructions, debugger commands, panel input/output formats, and model
details.

## Milestones

1. Identify and model the evidence-backed event/interrupt path that wakes the
   boot task; verify boot reaches `GrandPno` from erased flash and performs the
   expected initialization writes.
2. Establish physical switch and piano matrix wiring, then connect ordinary
   panel input, scan, and LED behavior to firmware.
3. Decode firmware LCD transactions into named segments, including
   `LCDTrack1`–`LCDTrack6` and `LCDTrackA`, and verify the resulting states.
4. Replace provisional peripheral stand-ins with verified models, prioritizing
   the boot path; expand USB, audio, and timing behavior.
5. Add hardware-independent regression tests for boot, panel-output mapping,
   and peripheral behavior as those paths become reproducible.

## Repository contents

- `src/` — GTK frontend, emulator core integration, LCD and flash models.
- `ui/` — panel controls, LCD character ROM data, and UI configuration.
- `README_EMULATOR_UI.md` — detailed UI, runtime, and debugger documentation.
- `NOTES_EMULATOR_REQUIREMENTS.md` — emulator research and unresolved
  requirements.
- `PSR_SHELL_ADDRESS_SAFETY.md` — safety guidance for hardware memory access.
- `PSR_Shell_usage.md` and `README_PSR_SHELL_MCP.md` — PSR diagnostic-shell
  usage and MCP bridge documentation.
- `SW Id's.txt` — firmware switch-ID notes.
- `PSR-E463.svg` — panel artwork, including the addressable LCD segments.

Additional LCD and chord analysis is recorded in `XF_CHORD_LCD_NOTES.md`.

## Hardware safety

The PSR shell forwards unrestricted diagnostic commands. Treat shell commands
as hardware access: keep exploration read-only unless a target memory region
and recovery procedure have been independently verified. Do not infer that a
write is safe from a successful read or from a Ghidra image range.
