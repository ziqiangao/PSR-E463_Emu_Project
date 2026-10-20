# PSR diagnostic shell: address safety notes

Updated: 2026-10-18

This is a conservative field note for agents using the live PSR-E463 diagnostic shell. “Safe” here means a specific read-only operation has been exercised or directly traced. A region present in Ghidra is not proof that the same address is mapped or harmless on the running keyboard.

## Confirmed read-only access

| Address / operation | Evidence | Guidance |
|---|---|---|
| `ioport` shell command | Run successfully on the connected keyboard; Ghidra `diagnostic_ioport_report` at `0x600b4752` reads six 16-bit registers at `0x40000100`, `0x40000102`, `0x40000104`, `0x40000106`, `0x40000108`, and `0x4000010A`. | This exact diagnostic command is the confirmed way to inspect the GPIO snapshot. It returned Port A H/M/L, Port B H/L, and Port C without disrupting the shell. Values are live and can change. |
| `help`, `info`, `dispinfo` shell commands | Each was run successfully and returned text followed by the normal `> ` prompt. | These are the confirmed low-risk shell inspection commands. |

No general-purpose address has been established as safe for arbitrary shell `d` dumps. The successful `ioport` operation does **not** validate other addresses in the same peripheral range.

## Emulator stubs: live shell observations

After the user rebooted the keyboard, these read-only `d` probes completed and returned to the `> ` prompt. `d/s 4` displayed 16 bytes per command. Values below are raw byte order from the shell; they are snapshots and may change while the instrument runs. They characterize observed reads only, not write safety or a complete hardware map.

| Emulator range / target | Live `d` observation | What is established |
|---|---|---|
| `0x20000000..0x20200000` (emulator SDRAM); probe `0x20002000` | `62 00 00 40 00 40 04 40 04 40 64 40 50 40 6E 40` | The startup-cleared address is readable and contains nonzero runtime data now. Do not assume a zero-filled live RAM region. |
| `0x68000000..0x68394000`; probe `0x68000830` | 16 zero bytes | Read returned zeros at the start of the firmware's `{length=0x393470, destination=0x68000830}` clear range. Ghidra's block covers only `0x19420` bytes from `0x68000000`, so the full runtime extent remains unconfirmed. |
| `0x40000000..0x40000fff`; probe `0x40000028` | `00 00 00 00 AC FE 00 00 AB FF 00 00 00 00 00 00` | Firmware polls `0x40000028`; it read as zero in this snapshot. Nearby words at `+4` and `+8` were `0x0000feac` and `0x0000ffab`. Register function unresolved. |
| `0x40004000..0x40004fff`; probe `0x4000400c` | `00 00 01 00 00 00 00 00 00 00 00 00 00 00 00 00` | The transfer data register at `0x4000400c` read `1`; control/status at `0x4000400e` read `1`. Ghidra shows startup sets bit 0 at `0x4000400e`; a transfer routine toggles bit 9 there. Hardware peripheral identity is still unresolved. |
| `0x40008000..0x40008fff`; probes `0x40008000`, `0x40008020`, `0x40008028` | At base: `BD 43 00 00 34 24 00 00 E2 00 00 00 00 00 00 00`; at `+0x20`: `C0 BC 02 00 67 3B 02 00 A3 00 00 00 00 00 00 00`; at `+0x28`: `A3 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00` | Ghidra `FUN_60086824` writes config values to offsets `+0`, `+8`, `+0x20`, and `+0x28`. Live `+0x28` matches the configured `0xa3`; block's hardware function remains unknown. |
| `0x40009000..0x40009fff`; probes `0x40009000`, `0x40009020`, `0x40009028` | At base: `C3 4A 01 00 F4 9E 00 00 E2 00 00 00 00 00 00 00`; at `+0x20`: `27 42 00 00 54 38 00 00 E2 00 00 00 00 00 00 00`; at `+0x28`: `E2 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00` | Same Ghidra routine writes `+0`, `+8`, `+0x20`, `+0x28`; live `+0`, `+8`, `+0x20`, `+0x28` include `0x14ac3`, `0xe2`, `0x4227`, `0xe2`. Function unknown. |
| `0x4000a000..0x4000afff`; probes `0x4000a000`, `0x4000a020`, `0x4000a028` | At base: `78 AE 33 00 A2 7F 0E 00 E2 00 00 00 00 00 00 00`; at `+0x20`: `86 95 02 00 EE 7B 00 00 E2 00 00 00 00 00 00 00`; at `+0x28`: `E2 00 00 00 00 00 00 00 01 00 00 00 01 00 00 00` | Same Ghidra routine configures this third block. The current values at `+0x30` and `+0x34` are both `1`; hardware function unknown. |
| `0xe000e000..0xe000efff`; probe `0xe000ed00` | `31 C2 2F 41 00 00 40 00 00 00 01 01 00 07 05 FA` | `SCB->CPUID` is `0x412fc231` (Arm Cortex-M3 r2p1); `SCB->VTOR` at `0xe000ed08` reads `0x01010000`, matching firmware setup. This confirms the core profile, not the identity of other MMIO. |
| `0x40002700` (not mapped in emulator); probe `0x40002700` | Eight repeated halfwords `00 F0` (`0xf000`) | Driver `FUN_01001768` initializes 64 consecutive halfwords from `0x40002700` to `0x4000277e` to `0xf000`, with additional nearby register writes. It appears to be an MMIO register block; its peripheral function is not identified. |

Probe transcript used `d/s 4`, followed by the listed `d ADDRESS` commands (addresses omit the `0x` prefix). All returned normally during this session. Avoid `m` commands; these observations do not establish any safe writes. The shell documentation warns that some memory regions can hang/crash the CPU, so unknown reads beyond the addresses exercised above remain unverified. Prefer the already-tested `ioport` command when it answers the question.

## Do not write

| Address / range | Why |
|---|---|
| `0x40000100–0x4000010B` | Active MMIO register bank. Ghidra shows LCD data on `PortA_L[10:3]` at `0x40000104`, RS on bit 11, and ENB on bit 12. Firmware updates these through read/modify/write operations. A shell write can corrupt the LCD bus or other GPIO functions. Use `ioport` to inspect; do not use `m`, `m/b`, `m/w`, or `m/l` here. |
| Other addresses in the `0x40000000` peripheral area, especially `0x40008000` | `0x40000102` is identified as Port A M by the `ioport` reporter. `0x40008000` is an unresolved control/status candidate with Ghidra write references from `0x60000070`, `0x6008682E`, and `0x68006C06`. The wider peripheral map and register semantics are incomplete. Do not generalize safety from the known GPIO registers. |
| `0x00000000–0x000133FF`, `0xFFFF0000–0xFFFF001F` | Ghidra maps low memory and vector labels here, but stack, exception, and runtime data roles are not fully resolved. Do not write or probe these from the live shell. |
| Any address in the static firmware, driver, or instrument-data image ranges below | These are Ghidra image blocks, not an established live-shell memory permission map. Treat them as static-analysis ranges only until live mapping is independently confirmed. |

There are currently **no confirmed safe write addresses**. Avoid all `m` commands until a scratch RAM region has been identified from startup and memory-map evidence and separately validated. The shell documentation warns that invalid regions can hang/crash the CPU and require a power cycle; the power button may not recover it.

## Ghidra image ranges (static analysis only)

Ghidra's `merged.bin` currently has these blocks:

| Range | Static interpretation |
|---|---|
| `0x00000000–0x000133FF` | Low memory / vectors / runtime data; exact live mapping unresolved |
| `0x01000000–0x0100FFFF` | First block of the driver image |
| `0x01010000–0x01027FFF` | Second block of the driver image |
| `0x60000000–0x618C9FFF` | Supplied firmware image |
| `0x68000000–0x6801941F` | Supplied instrument/device data image; runtime role not fully confirmed |

These ranges are useful for Ghidra code/data inspection. Do not infer that they are safe to read or write through the hardware diagnostic shell just because Ghidra contains them.

## Procedure for new addresses

1. Search the address and its references in Ghidra. Identify access width, read/write direction, and whether it is ordinary RAM, a firmware image, or MMIO.
2. Check whether the shell documentation or an existing read-only diagnostic explicitly covers the address.
3. If not, record it as **unknown**. Do not test a write. Do not assume a successful access to a neighboring address proves safety.
4. Before any future write experiment, identify a confirmed scratch RAM range and a recovery path. No such range is documented yet.
