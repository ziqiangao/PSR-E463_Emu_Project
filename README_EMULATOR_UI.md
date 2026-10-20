# PSR-E463 emulator UI

The native Rust GTK app renders `PSR-E463.svg` and uses `ui/controls.json` for
SVG IDs, switch matrix codes, piano bindings, and knob geometry.

## Current state and milestones

The panel UI, keyboard/mouse input events, two-contact piano interaction,
knobs, encoder, and named LED outputs work. The emulator loads and executes
the supplied firmware images with Unicorn and provides a debugger, smoke-test
mode, traces, and core dumps. The LCD has an ML9040A character-write model,
raw bus-event reporting, and generic SVG segment outputs; the seven track
status shapes are addressable as `LCDTrack1` through `LCDTrack6` and
`LCDTrackA` (`A` is the accompaniment track). A partial external-flash model
and several startup/device stand-ins are also present.

This is not yet a complete hardware emulator. Normal firmware execution still
stalls at the Yamaha boot screen and has not reached the `GrandPno` milestone.
The correct RTOS wake/interrupt source is unresolved, the fresh-flash boot
sequence has not been verified, and the seven track indicators are not yet
automatically attributed from firmware LCD writes. Physical input matrix
wiring, USB devices, and audio output are also incomplete; timer and startup
register stand-ins are approximations rather than cycle-accurate hardware.

Prioritized next milestones:

1. Identify and model the evidence-backed event/interrupt path that wakes the
   boot task, then verify boot reaches `GrandPno` from erased flash and performs
   the expected initialization writes.
2. Establish the physical switch/piano matrix mapping and connect ordinary
   panel input, scan, and LED behavior to firmware rather than only emitting
   symbolic UI events.
3. Decode firmware LCD activity into named indicators, including
   `LCDTrack1`–`LCDTrack6` and `LCDTrackA`, and verify their on/off states
   against firmware behavior. The UI can already render these IDs when
   supplied as `lcd_segment` or `lcd_segments_updated` outputs.
4. Replace provisional peripheral stand-ins with verified models, prioritizing
   the peripherals exercised by the boot path; then expand USB, audio, and
   timing behavior.
5. Add hardware-independent regression tests for the boot path, panel-output
   mapping, and each peripheral model as those behaviors become reproducible.

## Build and run

Install Rust, GTK 3 development libraries, librsvg, CMake, a C compiler, and
Clang/libclang (required to build Unicorn's Rust bindings), then run from the
repository root. The project Cargo config supplies the GCC intrinsic-header
path needed by bindgen on the current Linux development machine:

```sh
cargo run
```

`src/armv7.rs` exposes `ArmV7Core`, a 32-bit little-endian ARMv7-M execution
core backed by Unicorn Engine. Normal UI startup loads both supplied firmware
images and runs the main image in 25,000-instruction slices on the GTK timer.
This is an instruction budget, not cycle-accurate timing. The 20 ms UI cadence
does not imply that the guest ran at its hardware clock rate.
The live UI, debugger, and smoke-test firmware runtimes model the observed
16-bit counter at `0x40000018` as a free-running virtual timer, advancing it
once per 1,024 guest instructions. This deterministic approximation allows
timer polling loops to progress; it is not a cycle-accurate model of the SWX03
timer or its prescaler. The secondary-engine startup path also uses zero-backed
control registers at `0x40030000` and a `0x8000`-byte RAM/configuration window
at `0x40038000`; the observed start-request bit completes on the next virtual
timer tick. These are minimal firmware-progress stand-ins, not hardware models.
Other zero-backed startup pages are documented with their Ghidra evidence and
confidence in [NOTES_EMULATOR_REQUIREMENTS.md](NOTES_EMULATOR_REQUIREMENTS.md);
the service manual's block diagrams do not provide an internal SWX03 register
map.

Run an opt-in firmware smoke test with `cargo run -- --test-firmware` or set a
different instruction budget with `cargo run -- --test-firmware 1000000`. The
standalone smoke test remains available and maps `driver-01000000.bin` at `0x01000000` and
`firmware-0x60000000.bin` at `0x60000000`, starts in Thumb state on an
ARMv7-M (M-class) core, and reports the stop PC or first execution error. It
also supplies zeroed low RAM, SDRAM at `0x20000000..0x20200000`, the firmware
startup clear range at `0x68000000..0x68394000`, and zero-backed placeholder
pages for observed startup control and Cortex-M system-control accesses. The
placeholders are not hardware models; unmapped device accesses still stop
execution. Candidate read-only PSR shell `d` inspections for these addresses
are listed in [PSR_SHELL_ADDRESS_SAFETY.md](PSR_SHELL_ADDRESS_SAFETY.md). The
The live UI and smoke test currently use separate machine setup paths so the
smoke test remains useful for diagnosing startup faults.

To simulate the service-manual startup chord, run
`cargo run -- --force-test-mode`. The emulator holds C#2, F2, and G#2 in the
firmware input-scan samples until its modeled LCD displays `TEST`, then releases
them. The same option works with `--debug-firmware`. This is opt-in and does
not modify the firmware image or persistent flash. It currently injects scan
samples only; it does not yet guarantee test-mode entry because the scheduler
still stalls at `0x01006588` waiting for an event source that has not yet been
identified. The emulator now handles Cortex-M SVC and enabled/pending NVIC
exceptions through the firmware vector table, but does not synthesize a
keyboard or timer interrupt without evidence for its hardware source.
A debugger trace also shows the boot-time event-4 wait without any access to
the `0x40010000` controller; its known IRQ 9 setup is reached from USB state
handling, so the emulator does not assert that IRQ as a speculative wakeup.
The wait follows a request for the firmware volume `FMD000`, but Ghidra's
resolved driver callback is only a two-instruction success stub and does not
show that the request starts a transfer. The service manual documents when
interrupts are unmasked and shows SWX03's USB and flash connections, but does
not identify the internal IRQ source or map it to `0x40010000`.

For a downstream-only diagnostic, run
`cargo run -- --debug-firmware --diagnostic-wake-event4`. This makes the first
call to the `FUN_600b7924` event-4 wait wrapper return immediately, allowing
inspection of what boot does next. It is a one-shot wait bypass, not an RTOS
event, interrupt, or hardware model, and does not establish the correct boot
wake source. The diagnostic run returned from that wait but still reached the
dispatcher idle loop without displaying `GrandPno`; the flash image remained
erased. Normal runs remain unchanged.

Start the interactive terminal debugger with `cargo run -- --debug-firmware`.
It uses the same firmware and virtual peripheral setup as the UI, but waits at
the initial PC until you issue a command. Available commands include:

```text
bp 0x600c9a7e       set a breakpoint
c [instructions]   continue (default 1,000,000 instructions)
s [count]          step instructions
r                  show CPU registers
x 0x70000000 32    inspect memory bytes
watch r 0x70000000  stop on a flash read
watch w 0x70000000  stop on a flash write
bt                  scan the active stack for code addresses
trace 64            show recent instruction addresses
dump                write the live core dump and RAM snapshots
help                list commands
```

Addresses are hexadecimal, with or without the `0x` prefix. Continue in
bounded instruction chunks and inspect `trace`/registers to identify polling
loops; breakpoints and watchpoints stop guest execution before returning to
the prompt.

The live UI connects firmware writes on PA3–PA10, PA11 (RS), and PA12 (ENB) to
the ML9040A-B01GA model and uses `ui/character_rom.json` for its configurable
glyphs. The confirmed input-scan controller at `0x40000112` completes requests
with all contacts open by default. Ghidra maps the service chord to scan IDs
15, 19, and 22 (channels 2/3, bits 2/0/3); `--force-test-mode` injects those
samples at startup. The physical matrix wiring remains unresolved, so ordinary
panel/piano inputs are still logged as symbolic events rather than connected to
scan samples. Other startup peripheral pages are zero-backed stubs; LED and
knob ADC behavior is not modeled. Audio transfer words reach
dummy DAC and digital-amplifier sinks, with no sound output.

The service manual identifies two separate USB connections: USB TO DEVICE is
the USB 1.1 host port for flash storage, while USB TO HOST is the USB 1.1 device
port for connection to a computer (USB MIDI/audio). Both virtual ports remain
disconnected. Ghidra suggests bit 1 and bit 2 of the shared control word at
`0x40005028` enable the host and device sides respectively; this register
mapping is provisional. Firmware writes are retained and logged, but the
emulator does not enumerate devices, read mass storage, or exchange USB MIDI or
audio packets yet.

The external flash window begins at `0x70000000` and is backed by `flash.bin`
in the project directory. The configured image is exactly 16 MiB, initialized
to erased `0xff` bytes when first created. The flash model handles x16 reset,
autoselect ID reads, CFI query data for the MX29GL128F, word programming (NOR
1-to-0 behavior), sector erase, and chip erase commands. Programming and
erasing persist to the file. The full 16 MiB MX29GL128F address window is
modeled. This is a functional subset, not a cycle-accurate flash
timing or protection model. The default image is erased flash, not a dump of
factory parameter data.

If normal UI firmware execution stops on an emulation error, it writes
`firmware-live-core-dump.txt` plus RAM, SDRAM, and startup-RAM snapshots with
the `firmware-live-core-dump-` prefix. The report includes the faulting access,
registers, recent instruction addresses, and stack bytes. The smoke-test path
continues to use `firmware-core-dump*`.

The PSR-E463 service manual lists X101 at 16.9344 MHz and X102 at 48 MHz as
board resonators. It does not identify the CPU clock directly. The same SWX03
processor's Yamaha P-45B service manual labels its internal clock 135.4752 MHz
and master/bus clock 67.7376 MHz; the emulator records 135.4752 MHz as the
working SWX03 clock reference, not as an emulated instruction rate. [PSR-E463
service manual listing](https://www.scribd.com/document/866600166/Smanual-Psr-e463),
[Yamaha P-45B service manual](https://documents.cdn.ifixit.com/i4kPLXQkFxwIDFGY.pdf).

Unicorn Engine is distributed under GPL-2.0; account for that license if you
redistribute a build containing this dependency.

To check the character LCD through the ML9040A model, use
`cargo run -- --test-display` (shows `LCD TEST`) or provide up to eight
characters with `cargo run -- --test-display "HELLO 63"`.

Character dot patterns are configurable in `ui/character_rom.json`. Add a
single-character key and eight row masks (decimal 0–31); bit 4 is the leftmost
dot and bit 0 the rightmost. The Rust `lcd::CharacterRom` API exposes
`set_glyph(code, rows)` and `glyph(code)` for loading the same 5×8 patterns
from an emulator core. The included entries are editable starter patterns,
not a claim about the keyboard's mask ROM. The app does not parse MIDI files:
your MIDI reader/core should send each printable character byte to
`Ml9040a::write(true, byte)`, then route resulting display output to the UI.

The window is borderless and transparent where the desktop compositor allows
it. Right-drag or Alt+left-drag to move it; drag the lower-right grip to resize.
The Power button closes the app.

## Controls and keyboard

Every button emits a JSON press event on mouse/key down and a release event on
up. The event identifies the SVG element and includes its firmware matrix code
when one is configured. `Style-2` shares the confirmed `0x13` code with
`Style`. Panel button codes are taken from the firmware's button-test table;
the Power button closes the app and has no firmware matrix code. The 10-key
pad is mapped to the numeric keypad, plus
and minus use keypad add/subtract, and `z s x d c v g b h n j m` plays a
chromatic octave starting at C3. Keypad `*` and `/` move the computer-keyboard
octave up and down, within C1–C5. Clicking the SVG piano keys covers C1–C6.
Each piano press closes contact 1 immediately, then closes contact 2 after the
configured interval. Releasing opens contact 2 (if it closed) and then contact
1. A short tap before the interval elapses only closes contact 1. The firmware
can derive velocity from the time between the two matrix transitions. Mouse
wheel up shortens the interval (higher velocity); wheel down lengthens it.
The interval is clamped to 10–150 ms and shown with the selected octave in cyan.

Regular knobs rotate their SVG groups and report 0–127 values, with 64 at the
center. The two live control knobs have a center detent: values within 6 of 64
snap to 64. `knobs.regular.center_snap_radius` sets the radius and
`center_snap_ids` selects which configured knobs use it.
The rotary encoder reports one direction step for each crossed 15°
detent. Illuminated buttons are matrix-scanned like the other buttons and do
not light just because they are pressed. Named `led` outputs light the matching
button and, for Style, Song, and Groove, their separate indicator artwork in
the existing `#f55` color.

## Core interface

The app's diagnostic CPU I/O model writes event logs to stderr. It also keeps
the JSON-lines bridge on stdout, serialized from the public `PanelInput` enum
in `src/lib.rs`, so an external core can consume panel events. Piano contacts
are individual raw switch transitions:

```json
{"type":"switch","svg_id":"Voice","matrix_code":"12","pressed":true}
{"type":"switch","svg_id":"Voice","matrix_code":"12","pressed":false}
{"type":"piano_contact","key_id":"C4","midi_note":60,"contact":1,"pressed":true}
{"type":"piano_contact","key_id":"C4","midi_note":60,"contact":2,"pressed":true}
{"type":"encoder_step","direction":-1}
```

The UI reads `PanelOutput` JSON lines from stdin and applies them to the named
SVG element. `Voice`, `Style`, `Song`, and `Groove` LED outputs also address
their button-light artwork; Style, Song, and Groove address their separate
indicator paths as well. Other supported LED and LCD segment IDs are artwork
IDs. LCD text is rendered
into the individual 5×8 dot paths in the eight-character `Chars` area. All
ID-addressable LCD segment paths in the SVG's `LCD` group (excluding the
backlight) start off as `#bee`; lit segments use cyan. The encoder SVG group
is configured as `Dial`.

The seven LCD track-status shapes use the stable IDs `LCDTrack1`,
`LCDTrack2`, `LCDTrack3`, `LCDTrack4`, `LCDTrack5`, `LCDTrack6`, and
`LCDTrackA`. The `LCD` prefix distinguishes these display indicators from the
separate interactive `Track1`–`Track6` panel controls. `LCDTrackA` is the
artwork for the accompaniment track. As with other named segment IDs, these
can be driven through the generic LCD segment outputs; the current firmware
path reports raw LCD bus writes but does not yet decode them into these track
states. For example, `{"type":"lcd_segment","svg_id":"LCDTrack1","lit":true}`
lights the first track indicator.

The panel artwork is rendered once to a native-size surface. Resizing scales
that surface; knobs use cached sprites, and switch/LED/LCD changes use Cairo
overlays. The initial window uses the primary display width and the SVG's
aspect ratio.

```json
{"type":"led","svg_id":"Voice","lit":true}
{"type":"lcd_segment","svg_id":"A","lit":true}
{"type":"lcd_text","text":"VOICE 001"}
```

The output types are `led`, `lcd_segment`, `lcd_segments_updated`, `lcd_text`,
and `lcd_bus_write`; the input types are `switch`, `piano_contact`, `knob`, and
`encoder_step`. LCD segment batches have this shape:

```json
{"type":"lcd_segments_updated","segments":[{"svg_id":"A","lit":true},{"svg_id":"B","lit":false}]}
```

Firmware LCD transactions are also emitted as raw bus events, including bytes
that have no current text/segment interpretation:

```json
{"type":"lcd_bus_write","register_select":true,"value":65}
```

`register_select` is `false` for an instruction and `true` for data. The live
firmware UI logs each detected ENB rising edge, and the terminal debugger
prints the same raw RS/value pair. Decoding and SVG segment attribution remain
incomplete; raw events are observable rather than silently discarded.

`src/lcd.rs` exposes the `Ml9040a` controller model for the ML9040A-B01GA.
Pass controller writes as `(register_select, byte)`: `false` selects the
instruction register and `true` selects the data register. It tracks DDRAM,
CGRAM, cursor addressing, entry direction, display enable, and display shifts;
visible character changes are returned as `PanelOutput::LcdText` events. The
frontend rasterizes the characters with a host monospace font onto the SVG dot
paths. The CPU I/O adapter also feeds its existing GPIO LCD word writes into
this model. To drive the LCD from an emulator core, call `Ml9040a::write(rs,
byte)` for each emulated LCD bus transaction, then route any returned
`PanelOutput` through the panel output channel (the GTK app reads newline-
delimited output JSON from stdin). For example, send `{"type":"lcd_text",
"text":"VOICE 001"}` to display text directly, or update mapped SVG segments
with `lcd_segment` / `lcd_segments_updated`. The GTK app's `CpuEmulator` loads
and executes both firmware images. Each detected
GPIO LCD bus transaction is exposed as `LcdBusWrite`; decoded character
changes also update the display model. The external `JsonLinePanelIo` adapter
does not execute firmware and only applies output events supplied by its host.

Unknown or malformed input lines are ignored with a diagnostic on stderr. The
`CpuEmulator` runs the firmware in-process and logs panel inputs. Its known LCD
GPIO path affects the emulated ML9040A; other modeled MMIO reads/writes affect
local emulator state only and are never sent to connected hardware. Console
output names panel switch IDs symbolically because the physical matrix
row/column mapping has not yet been established. `PanelDisplayState` provides
a pure Rust model for applying LED and LCD output updates.

`PanelIo` in `src/lib.rs` is the frontend boundary. `JsonLinePanelIo` remains
available as a basic bridge. `LcdSegmentBank`
accepts batches of `LcdSegmentState` and returns an `LcdSegmentsUpdated` event
containing the IDs and new states, ready to forward as `PanelOutput`.

## Configuration

Edit `ui/controls.json` to change the artwork path, interactive SVG groups,
switch-code assignments, keypad mapping, piano computer keys, octave limits,
initial contact delay and wheel step, or knob centers and angles. Contact delay
is always clamped to 10–150 ms. IDs for keyboard notes, LEDs, and
LCD segments are taken directly from the SVG, so new artwork IDs can be wired
without changing the Rust event types.

The current live-control knob IDs are `LiveControlA` and `LiveControlB`; the
master volume group is `MasterVolume-2`. Keep these SVG IDs and centers in the
`knobs.regular.centers` list aligned with the artwork when it changes.
