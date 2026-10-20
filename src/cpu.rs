//! Panel I/O bridge and bounded firmware execution for the native UI.

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    fs,
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    rc::Rc,
    sync::mpsc::{Receiver, TryRecvError},
};

use unicorn_engine::{unicorn_const::{HookType, Prot}, RegisterARM};

use crate::armv7::ArmV7Core;
use crate::lcd::{CharacterRom, Ml9040a};
use crate::{PanelInput, PanelIo, PanelOutput};

pub const GPIO_REGISTER_BASE: u32 = 0x4000_0100;
pub const INPUT_SCAN_REGISTER: u32 = 0x4000_0112;
pub const PORT_A_HIGH: u32 = GPIO_REGISTER_BASE;
pub const PORT_A_MIDDLE: u32 = GPIO_REGISTER_BASE + 0x02;
pub const PORT_A_LOW: u32 = GPIO_REGISTER_BASE + 0x04;
pub const PORT_B_HIGH: u32 = GPIO_REGISTER_BASE + 0x06;
pub const PORT_B_LOW: u32 = GPIO_REGISTER_BASE + 0x08;
pub const PORT_C: u32 = GPIO_REGISTER_BASE + 0x0a;
pub const SWX03_CORE_CLOCK_HZ: u32 = 135_475_200;
const FIRMWARE_BASE: u32 = 0x6000_0000;
const DRIVER_BASE: u32 = 0x0100_0000;
const INSTRUCTIONS_PER_TICK: usize = 25_000;

/// Event-level stand-in for the SWX03 CPU while firmware loading/execution is
/// intentionally disabled. Inputs are logged and forwarded as JSON lines;
/// outputs arrive from the emulator bridge on stdin.
pub struct CpuEmulator<W: Write> {
    writer: W,
    outputs: Receiver<PanelOutput>,
    event_number: u64,
    registers: BTreeMap<u32, u16>,
    switches: HashMap<String, bool>,
    piano_contacts: HashMap<(u8, u8), bool>,
    lcd: Ml9040a,
    pending_outputs: Vec<PanelOutput>,
    firmware: Option<FirmwareRuntime>,
}

struct FirmwareRuntime {
    core: ArmV7Core,
    pc: u32,
    gpio_writes: Rc<RefCell<Vec<(u32, usize, i64)>>>,
    trace: Rc<RefCell<VecDeque<u32>>>,
    breakpoints: Rc<RefCell<HashSet<u32>>>,
    breakpoint_hit: Rc<RefCell<Option<u32>>>,
    watch_hit: Rc<RefCell<Option<String>>>,
    fault: Rc<RefCell<Option<(u32, usize, i64, u32, String)>>>,
    audio: Rc<RefCell<crate::audio::VirtualAudioOutputs>>,
    usb: Rc<RefCell<crate::usb::VirtualUsb>>,
    root: PathBuf,
    stopped: bool,
}

impl<W: Write> CpuEmulator<W> {
    pub fn new(writer: W, outputs: Receiver<PanelOutput>) -> Self {
        eprintln!("[CPU] panel I/O bridge ready");
        eprintln!(
            "[CPU] confirmed MMIO: GPIO {GPIO_REGISTER_BASE:#010x}..{:#010x}, input scan {INPUT_SCAN_REGISTER:#010x}",
            PORT_C
        );
        eprintln!("[CPU] panel matrix row/column pins are not yet resolved in Ghidra; reporting symbolic switch IDs");
        Self {
            writer,
            outputs,
            event_number: 0,
            registers: BTreeMap::new(),
            switches: HashMap::new(),
            piano_contacts: HashMap::new(),
            lcd: Ml9040a::default(),
            pending_outputs: Vec::new(),
            firmware: None,
        }
    }

    /// Construct the live firmware-backed UI bridge. Peripheral regions whose
    /// meaning is not established remain zero-backed smoke-test stubs.
    pub fn with_firmware(
        writer: W,
        outputs: Receiver<PanelOutput>,
        root: &Path,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut bridge = Self::new(writer, outputs);
        bridge.firmware = Some(FirmwareRuntime::load(root)?);
        Ok(bridge)
    }

    pub fn set_character_rom(&mut self, rom: CharacterRom) {
        self.lcd = Ml9040a::with_character_rom(rom);
    }

    /// Read one modeled 16-bit SWX03 I/O register. Unmodeled reads are logged
    /// and return `None`; no host hardware is accessed.
    pub fn read16(&mut self, address: u32) -> Option<u16> {
        if !is_modeled_register(address) {
            self.log(format!("MMIO read16 {address:#010x} (unmapped)"));
            return None;
        }
        let value = *self.registers.entry(address).or_default();
        self.log(format!("MMIO read16 {address:#010x} -> {value:#06x}"));
        Some(value)
    }

    /// Write one modeled 16-bit SWX03 I/O register and log known pin changes.
    /// This updates emulator state only; it never writes to the keyboard.
    pub fn write16(&mut self, address: u32, value: u16) -> bool {
        if !is_modeled_register(address) {
            self.log(format!(
                "MMIO write16 {address:#010x} <- {value:#06x} (unmapped)"
            ));
            return false;
        }
        let previous = self.registers.insert(address, value).unwrap_or_default();
        let pin_changes = describe_pin_changes(address, previous, value);
        if pin_changes.is_empty() {
            self.log(format!("MMIO write16 {address:#010x} <- {value:#06x}"));
        } else {
            self.log(format!(
                "MMIO write16 {address:#010x} <- {value:#06x}; {}",
                pin_changes.join(", ")
            ));
        }
        true
    }

    /// Simulate the confirmed LCD GPIO write sequence for one controller word.
    /// Segment-ID to controller-address mapping remains a separate layer.
    pub fn write_lcd_word(&mut self, word: u16) {
        let mut port = self.registers.get(&PORT_A_LOW).copied().unwrap_or_default();
        port = (port & !0x07f8) | ((word & 0x00ff) << 3);
        port = if word & 0x4000 != 0 {
            port | (1 << 11)
        } else {
            port & !(1 << 11)
        };
        self.write16(PORT_A_LOW, port);
        self.write16(PORT_A_LOW, port | (1 << 12));
        self.write16(PORT_A_LOW, port);
        if let Some(output) = self.lcd.write(word & 0x4000 != 0, word as u8) {
            self.log_output(&output);
            self.pending_outputs.push(output);
        }
    }

    pub fn switch_state(&self, svg_id: &str) -> bool {
        self.switches.get(svg_id).copied().unwrap_or(false)
    }

    pub fn piano_contact_state(&self, midi_note: u8, contact: u8) -> bool {
        self.piano_contacts
            .get(&(midi_note, contact))
            .copied()
            .unwrap_or(false)
    }

    fn log(&mut self, message: String) {
        self.event_number = self.event_number.wrapping_add(1);
        eprintln!("[CPU event {:06}] {message}", self.event_number);
    }

    fn log_output(&mut self, output: &PanelOutput) {
        match output {
            PanelOutput::Led { svg_id, lit } => self.log(format!(
                "panel output LED id={svg_id} state={}",
                if *lit { "on" } else { "off" }
            )),
            PanelOutput::LcdSegment { svg_id, lit } => self.log(format!(
                "panel output LCD segment id={svg_id} state={}",
                if *lit { "on" } else { "off" }
            )),
            PanelOutput::LcdSegmentsUpdated { segments } => {
                let states = segments
                    .iter()
                    .map(|segment| {
                        format!(
                            "{}={}",
                            segment.svg_id,
                            if segment.lit { "on" } else { "off" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                self.log(format!("panel output LCD segments [{}]", states));
            }
            PanelOutput::LcdText { text } => self.log(format!("panel output LCD text {text:?}")),
        }
    }
}

impl<W: Write> PanelIo for CpuEmulator<W> {
    fn emit_input(&mut self, input: PanelInput) {
        let description = match &input {
            PanelInput::Switch {
                svg_id,
                matrix_code,
                pressed,
            } => {
                self.switches.insert(svg_id.clone(), *pressed);
                format!(
                    "panel matrix switch id={svg_id} firmware_code={} state={}",
                    matrix_code.as_deref().unwrap_or("unassigned"),
                    if *pressed { "closed" } else { "open" }
                )
            }
            PanelInput::PianoContact {
                key_id,
                midi_note,
                contact,
                pressed,
            } => {
                self.piano_contacts.insert((*midi_note, *contact), *pressed);
                format!(
                    "piano matrix key={key_id} midi={midi_note} contact={contact} state={}",
                    if *pressed { "closed" } else { "open" }
                )
            }
            PanelInput::Knob { svg_id, value } => {
                format!("panel analog input id={svg_id} value={value}")
            }
            PanelInput::EncoderStep { direction } => {
                format!("panel encoder direction={direction:+}")
            }
        };
        self.log(description);
        if serde_json::to_writer(&mut self.writer, &input).is_ok() {
            let _ = self.writer.write_all(b"\n");
            let _ = self.writer.flush();
        }
    }

    fn poll_outputs(&mut self) -> Vec<PanelOutput> {
        let mut outputs = std::mem::take(&mut self.pending_outputs);
        loop {
            match self.outputs.try_recv() {
                Ok(output) => {
                    self.log_output(&output);
                    outputs.push(output);
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        outputs
    }

    fn tick(&mut self) {
        let Some(runtime) = self.firmware.as_mut() else { return };
        if runtime.stopped { return; }
        match runtime.core.run(runtime.pc, INSTRUCTIONS_PER_TICK) {
            Ok(pc) => runtime.pc = pc | 1,
            Err(error) => {
                let pc = runtime.core.register(RegisterARM::PC).unwrap_or_default() as u32;
                eprintln!("[CPU] firmware stopped at {pc:#010x}: {error}");
                eprintln!("[AUDIO] {}", runtime.audio.borrow().summary());
                eprintln!("[USB] {}", runtime.usb.borrow().summary());
                if let Err(dump_error) = runtime.write_core_dump() {
                    eprintln!("[CPU] failed to write firmware core dump: {dump_error}");
                }
                runtime.stopped = true;
            }
        }
        // The scan block sets bit 15 when a scan starts. Until the matrix
        // channel/bit wiring is verified, complete each request with all
        // contacts open (zero samples) rather than inventing switch positions.
        let mut scan_status = [0u8; 2];
        if runtime.core.read_bytes(INPUT_SCAN_REGISTER, &mut scan_status).is_ok() {
            let status = u16::from_le_bytes(scan_status);
            if status & 0x8000 != 0 {
                let idle = (status & !0x8000).to_le_bytes();
                let _ = runtime.core.load_bytes(INPUT_SCAN_REGISTER, &idle);
            }
        }
        let writes = std::mem::take(&mut *runtime.gpio_writes.borrow_mut());
        for (address, size, value) in writes {
            if address != PORT_A_LOW || !(1..=2).contains(&size) { continue; }
            let old = self.registers.get(&PORT_A_LOW).copied().unwrap_or_default();
            let new = value as u16;
            self.registers.insert(PORT_A_LOW, new);
            if old & (1 << 12) == 0 && new & (1 << 12) != 0 {
                let rs = new & (1 << 11) != 0;
                let byte = ((new >> 3) & 0xff) as u8;
                if let Some(output) = self.lcd.write(rs, byte) {
                    self.log_output(&output);
                    self.pending_outputs.push(output);
                }
            }
        }
    }
}

impl FirmwareRuntime {
    fn load(root: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        const PAGE: usize = 0x1000;
        const RAM_SIZE: usize = 0x0013_4000;
        const SDRAM_SIZE: usize = 0x0020_0000;
        const STARTUP_RAM_SIZE: usize = 0x0039_4000;
        let mut core = ArmV7Core::new()?;
        for (base, size) in [
            (0x0000_0000, RAM_SIZE),
            (0x2000_0000, SDRAM_SIZE),
            (0x6800_0000, STARTUP_RAM_SIZE),
            (0x4000_0000, PAGE),
            (0x4000_1000, PAGE), // GPIO and input scan interface
            (0x4000_2000, PAGE), // observed startup peripheral, semantics unknown
            (0x4000_4000, PAGE),
            (0x4000_5000, PAGE), // USB host/device control word; ports disconnected
            (0x4000_8000, PAGE),
            (0x4000_9000, PAGE),
            (0x4000_a000, PAGE),
            (0xe000_e000, PAGE),
        ] { core.map_region(base, size, Prot::ALL)?; }
        crate::flash::attach(&mut core, &root.join("flash.bin"))?;
        core.set_register(RegisterARM::SP, RAM_SIZE as u64)?;
        for (filename, address) in [("driver-01000000.bin", DRIVER_BASE), ("firmware-0x60000000.bin", FIRMWARE_BASE)] {
            let bytes = fs::read(root.join(filename))?;
            let size = bytes.len().div_ceil(PAGE) * PAGE;
            core.map_region(address, size, Prot::ALL)?;
            core.load_bytes(address, &bytes)?;
            eprintln!("[CPU] loaded {} bytes from {filename} at {address:#010x}", bytes.len());
        }
        let gpio_writes = Rc::new(RefCell::new(Vec::new()));
        let hook_writes = Rc::clone(&gpio_writes);
        let trace = Rc::new(RefCell::new(VecDeque::with_capacity(64)));
        let hook_trace = Rc::clone(&trace);
        let breakpoints = Rc::new(RefCell::new(HashSet::new()));
        let hook_breakpoints = Rc::clone(&breakpoints);
        let breakpoint_hit = Rc::new(RefCell::new(None));
        let hook_breakpoint_hit = Rc::clone(&breakpoint_hit);
        core.engine_mut().add_code_hook(1, 0, move |engine, address, _| {
            let mut trace = hook_trace.borrow_mut();
            if trace.len() == 256 { trace.pop_front(); }
            trace.push_back(address as u32);
            drop(trace);
            if hook_breakpoints.borrow().contains(&(address as u32 & !1)) {
                *hook_breakpoint_hit.borrow_mut() = Some(address as u32 & !1);
                let _ = engine.emu_stop();
            }
        })?;
        core.engine_mut().add_mem_hook(HookType::MEM_WRITE, 0x4000_0100, 0x4000_0113, move |_, _, address, size, value| {
            hook_writes.borrow_mut().push((address as u32, size, value));
            false
        })?;
        let fault = Rc::new(RefCell::new(None));
        let hook_fault = Rc::clone(&fault);
        core.engine_mut().add_mem_hook(HookType::MEM_UNMAPPED, 1, 0, move |engine, kind, address, size, value| {
            let pc = engine.reg_read(RegisterARM::PC).unwrap_or_default() as u32;
            *hook_fault.borrow_mut() = Some((address as u32, size, value, pc, format!("{kind:?}")));
            false
        })?;
        let watch_hit = Rc::new(RefCell::new(None));
        let audio = crate::audio::attach(core.engine_mut())?;
        let usb = crate::usb::attach(core.engine_mut())?;
        crate::bitband::attach(&mut core, Rc::clone(&usb))?;
        eprintln!("[CPU] firmware running; SWX03 clock reference is {SWX03_CORE_CLOCK_HZ} Hz (not simulated; instruction engine is not cycle-accurate)");
        eprintln!("[AUDIO] dummy DAC (AK4430) and D.AMP (YDA176) sinks attached at 0x4000400c; selector bit 9 at 0x4000400e, set=SDO1/D.AMP");
        Ok(Self { core, pc: FIRMWARE_BASE | 1, gpio_writes, trace, breakpoints, breakpoint_hit, watch_hit, fault, audio, usb, root: root.to_path_buf(), stopped: false })
    }

    fn add_watchpoint(&mut self, address: u32, length: usize, read: bool, write: bool) -> Result<(), Box<dyn std::error::Error>> {
        let end = address.checked_add(length.saturating_sub(1) as u32).ok_or("watchpoint range overflow")?;
        let event = Rc::clone(&self.watch_hit);
        if read {
            let event = Rc::clone(&event);
            self.core.engine_mut().add_mem_hook(HookType::MEM_READ, address as u64, end as u64, move |engine, _, at, size, value| {
                *event.borrow_mut() = Some(format!("read address={at:#010x} size={size} value={value:#x}"));
                let _ = engine.emu_stop();
                false
            })?;
        }
        if write {
            self.core.engine_mut().add_mem_hook(HookType::MEM_WRITE, address as u64, end as u64, move |engine, _, at, size, value| {
                *event.borrow_mut() = Some(format!("write address={at:#010x} size={size} value={value:#x}"));
                let _ = engine.emu_stop();
                false
            })?;
        }
        Ok(())
    }

    fn write_core_dump(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut report = String::from("PSR-E463 live firmware core dump\n\n");
        if let Some((address, size, value, pc, kind)) = self.fault.borrow().as_ref() {
            report.push_str(&format!("fault: {kind} address={address:#010x} size={size} value={value:#x} pc={pc:#010x}\n\n"));
        }
        for (name, reg) in [
            ("r0", RegisterARM::R0), ("r1", RegisterARM::R1), ("r2", RegisterARM::R2),
            ("r3", RegisterARM::R3), ("r4", RegisterARM::R4), ("r5", RegisterARM::R5),
            ("r6", RegisterARM::R6), ("r7", RegisterARM::R7), ("r8", RegisterARM::R8),
            ("r9", RegisterARM::R9), ("r10", RegisterARM::R10), ("r11", RegisterARM::R11),
            ("r12", RegisterARM::R12), ("sp", RegisterARM::SP), ("lr", RegisterARM::LR),
            ("pc", RegisterARM::PC), ("cpsr", RegisterARM::CPSR),
        ] {
            report.push_str(&format!("{name:>4} = {:#010x}\n", self.core.register(reg)? as u32));
        }
        let sp = self.core.register(RegisterARM::SP)? as u32;
        let stack_start = sp.saturating_sub(0x1000) & !0xf;
        let mut stack = vec![0u8; 0x1000];
        if self.core.read_bytes(stack_start, &mut stack).is_ok() {
            report.push_str(&format!("\nstack memory at {stack_start:#010x}:\n"));
            for (offset, word) in stack.chunks_exact(4).enumerate() {
                if offset % 4 == 0 { report.push_str(&format!("\n{:#010x}: ", stack_start + (offset * 4) as u32)); }
                report.push_str(&format!("{:02x}{:02x}{:02x}{:02x} ", word[0], word[1], word[2], word[3]));
            }
            report.push('\n');
        }
        report.push_str("\nrecent instruction addresses (oldest first):\n");
        for address in self.trace.borrow().iter() { report.push_str(&format!("{address:#010x}\n")); }
        let dump_path = self.root.join("firmware-live-core-dump.txt");
        fs::write(&dump_path, report)?;
        for (name, base, size) in [
            ("ram", 0x0000_0000, 0x0013_4000usize),
            ("sdram", 0x2000_0000, 0x0020_0000usize),
            ("startup-ram", 0x6800_0000, 0x0039_4000usize),
        ] {
            let mut bytes = vec![0; size];
            self.core.read_bytes(base, &mut bytes)?;
            fs::write(self.root.join(format!("firmware-live-core-dump-{name}.bin")), bytes)?;
        }
        eprintln!("[CPU] wrote live firmware core dump {}", dump_path.display());
        Ok(())
    }
}

/// Run the firmware in a terminal debugger. This shares the same memory and
/// peripheral setup as the UI runtime, and never connects to physical hardware.
pub fn run_firmware_debugger(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut runtime = FirmwareRuntime::load(root)?;
    println!("Firmware debugger ready at {:#010x}. Type 'help' for commands.", runtime.pc & !1);
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut line = String::new();
    loop {
        print!("psr> ");
        io::stdout().flush()?;
        line.clear();
        if input.read_line(&mut line)? == 0 { break; }
        let mut args = line.split_whitespace();
        let Some(command) = args.next() else { continue };
        match command {
            "help" | "h" | "?" => println!("commands:\n  c [count]           continue up to count instructions (default 1,000,000)\n  s [count]           single-step (default 1)\n  r                   show CPU registers\n  x ADDRESS [length]  examine bytes (default 64, max 4096)\n  bp ADDRESS          add breakpoint\n  bc ADDRESS          clear breakpoint\n  bl                  list breakpoints\n  watch r|w|rw ADDRESS [length]  stop on memory access\n  bt                  scan the active stack for return addresses\n  trace [count]       show recent instruction addresses\n  dump                write the standard live core dump files\n  q                   quit"),
            "q" | "quit" => break,
            "r" | "regs" => print_debug_registers(&runtime)?,
            "bp" => match args.next().and_then(parse_debug_address) {
                Some(address) => { runtime.breakpoints.borrow_mut().insert(address & !1); println!("breakpoint set at {:#010x}", address & !1); }
                None => println!("usage: bp ADDRESS"),
            },
            "bc" => match args.next().and_then(parse_debug_address) {
                Some(address) => { if runtime.breakpoints.borrow_mut().remove(&(address & !1)) { println!("breakpoint cleared"); } else { println!("no breakpoint at {:#010x}", address & !1); } }
                None => println!("usage: bc ADDRESS"),
            },
            "bl" => {
                let points = runtime.breakpoints.borrow();
                if points.is_empty() { println!("no breakpoints"); }
                else { for address in points.iter() { println!("{address:#010x}"); } }
            }
            "x" => {
                let Some(address) = args.next().and_then(parse_debug_address) else { println!("usage: x ADDRESS [length]"); continue };
                let length = args.next().and_then(|v| v.parse::<usize>().ok()).unwrap_or(64).min(4096);
                let mut bytes = vec![0; length];
                match runtime.core.read_bytes(address, &mut bytes) {
                    Ok(()) => print_memory(address, &bytes),
                    Err(error) => println!("memory read failed: {error}"),
                }
            }
            "watch" => {
                let mode = args.next().unwrap_or("");
                let Some(address) = args.next().and_then(parse_debug_address) else { println!("usage: watch r|w|rw ADDRESS [length]"); continue };
                let length = args.next().and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, 4096);
                let read = mode.contains('r');
                let write = mode.contains('w');
                if !read && !write { println!("watch mode must be r, w or rw"); continue; }
                runtime.add_watchpoint(address, length, read, write)?;
                println!("watchpoint set: {mode} {address:#010x}..{:#010x}", address.saturating_add(length as u32 - 1));
            }
            "bt" => print_debug_backtrace(&runtime)?,
            "trace" => {
                let count = args.next().and_then(|v| v.parse::<usize>().ok()).unwrap_or(32).min(256);
                let trace = runtime.trace.borrow();
                for address in trace.iter().rev().take(count).collect::<Vec<_>>().into_iter().rev() { println!("{address:#010x}"); }
            }
            "dump" => runtime.write_core_dump()?,
            "s" | "step" | "c" | "continue" => {
                let step = command == "s" || command == "step";
                let count = args.next().and_then(|v| v.parse::<usize>().ok()).unwrap_or(if step { 1 } else { 1_000_000 }).max(1);
                if step {
                    let current = runtime.pc & !1;
                    let had_breakpoint = runtime.breakpoints.borrow_mut().remove(&current);
                    *runtime.breakpoint_hit.borrow_mut() = None;
                    let result = runtime.core.run(runtime.pc, count);
                    if had_breakpoint { runtime.breakpoints.borrow_mut().insert(current); }
                    runtime.pc = result.unwrap_or_else(|error| {
                        eprintln!("execution error: {error}");
                        runtime.core.register(RegisterARM::PC).unwrap_or_default() as u32
                    }) | 1;
                } else {
                    *runtime.breakpoint_hit.borrow_mut() = None;
                    *runtime.watch_hit.borrow_mut() = None;
                    runtime.pc = match runtime.core.run(runtime.pc, count) {
                        Ok(pc) => pc | 1,
                        Err(error) => {
                            eprintln!("execution error: {error}");
                            runtime.core.register(RegisterARM::PC).unwrap_or_default() as u32 | 1
                        }
                    };
                }
                if let Some(address) = runtime.breakpoint_hit.borrow_mut().take() { println!("stopped at breakpoint {address:#010x}"); }
                if let Some(event) = runtime.watch_hit.borrow_mut().take() { println!("watchpoint: {event}"); }
                println!("PC={:#010x}", runtime.pc & !1);
            }
            _ => println!("unknown command '{command}'; type 'help'"),
        }
    }
    Ok(())
}

fn parse_debug_address(value: &str) -> Option<u32> {
    u32::from_str_radix(value.trim_start_matches("0x").trim_start_matches("0X"), 16).ok()
}

fn print_debug_registers(runtime: &FirmwareRuntime) -> Result<(), Box<dyn std::error::Error>> {
    for (name, reg) in [
        ("r0", RegisterARM::R0), ("r1", RegisterARM::R1), ("r2", RegisterARM::R2), ("r3", RegisterARM::R3),
        ("r4", RegisterARM::R4), ("r5", RegisterARM::R5), ("r6", RegisterARM::R6), ("r7", RegisterARM::R7),
        ("r8", RegisterARM::R8), ("r9", RegisterARM::R9), ("r10", RegisterARM::R10), ("r11", RegisterARM::R11),
        ("r12", RegisterARM::R12), ("sp", RegisterARM::SP), ("lr", RegisterARM::LR), ("pc", RegisterARM::PC), ("cpsr", RegisterARM::CPSR),
    ] { println!("{name:>4} = {:#010x}", runtime.core.register(reg)? as u32); }
    Ok(())
}

fn print_memory(address: u32, bytes: &[u8]) {
    for (line, chunk) in bytes.chunks(16).enumerate() {
        print!("{:#010x}: ", address.saturating_add((line * 16) as u32));
        for byte in chunk { print!("{byte:02x} "); }
        println!();
    }
}

fn print_debug_backtrace(runtime: &FirmwareRuntime) -> Result<(), Box<dyn std::error::Error>> {
    let sp = runtime.core.register(RegisterARM::SP)? as u32;
    let lr = runtime.core.register(RegisterARM::LR)? as u32;
    println!("SP={sp:#010x} LR={lr:#010x}");
    let mut stack = vec![0; 0x1000];
    let start = sp.saturating_sub(stack.len() as u32) & !3;
    if runtime.core.read_bytes(start, &mut stack).is_err() { println!("stack is not readable"); return Ok(()); }
    for (offset, word) in stack.chunks_exact(4).enumerate() {
        let value = u32::from_le_bytes(word.try_into()?);
        if (0x6000_0000..0x6200_0000).contains(&value) || (0x0100_0000..0x0102_0000).contains(&value) {
            println!("possible return address at {:#010x}: {value:#010x}", start + (offset * 4) as u32);
        }
    }
    Ok(())
}

fn is_modeled_register(address: u32) -> bool {
    matches!(
        address,
        PORT_A_HIGH
            | PORT_A_MIDDLE
            | PORT_A_LOW
            | PORT_B_HIGH
            | PORT_B_LOW
            | PORT_C
            | INPUT_SCAN_REGISTER
    )
}

fn describe_pin_changes(address: u32, old: u16, new: u16) -> Vec<String> {
    if address != PORT_A_LOW {
        return Vec::new();
    }
    let mut changes = Vec::new();
    for bit in 0..16 {
        if (old ^ new) & (1 << bit) == 0 {
            continue;
        }
        let signal = match bit {
            2 => "PA2 (DAC reset)".to_owned(),
            3..=10 => format!("PA{bit} (LCD D{})", bit - 3),
            11 => "PA11 (LCD RS)".to_owned(),
            12 => "PA12 (LCD ENB)".to_owned(),
            _ => continue,
        };
        changes.push(format!(
            "{signal}={}",
            if new & (1 << bit) != 0 { 1 } else { 0 }
        ));
    }
    changes
}
