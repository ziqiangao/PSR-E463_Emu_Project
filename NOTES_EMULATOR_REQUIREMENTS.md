# PSR-E463 emulator: initial requirements and reverse-engineering notes

Updated: 2026-10-18

Live-shell address guidance is tracked separately in [PSR_SHELL_ADDRESS_SAFETY.md](PSR_SHELL_ADDRESS_SAFETY.md). It distinguishes the exact read-only probes we have exercised from static Ghidra image ranges and unknown/unsafe write targets. No general-purpose safe write address has been confirmed.

## Workspace snapshot

The workspace contains the firmware and driver binaries plus shell tooling and project notes; an emulator core has not yet been implemented:

- `firmware-0x60000000.bin` (about 25 MiB)
- `driver-01000000.bin` (about 96 KiB)

The Ghidra project has one open program, `merged.bin`, loaded as a raw binary with language `ARM:LE:32:v7`, little endian, 32-bit address size, image base `0x00000000`, and 7,655 analyzed functions. Ghidra reports five memory blocks:

| Address range | Size | Initial interpretation |
|---|---:|---|
| `0x00000000–0x000133ff` | 0x13400 | low memory / vectors and runtime data; exact role to confirm |
| `0x01000000–0x0100ffff` | 0x10000 | first portion of driver image |
| `0x01010000–0x01027fff` | 0x18000 | combined with previous block, spans full driver image |
| `0x60000000–0x618c9fff` | 0x18ca000 (25,993,216 bytes) | supplied firmware blob; now a single contiguous Ghidra block |
| `0x68000000–0x6801941f` | 0x19420 | device/instrument data region; exact role to confirm |

Ghidra exposes ARM exception-vector symbols at `0x0` through `0x1c` and high-vector labels at `0xffff0000` through `0xffff001c`. These establish that exception handling and vector configuration matter, but the vectors' actual runtime mapping and reset path have not yet been traced. The `Reset` symbol at `0x0` does not contain the image's boot routine: that address disassembles as ASCII-like data. The firmware image starts with Thumb code at `0x60000000`; its first instruction branches to `0x60000010`, and the next path enters a setup routine at `0x6000001c`.

The first 16 bytes of `firmware-0x60000000.bin` match Ghidra's bytes at `0x60000000`. The firmware file is 25,993,216 bytes, and Ghidra now maps a contiguous block of exactly that size at `0x60000000–0x618c9fff`; the earlier missing-range concern is resolved for this supplied blob. This does not prove that the blob contains the entirety of the physical 256Mbit PROG/WAVE ROM. The 98,304-byte `driver-01000000.bin` equals the combined length of the Ghidra `0x01000000–0x0100ffff` and `0x01010000–0x01027fff` blocks, and Ghidra has executable functions beginning at `0x01000000`. This supports direct mapping of that driver image at `0x01000000`. SHA-256:

- `firmware-0x60000000.bin`: `f8a353feb326ac9ec2aa4d8fb3ebe7bf6f5121a65b4388ff4597e83aece549a4`
- `driver-01000000.bin`: `6f2611264c550ecdfba56fdf4d415a311bc0dcaafc29cd522b0e96ec8924ca86`

Ghidra's decompilation of `initialise_and_dispatch_firmware_image` at `0x60000268` shows concrete early-startup behavior: it writes a collection of halfword configuration registers through pointers near `0x600002b8` and `0x600002bc`, sets a control register field to `0x81`, compares two pairs of status/size values, and conditionally decompresses an LZSS image. The decompressor uses a 0x1000-byte history window with initial position 0x12. Startup polls a control register until clear, repeats that poll 50 times, may decompress a second time depending on a status bit, then calls an entry callback. Literal pointer values near `0x600000bc–0x600000d8` include `0x40000102` (now identified as Port A M in the GPIO register bank), an unresolved `0x40008000` candidate, and `0x60000000`. The broader `0x40000000` peripheral map and the `0x40008000` register meaning remain unresolved, so do not probe them from the shell. This makes a boot image handoff and the referenced control/status registers high-priority reverse-engineering targets.

The bounded Unicorn firmware run stopped in a startup memory-clear callback, not at a peripheral write. In Ghidra, `FUN_6007ce4a` is now named `ClearStartupMemoryRanges`; it consumes `{byte_length, destination}` pairs and writes zero words through each range. The callback entry at `0x60107d38` in `InitializeRelativeStartupCallbacks` (`0x60107c9c`) passes a descriptor list at `0x60107d3c`, now labeled `g_dw_startup_zero_range_descriptors`. The first ranges are `{0x7264, 0x0101030c}`, `{0x025c, 0x20002000}`, and `{0x00393470, 0x68000830}`, followed by a zero-length sentinel. A destination with its high bit set is resolved relative to R9.

The emulator maps the 2 MiB `0x20000000..0x20200000` SDRAM request plus an additional zeroed guest-RAM region `0x68000000..0x68394000`, required to cover the third startup-clear descriptor. A memory hook confirmed the original error was a 4-byte zero write to unmapped address `0x68000830` at `PC=0x6007ce60`; the CPU was 4 bytes into the descriptor's `0x393470`-byte clear. Mapping that range removed the write fault, and a 4,000,000-instruction run completed the clear. The emulator is now configured as Unicorn M-class (`ARMv7-M`), rather than generic ARM: Ghidra disassembly at `0x600c9a1a` shows `MSR PSP, R0` followed by `MSR CONTROL, R0`, confirming why generic ARM reported an invalid instruction. With the M-class profile, execution passed those instructions and wrote `0x01010000` to Cortex-M `SCB->VTOR` at `0xE000ED08`; an emulator-only `0xE000E000` system-control backing page lets the smoke run continue.

An earlier `--test-firmware 10000000` run with the live UI's observed `0x40001000` and `0x40002000` pages mapped reproduced the normal UI fault exactly: `WRITE_UNMAPPED`, halfword `0x00f0` to `0x70000aaa` at `PC=0x600d33c0`. Ghidra showed this was not an arbitrary store: `FUN_600d33b4` calls `FUN_600cba2c(0, 0xaaa)` then writes `0x00f0` twice. `FUN_600cba2c` adds the requested offset to the flash base in the table at `0x60106b98`; that table begins with `0x70000000`. The routine therefore sends a flash reset command to base+0xAAA. The service manual identifies IC121 as a 128-Mbit MX29GL128F parallel flash. The live firmware error path records registers, fault details, the recent instruction trace, stack bytes, and RAM/SDRAM/startup-RAM images in `firmware-live-core-dump*`; the smoke-test path writes `firmware-core-dump*` files.

The flash window is implemented by `src/flash.rs` and backed by a 16 MiB `flash.bin` in the project root. A missing file is initialized to `0xff`, matching erased NOR state; shorter files are padded with `0xff`, and larger files are rejected without truncation. The model implements the firmware-observed x16 reset, autoselect, word-program sequences, sector/chip erase, and MX29GL128F CFI query values. Firmware-programmed words are constrained to NOR 1-to-0 transitions and persisted. The default file contains erased bytes, not captured factory parameter data.

The service manual's boot-flow chart says a unit that boots but fails before the test-program decision is likely to have an SWX03 or DataFlash fault. The emulator can reach and display the “YAMAHA” opening indication, so that chart does not by itself prove the cause of a freeze after that point. However, Ghidra shows the firmware queries CFI during flash initialization: it checks `QRY`, then reads the erase-region count at offset `0x58` and region descriptors at `0x5a`–`0x60`. The previous emulator implementation returned zero for these fields, which contradicted the MX29GL128F datasheet (one region, 128 × 128 KiB sectors). `src/flash.rs` now returns the documented CFI fields, including the geometry and timing values read by the firmware. The mapped flash and backing file now cover the full 16 MiB chip capacity; the reported Yamaha-screen freeze still needs a fresh run to confirm whether the CFI correction resolves it. The default flash image contains erased bytes rather than captured factory parameter data.

The earlier smoke-test stop at `PC=0x0100178e` writing `0xf000` to `0x40002700` remains relevant when the `0x40002000` page is omitted; that page is an unknown startup stub, not an identified peripheral model. The `0x40005000` page is now reserved for the provisional disconnected USB model; startup stubs at `0x40000000`, `0x40004000`, `0x40008000`, `0x40009000`, `0x4000a000`, and `0xe000e000` remain zero-backed placeholders or currently unmapped. Ghidra's `0x68000000` block spans only `0x19420` bytes, smaller than the startup-clear descriptor, so the larger guest-RAM map is supported as a firmware startup target but its physical hardware extent is not independently confirmed. Read-only `d` inspections of these live addresses completed after reboot; raw results and interpretations are in [PSR_SHELL_ADDRESS_SAFETY.md](PSR_SHELL_ADDRESS_SAFETY.md). Do not infer shell write safety from these emulator mappings.

### `0x40005028` smoke-test read

Reproduced with `cargo run -- --test-firmware 10000000`: `READ_UNMAPPED`, size 4, at `0x40005028`, `PC=0x600b17a8`. Ghidra disassembly shows the faulting instruction is `ldr r1, [r0]`; `FUN_600b177e` loads `r0` from `DAT_600b18e0`, whose stored value is `0x40005028`, sets bit 2 in the word, and writes the modified value back. A separate initializer references the same address and sets bit 1, so it behaves like a shared control/enable word, not a FIFO or ADC sample read.

The surrounding `FUN_600b177e` setup calls `FUN_600b84e2` with the USB product string `Digital Keyboard` and interface string `iAP_Interface`, after setting bit 2. This is the USB device-side setup for USB TO HOST. A separate storage-volume path references the same control word, sets bit 1, and uses the `USB000` volume name, consistent with USB host-side USB TO DEVICE. This makes the shared control word a USB controller enable/configuration register, not an AUX-in/PCM1803A SDI data register. The exact hardware definition and bit names remain unconfirmed.

The emulator maps the `0x40005000` page and observes writes to `0x40005028`. Its provisional model treats bit 1 as USB host enable and bit 2 as USB device enable. The virtual USB TO DEVICE host and USB TO HOST device stay disconnected; no mass-storage device, USB audio stream, or USB MIDI peer is synthesized. The register backing allows firmware read-modify-write setup to continue, while the controller model reports no attached peer.

The follow-on write fault at `PC=0x600c9a7e`, `address=0x420a0508`, is the Cortex-M peripheral bit-band alias for bit 2 of `0x40005028`: alias offset `0x000a0508` decodes to peripheral byte offset `0x5028`, bit 2. Ghidra's `DAT_600ca218` contains this alias address, and `FUN_600c9a76` writes `1` to it to set the USB-device enable bit. `src/bitband.rs` now maps the peripheral alias window and translates alias reads/writes to the target peripheral bit. The 10-million-instruction smoke run passes this store and then exhausts its instruction budget in `FUN_600c9a76`'s polling loop at `PC=0x600c9a8a`, which reads `0x40000018`; the current zero-backed page does not advance this apparent delay counter. This is now a timer/peripheral modeling issue, not a USB port attach or bit-band fault.

### `0x40004000` transfer block

Ghidra finds no accesses to the first eight bytes at `0x40004000..0x40004007`; the active 16-bit register block starts at `0x40004008`:

| Address | Firmware behavior |
|---|---|
| `0x40004008` | `FUN_600d8424` writes a transfer/start word with bit 15 set, then reads this register while streaming data. |
| `0x4000400a` | Receives the low 16 bits of the transfer address/command (`0x8000` in the traced call). |
| `0x4000400c` | 16-bit data/FIFO register; the transfer helper writes each word from its input buffer here. The live read snapshot returned `1`. |
| `0x4000400e` | Control/status. `FUN_600002c0` sets bit 0 during startup. `FUN_600d8424` sets bit 9 while transferring and clears it afterward. Current working interpretation: bit 9 selects whether the transfer is routed to SDO0 or SDO1. The live read snapshot returned `1`. |

The service manual's overall circuit diagram (PDF page 36, printed page 1) labels SWX03 audio serial pins `SDI`, `SDO0`, and `SDO1` and routes `SDI` to the ADC section. In the audio data direction, this means the PCM1803A ADC's serial output feeds the SWX03's `SDI` input; it is not an ADC input driven by the CPU. The two SDO signals are CPU serial outputs toward the DAC and digital-amplifier paths. The manual does not associate these pins with the `0x40004008` transfer block or document bit 9's exact polarity, so that register-to-pin relationship remains a firmware-based working hypothesis.

The live firmware runtime now hooks writes to `0x4000400c` and routes each 16-bit word to a dummy AK4430 DAC sink when bit 9 at `0x4000400e` is clear, or a dummy YDA176 D.AMP sink when it is set. The sinks retain a count and the latest 32 opaque words; they do not decode PCM, generate audio, or model either chip's control/status behavior. Console output reports route changes and a count/recent-word summary if firmware execution stops. `SDI` remains unmodeled and reads as the existing zero-backed peripheral memory. The bit polarity and association of this transfer block with the schematic SDO pins remain provisional pending more firmware or register-map evidence. This hook is in the live UI firmware runtime only; the firmware smoke-test path is unchanged.

This establishes a firmware-used, word-stream transfer interface with status/control, but not the peripheral attached to it or the meaning of the transfer address. The helper call at `0x600d2ea8` passes transfer address `0x8000`; its surrounding caller has not been fully named. The service manual does not map this register block to a specific IC. The UI model currently leaves this page as generic memory; do not treat it as modeled hardware or write to it through the physical keyboard shell.

## Firmware identity and evidence

Ghidra strings include `PSR-E463` at `0x600ac69c` and `0x600b12b8`, `Yamaha Corporation`, `PSR-E463.BUP`, and a `PSR-E463_instdat.bin` filename at `0x680071d0`. Embedded build paths mention `model18/18PSR-EH`, and source-like filenames include `seq_plst.c`, `seq_plib.c`, `seq_rclb.c`, `seq_pply.c`, `seq_pset.c`, `seq_pon.c`, and `seq_psmf.c`. This points to substantial product/sequence firmware logic and built-in instrument data, but does not by itself identify the physical SoC or all peripheral registers.

String evidence also points to these behaviors and subsystems:

- Keyboard and sound product identity, voices, styles, songs, registrations, and demo content.
- Sequencer/style playback (`Main A`, `Fill In`, `Intro`, `Ending`, `PresetStyle`).
- USB mass storage, file browser/import/export, backup (`USB/FLASH MEMORY`, `PSR-E463.BUP`, `PSR-E463.tmp`).
- Internal flash and RAM checks, boot/parameter versions, and flash writes.
- Audio/part controls and diagnostic output (`Main`, `Dual`, `Left`, `DSP Param`, master volume, pitch bend, headphone/aux status).

These are firmware strings and imply candidate emulator features; actual user-visible reachability and required peripherals still need call-graph and MMIO tracing.

## Service-manual hardware evidence

The workspace now includes `yamaha_psr-e463_sm.pdf` (39-page service manual). The extracted schematic pages and parts list give the following hardware inventory. The board part numbers and IC designators are from the manual, not inferred from firmware symbols.

| Function | Service-manual identification | Emulator implication |
|---|---|---|
| Main CPU / peripherals | SWX03, IC001, Yamaha part YG320A00; chip marking MB8AA4640 | Model the firmware's ARM machine and the SWX03 peripheral register behavior. Ghidra identifies ARMv7, but the manual alone does not specify the internal CPU implementation. |
| Program/wave ROM | IC101, 256Mbit PROG/WAVE ROM in block diagram; part list has programmed ROM entry | Firmware and waveform/program assets may share this device; map/parse the supplied firmware image and identify the ROM contents. |
| Flash | IC121, 128Mbit MX29GL128FLT2I-90G | Persistent user/backup data and flash commands need a nonvolatile model for file/settings features. |
| SDRAM | IC131, 64Mbit W9864G6KH-6 | Main firmware RAM; implement the actual address range/configuration found in firmware startup. |
| Audio ADC | IC702, PCM1803A, 24-bit | AUX IN is digitized; an audio-input model is needed for AUX tests and input features. |
| Audio DAC | IC201, AK4430, 24-bit | Stereo output path; emulator must turn generated PCM into host audio and model relevant control/reset/status behavior. |
| Digital amplifier | IC251, YDA176 | Speaker output, mute, and amp control path. Consider output mixing and speaker/headphone routing. |
| Analog amplifiers | IC211 and IC701, NJM8080G | Headphone/output analog stage; likely represented by gain/routing behavior rather than transistor-level simulation. |
| LCD controller/driver | LCD display LD601, LCD driver IC601; block diagram labels IC601 as LCD driver | Emulate display writes/refresh or expose a host-side display. The manual says LCD enable/RS are SWX03 PA12/PA11 and refresh follows interrupt unmask. |
| USB | USB1.1 full-speed host at USB TO DEVICE; USB1.1 full-speed device at USB TO HOST | Distinct host-storage and device/MIDI/audio-to-PC paths. Emulator models both ports as disconnected; `0x40005028` bit 1/2 role mapping is provisional. |
| Panel/key controls | 61-key keyboard split MKL/MKH, encoder, switch matrices, knobs, LEDs; scanned by SWX03 port lines | Translate host keyboard/panel inputs into the firmware’s GPIO/matrix protocol and return scan/LED state. |
| Clocks | 48 MHz resonator X102 and 16.9344 MHz resonator X101; related SWX03 Yamaha P-45B manual lists internal 135.4752 MHz and master/bus 67.7376 MHz | The E463 manual identifies the board resonators but not the CPU clock. Use 135.4752 MHz as a working SWX03 reference from the related P-45B manual; this emulator does not yet execute cycle-accurately. |
| Power/reset | IC111 reset supervisor; +5D, +3.3D, +3.3A, +HP rails | A pure software model can start rails as valid, but must provide expected reset sequencing and readiness status. |

The service manual's system boot flow (PDF page 21, printed page 56) says: power on, access Program ROM IC101, start initialization, cancel DAC reset/mute through SWX03 PA2, initialize backup data, initialize PCM, unmask interrupts, then complete initialization and refresh the LCD using PA12 (ENB) and PA11 (RS). It explicitly distinguishes test-mode selection from normal “YAMAHA” opening indication and normal “GrandPno” main screen. This gives useful bring-up checkpoints and a documented order for the audio and display devices.

The block diagram (PDF page 34, printed page 9) shows the signal path: ADC PCM1803A for AUX IN; DAC AK4430; YDA176 digital amp; headphone/mute circuits; and stereo speakers. It also shows SWX03 control connections to the keyboard and panel, USB endpoints, LCD, and external memory. The parts list (PDF page 31, printed page 9) supplies chip identifiers and memory capacities. The DMLCD test-point table (PDF page 17, printed page 52) lists +5D, +3.3D, RESET, +3.3A, DAC-L, and DAC-R checks. These references make the manual useful as a top-level hardware contract, but it does not provide the CPU's full register map or prove that every analog behavior must be modeled at circuit level.

## What an emulator needs

### 1. Define an achievable target

“An emulator for the PSR-E463” can mean booting the product firmware, running its UI and file features, reproducing MIDI/audio, or cycle-accurate hardware behavior. Those targets have very different costs. Start with a boot-and-diagnostics milestone, then add interactive controls, MIDI, and sound output. Keep hardware behavior behind interfaces so a partial device model can be improved without rewriting the CPU core.

### 2. CPU and machine execution

- ARMv7 little-endian execution, including ARM and Thumb state as indicated by code inspection.
- Correct exception modes, banked registers, CPSR/SPSR, IRQ/FIQ, reset, and high-vector behavior.
- Instruction fetch/data memory semantics, alignment, aborts, and deterministic stepping.
- A machine scheduler that advances CPU, timers, DMA, audio, and interrupt sources against one clock model.

The Ghidra language is `ARM:LE:32:v7`, while both the firmware and driver entry bytes disassemble as Thumb code; this is strong evidence for the main firmware CPU ISA and use of Thumb state. It does not establish the exact core model, clocks, coprocessors, or timing.

### 3. Memory and boot chain

- Establish the real physical map for ROM/flash, SRAM/DRAM, memory-mapped I/O, and aliases.
- Determine which file corresponds to which address range and whether blobs are complete, encrypted, compressed, or require a boot header.
- Implement reset-time copies/decompression, vector relocation, and initialization before attempting normal boot.
- Model persistent parameter/backup flash and instrument data, including erase/write behavior and checksums where required.
- Preserve the firmware's native address layout, while keeping unmapped reads/writes visible in logs.

The Ghidra memory blocks are an analysis mapping, not proof that these are the physical memory regions or permissions. Compare each workspace blob's length/hash and bytes against the imported Ghidra blocks before writing a loader.

### 4. Peripheral models, prioritized by traced firmware use

Based on the service manual, investigate and likely model:

- SWX03 interrupt/timer and GPIO behavior.
- 61-key and panel switch matrix scans, encoder/knobs, LEDs, and LCD command/data refresh.
- Distinct USB 1.1 full-speed host and device roles.
- Program/wave ROM, SDRAM, flash command behavior, and startup-time memory checks.
- Audio data path using PCM1803A ADC, AK4430 DAC, YDA176 digital amp, mute/headphone sensing, and likely audio DMA/interrupts.
- AUX input capture and sustain pedal detection.
- Clock/reset state needed to make devices ready in the documented startup order.

The block diagram's `DSP` label is next to a panel button (SW273); it does not identify a separate sound-generation DSP. Do not infer a separate audio CPU from this label. Firmware code, memory traffic, and chip-level tracing must determine whether synthesis is performed inside SWX03, in ROM/ASIC logic, or by another component.

## LCD control evidence from Ghidra

## Panel input scan and GPIO evidence from Ghidra

The open Ghidra program `merged.bin` is analyzed as little-endian ARMv7. The
driver's `service_input_scan_and_dispatch_events` at `0x01000836` accesses the
input-scan controller through the pointer stored at `DAT_01000d08`. Ghidra
bytes at that pointer resolve it to `0x40000112`. The service routine samples
configured channels (up to 16), compares their snapshots, dispatches changes,
and then calls `process_pending_input_events` at `0x0100037e`. The setup routine
at `0x01000b2c` initializes this scan interface. This identifies a controller
interface address, but does not establish the physical matrix row/column pin
assignment or the meaning of every register field. The UI's CPU event model
therefore reports symbolic SVG switch IDs and does not invent pin coordinates.

The refreshed SVG includes an eight-character dot-matrix area (`Chars`, with
`Char1`–`Char8`). The supplied controller identification is ML9040A-B01GA.
`src/lcd.rs` provides a software register-level model for instruction/data
writes, 80-byte DDRAM, 64-byte CGRAM, address counter, basic display commands,
and eight visible character positions. Frontend updates render in the artwork's
character area. This is a logical controller model; electrical waveforms and
physical glass wiring are not simulated. The [ML9040A datasheet](https://manualmachine.com/oki/ml9040a/24284564-specifications/)
describes 16 common outputs, 40 segment outputs, built-in character ROM, and
CGRAM; the B suffix selects B-mode LCD waveforms.

The updated SVG knob groups are `LiveControlA`, `LiveControlB`, and
`MasterVolume-2`; their configured centers remain (352.3, 405.99),
(415.4, 405.99), and (510.74, 257.17), respectively.

Ghidra names the LCD GPIO routines `lcd_gpio_set_enable` (`0x0100b1d8`),
`lcd_gpio_write_word` (`0x0100b1e0`), `lcd_gpio_queue_word` (`0x0100b206`),
`lcd_gpio_service_queue` (`0x0100b216`), `lcd_gpio_set_data_bus`
(`0x0100b45c`), and `lcd_gpio_set_rs` (`0x0100b476`). The data-bus routine
writes the controller byte to PA3–PA10, RS is PA11, and ENB is pulsed on PA12.
The service manual identifies PA2 as DAC reset. The GPIO register bank used
by the diagnostic report is based at `0x40000100`; the known low port-A
register is `0x40000104`, and the scan interface pointer is `0x40000112`.

`src/cpu.rs` models these identified addresses for diagnostic register reads
and writes and logs the known LCD/DAC pin transitions. These operations update
local state only. No firmware or ROM is loaded or executed, and no MMIO access
is forwarded to physical hardware. The wider peripheral map, exact switch
matrix pins, and scan-register semantics remain to be traced before building
an executing machine model.

The firmware does expose individual display bits in software:

- `set_display_segment_bits` at `0x600993fc` ORs a supplied mask into a selected display-buffer byte; `clear_display_segment_bits` at `0x60099406` clears the selected mask bits.
- The logical display-item helpers at `0x60077d00` and related functions obtain a byte slot and bit mask from a lookup table. A pointer used by those helpers resolves to table data at `0x600e3a24`; entries are four bytes: a little-endian 16-bit slot, a mask byte, and a padding byte. `0x40` is used as an out-of-range/sentinel slot. The helper IDs include indicator IDs such as `0xa9`, `0xb0`, and `0xdf–0xed`.
- Numeric display formatting at `0x60099d7a` and `0x60099c8a` converts digit patterns into per-segment set/clear calls using glyph/layout tables. This means the image can be driven by firmware bit state, rather than treating every display glyph as one indivisible string.
- `flush_display_buffer_differences` at `0x6009944e` compares 64 output bytes against a previous-state copy and sends only changed bytes. For each changed byte it issues an address command (`index | 0x40`) and data (`byte | 0x4000`) through `thunk_EXT_FUN_0100b206`.
- `update_display_text_if_changed` at `0x600a4880` separately updates up to eight text characters using address commands (`index | 0x80`) and character data (`char | 0x4000`). This is a separate text path from the 64-byte display/segment buffer.
- `initialise_yamaha_display_interface` at `0x600a475a` sends controller setup commands, initializes eight character bytes, clears 64 controller entries, and initializes eight additional display registers. The exact controller command protocol and the physical LCD glass mapping still need validation against the IC601 datasheet or captures.

### LCD GPIO and live diagnostic-shell observations (2026-10-17)

- The diagnostic shell is logged in and responds to read-only commands. `ioport` reports `PortA_H=0x00000003`, `PortA_M=0x000000F0`, `PortA_L=0x00000F7F`, `PortB_H=0x00000001`, `PortB_L=0x00000003`, and `PortC=0x00000081`. `dispinfo` reports display task ID `002` (Jobcon ID `004`). `info` identifies firmware develop number `#4000` (main `#4000`), built Dec 06 2017 16:49:55.
- Recovered `diagnostic_ioport_report` at `0x600b4752`. It reads six consecutive 16-bit values from MMIO base `0x40000100`, at offsets `+0,+2,+4,+6,+8,+0xA`, and prints them as Port A H/M/L, Port B H/L, and Port C. This is a live register read, not a cached software mirror. The shell's `ioport` command is useful for snapshots; no port writes were performed.
- LCD GPIO writes confirm the pin mapping and eliminate the bank ambiguity: `lcd_gpio_set_data_bus` (`0x0100b45c`) writes the low data byte into `PortA_L[10:3]`; `lcd_gpio_set_rs` (`0x0100b476`) sets/clears `PortA_L[11]`; `lcd_gpio_set_enable` (`0x60107de4`) sets/clears `PortA_L[12]`. This agrees with the service manual's PA11=RS and PA12=ENB. `lcd_gpio_write_word` (`0x0100b1e0`) takes one 16-bit display word, places its low byte on PA3–PA10, selects RS from word bit `0x4000` (set = data, clear = instruction), then pulses ENB high and low. The display-driver queue helper `lcd_gpio_queue_word` (`0x0100b206`) enqueues words; `lcd_gpio_service_queue` (`0x0100b216`) dequeues one and performs the GPIO transaction.
- The live `PortA_L=0xF7F` snapshot therefore has RS (bit 11) high and ENB (bit 12) low at the time of the read: an idle state after a data transfer. The 64-byte high-level segment buffer is translated to address/data words and ultimately reaches the LCD over this GPIO bus; model both layers if GPIO behavior matters.
- Avoid writing the MMIO bank at `0x40000100` while the keyboard is running: Port A includes the LCD strobe/control lines, and a write could disrupt display traffic or other GPIO functions. For emulation, implement reads/writes to the register bank and observe firmware-driven ENB/RS transitions; do not emulate the LCD only as a memory buffer if the goal includes GPIO-level behavior.

The service manual lists test item **031 All LCD On** and item **032 All LCD Off** (PDF page 12, printed page 38); each tests whether all LCD dots illuminate or turn off. Firmware strings are `LCD   On` at `0x600f2144` and `LCD  Off` at `0x600f2150`. In test cleanup `FUN_6008813e` checks command/status `0x1f` and calls `FUN_600818d2`; that helper clears the 64-byte output buffer and flushes it. This is consistent with restoring/clearing the display after item 031, not the all-on action itself. The activation routine for item 031 has not yet been isolated; trace the test dispatcher and lookup data further before naming a function as the all-on implementation.

For the emulator artwork, model the firmware-visible state as (a) the 64-byte display buffer, (b) the separate 8-character text buffer, and (c) any auxiliary display registers/indicators. Use the slot/mask table to map individual addressable elements to Illustrator SVG IDs. The physical LCD has a 16-COM by 40-SEG matrix according to the manual, so verify the exact matrix-to-buffer mapping before assuming every glass dot corresponds one-to-one with one of the 512 bits in the 64-byte buffer.

This is a hypothesis checklist, not a confirmed peripheral inventory. Trace constant MMIO addresses and driver calls to determine the actual device set. Avoid implementing USB, display, or analog behavior speculatively if the chosen first milestone only needs boot diagnostics.

### 5. Instrument and content data

The mapped region at `0x68000000` and embedded filename `PSR-E463_instdat.bin` suggest separate instrument data may be involved. Determine whether voice/style/sample content is already contained in `firmware-0x60000000.bin`, in `driver-01000000.bin`, or supplied as a separate instrument-data image. A useful sound-emulation milestone must identify sample formats, voice/program tables, key zones, envelopes, effects, drum maps, and audio mixing. Firmware UI boot alone is not evidence that sound synthesis works.

### 6. Host-facing features

For a practical emulator, define explicit adapters for:

- Keyboard/panel input and display output.
- MIDI input/output and virtual MIDI devices.
- Audio output with a chosen sample rate and channel count.
- Virtual removable USB storage mapped to a host directory or disk image.
- Persistent internal settings/backup storage.

Keep host file access sandboxed to a configured virtual drive. Firmware APIs and file formats should be reverse engineered before claiming compatibility.

## Suggested work sequence

1. Inventory and hash both binary blobs; determine exact sizes, headers, and whether they correspond to Ghidra's mapped regions. The firmware now maps exactly by size/range; verify additional bytes in the driver image against Ghidra.
2. In Ghidra, trace startup from `0x60000000`, identify the first stack/RAM setup, image copies, decompressed image entry, MMU/cache setup, and first peripheral accesses. The vector symbol at `0x0` is not the actual firmware entry in this import.
3. Recover the firmware memory map by collecting constant addresses used in loads/stores and cross-referencing driver initialization. Name each candidate register block with evidence and confidence.
4. Locate IRQ setup and handlers; identify timer, DMA, UART, USB, storage, display, and audio initialization routines. Record pending/enable/ack semantics and observed interrupt sources.
5. Identify firmware/data partition boundaries and boot requirements; build a loader that maps exact bytes and logs unsupported accesses.
6. Implement ARM core, memory map, scheduler, and minimal interrupt/timer models; add regression fixtures from captured instruction/address traces.
7. Boot until the first unsupported MMIO access, implement only the needed behavior, and repeat. Track milestones: reset, startup, diagnostics, panel UI, MIDI, audio, USB/files.
8. Use legal firmware dumps and user-provided assets; keep provenance and hashes in the repository notes.

## Immediate unknowns / next Ghidra passes

- What are the exact meaning and addresses of the startup configuration/control/status registers referenced through literals near `0x600000bc–0x600000d8`?
- What does the SWX03's ARMv7 core expose for physical RAM, peripheral registers, exception/interrupt controller, clocks, and audio DMA?
- Which firmware region contains the 256Mbit PROG/WAVE ROM contents, and how is it split between code and sample data?
- What protocol does the firmware use with the AK4430, PCM1803A, YDA176, LCD driver, and two USB controllers?
- What is the exact SoC/CPU core and boot sequence? ARMv7 ISA is known; exact hardware is not.
- Does the main processor run all audio synthesis, or is there a separate DSP/sound chip with its own firmware?
- Which MMIO ranges are actually accessed during boot and normal operation?
- What RAM sizes, clocks, interrupt controller, timer, DMA, display, USB, MIDI, and audio interfaces are required for each milestone?
- Which instrument/sample resources are present, and how are they decoded and played?
- What is the intended initial fidelity target (boot/UI, MIDI, audio, or full peripheral behavior)?

## Confidence labels

- **Confirmed from Ghidra and local byte comparison:** ARM little-endian 32-bit ARMv7 language; Thumb startup code at `0x60000000`; firmware block range exactly matches the firmware file size; 7,654 analyzed functions; driver blob maps in size at `0x01000000`; firmware prefix matches at `0x60000000`; LZSS boot handoff; PSR-E463 identity strings and product content strings.
- **Confirmed from the service manual:** SWX03/MB8AA4640 main CPU/peripheral IC; external 256Mbit PROG/WAVE ROM, 128Mbit flash, 64Mbit SDRAM; 24-bit PCM1803A ADC and AK4430 DAC; YDA176 digital amp; USB1.1 full-speed host/device; 61-key/panel matrix, LCD, clocks, and documented startup order.
- **Likely:** internal instrument/backup data and external USB storage matter to a full-featured machine.
- **Unverified:** physical chip models, exact memory map, clocks, peripheral register layouts, audio-generation algorithm, boot chain, and whether the binaries are complete dumps.

