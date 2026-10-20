//! ARMv7-M little-endian instruction core backed by Unicorn Engine 2.
//!
//! This is an instruction executor, not a complete SWX03 machine. Memory and
//! device regions are mapped explicitly by the caller; no firmware is loaded
//! automatically.

use std::{
    cell::{Cell, RefCell},
    fmt,
    rc::Rc,
};
use unicorn_engine::{
    uc_error,
    unicorn_const::{Arch, HookType, Mode, Prot},
    RegisterARM, Unicorn,
};

const PAGE_SIZE: u64 = 0x1000;
const SCS_BASE: u32 = 0xe000_e000;
const SCS_END: u32 = 0xe000_efff;
const SCB_ICSR: u32 = 0xe000_ed04;
const SCB_VTOR: u32 = 0xe000_ed08;
const SCB_SHPR3: u32 = 0xe000_ed20;
const SCB_SHPR3_PENDSV: u32 = SCB_SHPR3 + 2;
const SCB_SHPR3_SYSTICK: u32 = SCB_SHPR3 + 3;
const NVIC_ISER: u32 = 0xe000_e100;
const NVIC_ICER: u32 = 0xe000_e180;
const NVIC_ISPR: u32 = 0xe000_e200;
const NVIC_ICPR: u32 = 0xe000_e280;
const NVIC_IPR: u32 = 0xe000_e400;
const NVIC_WORDS: usize = 8;
const EXC_RETURN_HANDLER_MSP: u32 = 0xffff_fff1;
const EXC_RETURN_THREAD_MSP: u32 = 0xffff_fff9;
const EXC_RETURN_THREAD_PSP: u32 = 0xffff_fffd;
const SVC_EXCEPTION: u16 = 11;
const PENDSV_EXCEPTION: u16 = 14;
const SYSTICK_EXCEPTION: u16 = 15;
const BASIC_FRAME_BYTES: u32 = 8 * 4;

pub struct ArmV7Core {
    engine: Unicorn<'static, ()>,
    interrupts: Rc<RefCell<InterruptController>>,
    interrupt_event: Rc<RefCell<Option<ExceptionEvent>>>,
    execution_count: Rc<Cell<usize>>,
    execution_limit: Rc<Cell<usize>>,
}

enum ExceptionEvent {
    Svc,
    Return,
    Pending(u16),
    Unsupported(u32),
    Failure(String),
}

#[derive(Debug)]
pub enum CoreRunError {
    Unicorn(uc_error),
    Exception(String),
}

impl fmt::Display for CoreRunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unicorn(error) => write!(formatter, "Unicorn execution failed: {error}"),
            Self::Exception(error) => write!(formatter, "Cortex-M exception failed: {error}"),
        }
    }
}

impl std::error::Error for CoreRunError {}

struct InterruptController {
    enabled: [u32; NVIC_WORDS],
    pending: [u32; NVIC_WORDS],
    priorities: Vec<u8>,
    pend_sv: bool,
    pend_systick: bool,
    pendsv_priority: u8,
    systick_priority: u8,
}

impl Default for InterruptController {
    fn default() -> Self {
        Self {
            enabled: [0; NVIC_WORDS],
            pending: [0; NVIC_WORDS],
            priorities: vec![0; NVIC_WORDS * 32],
            pend_sv: false,
            pend_systick: false,
            pendsv_priority: 0,
            systick_priority: 0,
        }
    }
}

impl InterruptController {
    fn write_register(&mut self, address: u32, size: usize, value: i64) {
        let value = value as u64;
        for byte_offset in 0..size.min(8) {
            let at = address + byte_offset as u32;
            let byte = (value >> (byte_offset * 8)) as u8;
            match at {
                NVIC_ISER..=0xe000_e11f => {
                    let index = ((at - NVIC_ISER) / 4) as usize;
                    if index < NVIC_WORDS {
                        self.enabled[index] |= u32::from(byte) << ((at & 3) * 8);
                    }
                }
                NVIC_ICER..=0xe000_e19f => {
                    let index = ((at - NVIC_ICER) / 4) as usize;
                    if index < NVIC_WORDS {
                        self.enabled[index] &= !(u32::from(byte) << ((at & 3) * 8));
                    }
                }
                NVIC_ISPR..=0xe000_e21f => {
                    let index = ((at - NVIC_ISPR) / 4) as usize;
                    if index < NVIC_WORDS {
                        self.pending[index] |= u32::from(byte) << ((at & 3) * 8);
                    }
                }
                NVIC_ICPR..=0xe000_e29f => {
                    let index = ((at - NVIC_ICPR) / 4) as usize;
                    if index < NVIC_WORDS {
                        self.pending[index] &= !(u32::from(byte) << ((at & 3) * 8));
                    }
                }
                NVIC_IPR..=0xe000_e4ff => {
                    let index = (at - NVIC_IPR) as usize;
                    if index < self.priorities.len() {
                        self.priorities[index] = byte;
                    }
                }
                SCB_ICSR if byte_offset == 0 => {
                    let word = value as u32;
                    if word & (1 << 28) != 0 {
                        self.pend_sv = true;
                    }
                    if word & (1 << 27) != 0 {
                        self.pend_sv = false;
                    }
                    if word & (1 << 26) != 0 {
                        self.pend_systick = true;
                    }
                    if word & (1 << 25) != 0 {
                        self.pend_systick = false;
                    }
                }
                SCB_SHPR3_PENDSV => self.pendsv_priority = byte,
                SCB_SHPR3_SYSTICK => self.systick_priority = byte,
                _ => {}
            }
        }
    }

    fn has_pending(&self) -> bool {
        self.pend_sv
            || self.pend_systick
            || self
                .pending
                .iter()
                .zip(self.enabled)
                .any(|(pending, enabled)| pending & enabled != 0)
    }

    fn take_pending(&mut self, base_priority: u8) -> Option<u16> {
        let is_unmasked = |priority: u8| base_priority == 0 || priority < base_priority;
        let mut selected: Option<(u8, u16)> = None;
        if self.pend_sv && is_unmasked(self.pendsv_priority) {
            selected = Some((self.pendsv_priority, PENDSV_EXCEPTION));
        }
        if self.pend_systick && is_unmasked(self.systick_priority) {
            let candidate = (self.systick_priority, SYSTICK_EXCEPTION);
            if selected.map_or(true, |current| candidate < current) {
                selected = Some(candidate);
            }
        }
        for (word_index, (pending, enabled)) in self.pending.iter().zip(self.enabled).enumerate() {
            let ready = *pending & enabled;
            for bit in 0..32 {
                if ready & (1 << bit) == 0 {
                    continue;
                }
                let irq = word_index * 32 + bit;
                let candidate = (self.priorities[irq], (irq + 16) as u16);
                if is_unmasked(candidate.0) && selected.map_or(true, |current| candidate < current)
                {
                    selected = Some(candidate);
                }
            }
        }
        let (_, exception) = selected?;
        match exception {
            PENDSV_EXCEPTION => self.pend_sv = false,
            SYSTICK_EXCEPTION => self.pend_systick = false,
            number => {
                let irq = usize::from(number - 16);
                self.pending[irq / 32] &= !(1 << (irq % 32));
            }
        }
        Some(exception)
    }

    fn pend_irq(&mut self, irq: u16) -> Result<(), &'static str> {
        let irq = usize::from(irq);
        if irq >= NVIC_WORDS * 32 {
            return Err("external interrupt number exceeds the modeled NVIC range");
        }
        self.pending[irq / 32] |= 1 << (irq % 32);
        Ok(())
    }
}

impl ArmV7Core {
    /// Create a 32-bit little-endian ARMv7-M CPU context.
    pub fn new() -> Result<Self, uc_error> {
        let engine = Unicorn::new(Arch::ARM, Mode::MCLASS | Mode::LITTLE_ENDIAN)?;
        let mut core = Self {
            engine,
            interrupts: Rc::new(RefCell::new(InterruptController::default())),
            interrupt_event: Rc::new(RefCell::new(None)),
            execution_count: Rc::new(Cell::new(0)),
            execution_limit: Rc::new(Cell::new(0)),
        };
        core.install_exception_hooks()?;
        Ok(core)
    }

    /// Map a page-aligned region into the guest address space.
    pub fn map_region(
        &mut self,
        address: u32,
        size: usize,
        permissions: Prot,
    ) -> Result<(), uc_error> {
        let address = u64::from(address);
        let size = u64::try_from(size).map_err(|_| uc_error::ARG)?;
        if address % PAGE_SIZE != 0 || size == 0 || size % PAGE_SIZE != 0 {
            return Err(uc_error::ARG);
        }
        self.engine.mem_map(address, size, permissions)
    }

    /// Copy bytes into a region that the caller has already mapped.
    pub fn load_bytes(&mut self, address: u32, bytes: &[u8]) -> Result<(), uc_error> {
        self.engine.mem_write(u64::from(address), bytes)
    }

    pub fn read_bytes(&self, address: u32, output: &mut [u8]) -> Result<(), uc_error> {
        self.engine.mem_read(u64::from(address), output)
    }

    pub fn register(&self, register: RegisterARM) -> Result<u64, uc_error> {
        self.engine.reg_read(register)
    }

    pub fn set_register(&mut self, register: RegisterARM, value: u64) -> Result<(), uc_error> {
        self.engine.reg_write(register, value)
    }

    /// Execute at most `instruction_limit` ARMv7-M Thumb instructions.
    /// Returns the resulting PC.
    pub fn run(&mut self, pc: u32, instruction_limit: usize) -> Result<u32, CoreRunError> {
        if instruction_limit == 0 {
            return Ok(pc);
        }
        let address = u64::from(pc & !1);
        let mut cpsr = self
            .engine
            .reg_read(RegisterARM::CPSR)
            .map_err(CoreRunError::Unicorn)?;
        cpsr |= 1 << 5;
        self.engine
            .reg_write(RegisterARM::CPSR, cpsr)
            .map_err(CoreRunError::Unicorn)?;
        self.execution_count.set(0);
        self.execution_limit.set(instruction_limit);
        let mut start = address | 1;
        loop {
            let executed = self.execution_count.get();
            if executed >= instruction_limit {
                break;
            }
            let cpsr = self
                .engine
                .reg_read(RegisterARM::CPSR)
                .map_err(CoreRunError::Unicorn)?;
            self.engine
                .reg_write(RegisterARM::CPSR, cpsr | (1 << 5))
                .map_err(CoreRunError::Unicorn)?;
            self.engine
                .emu_start(start, 0, 0, instruction_limit - executed)
                .map_err(CoreRunError::Unicorn)?;
            let event = self.interrupt_event.borrow_mut().take();
            let Some(event) = event else {
                break;
            };
            match event {
                ExceptionEvent::Svc => enter_exception(&mut self.engine, SVC_EXCEPTION)
                    .map_err(CoreRunError::Exception)?,
                ExceptionEvent::Return => {
                    return_from_exception(&mut self.engine).map_err(CoreRunError::Exception)?
                }
                ExceptionEvent::Pending(exception) => {
                    enter_exception(&mut self.engine, exception).map_err(CoreRunError::Exception)?
                }
                ExceptionEvent::Unsupported(number) => {
                    return Err(CoreRunError::Exception(format!(
                        "unsupported Unicorn ARM interrupt {number}"
                    )))
                }
                ExceptionEvent::Failure(error) => return Err(CoreRunError::Exception(error)),
            }
            let current_pc = self
                .engine
                .reg_read(RegisterARM::PC)
                .map_err(CoreRunError::Unicorn)? as u32;
            start = u64::from(current_pc & !1) | 1;
        }
        self.engine
            .reg_read(RegisterARM::PC)
            .map(|pc| pc as u32)
            .map_err(CoreRunError::Unicorn)
    }

    pub fn pend_interrupt(&mut self, irq: u16) -> Result<(), &'static str> {
        self.interrupts.borrow_mut().pend_irq(irq)
    }

    pub fn interrupt_pender(
        &self,
    ) -> impl Fn(u16) -> Result<(), &'static str> + 'static {
        let interrupts = Rc::clone(&self.interrupts);
        move |irq| interrupts.borrow_mut().pend_irq(irq)
    }

    fn install_exception_hooks(&mut self) -> Result<(), uc_error> {
        let interrupts = Rc::clone(&self.interrupts);
        self.engine.add_mem_hook(
            HookType::MEM_WRITE,
            u64::from(SCS_BASE),
            u64::from(SCS_END),
            move |_, _, address, size, value| {
                interrupts
                    .borrow_mut()
                    .write_register(address as u32, size, value);
                false
            },
        )?;

        let interrupts = Rc::clone(&self.interrupts);
        let interrupt_event = Rc::clone(&self.interrupt_event);
        let execution_count = Rc::clone(&self.execution_count);
        let execution_limit = Rc::clone(&self.execution_limit);
        self.engine.add_code_hook(1, 0, move |engine, _, _| {
            if execution_count.get() >= execution_limit.get() {
                let _ = engine.emu_stop();
                return;
            }
            if interrupts.borrow().has_pending() {
                let masks = (
                    engine.reg_read(RegisterARM::PRIMASK),
                    engine.reg_read(RegisterARM::IPSR),
                    engine.reg_read(RegisterARM::BASEPRI),
                );
                let (primask, ipsr, basepri) = match masks {
                    (Ok(primask), Ok(ipsr), Ok(basepri)) => (primask, ipsr, basepri as u8),
                    (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => {
                        *interrupt_event.borrow_mut() = Some(ExceptionEvent::Failure(format!(
                            "could not inspect Cortex-M interrupt state: {error}"
                        )));
                        let _ = engine.emu_stop();
                        return;
                    }
                };
                if primask & 1 == 0 && ipsr == 0 {
                    if let Some(exception) = interrupts.borrow_mut().take_pending(basepri) {
                        *interrupt_event.borrow_mut() = Some(ExceptionEvent::Pending(exception));
                        let _ = engine.emu_stop();
                        return;
                    }
                }
            }
            execution_count.set(execution_count.get() + 1);
        })?;

        let interrupt_event = Rc::clone(&self.interrupt_event);
        self.engine.add_intr_hook(move |engine, interrupt| {
            let event = match interrupt {
                2 => ExceptionEvent::Svc,
                8 => ExceptionEvent::Return,
                number => ExceptionEvent::Unsupported(number),
            };
            *interrupt_event.borrow_mut() = Some(event);
            let _ = engine.emu_stop();
        })?;
        Ok(())
    }

    /// Read or write the guest's 32-bit general register bank through the
    /// underlying Unicorn API, including PC, SP, LR and CPSR.
    pub fn engine(&self) -> &Unicorn<'static, ()> {
        &self.engine
    }

    pub fn engine_mut(&mut self) -> &mut Unicorn<'static, ()> {
        &mut self.engine
    }
}

fn enter_exception<'a>(engine: &mut Unicorn<'a, ()>, exception: u16) -> Result<(), String> {
    let old_ipsr = read_register(engine, RegisterARM::IPSR)?;
    let control = read_register(engine, RegisterARM::CONTROL)?;
    let use_psp = old_ipsr == 0 && control & 2 != 0;
    let stack_register = if use_psp {
        RegisterARM::PSP
    } else {
        RegisterARM::MSP
    };
    let old_sp = read_register(engine, stack_register)? as u32;
    let pc = read_register(engine, RegisterARM::PC)? as u32 & !1;
    let xpsr = read_register(engine, RegisterARM::XPSR)? as u32 | (1 << 24);
    let padding = u32::from(old_sp & 4 != 0);
    let frame_sp = old_sp
        .checked_sub(BASIC_FRAME_BYTES + padding * 4)
        .ok_or_else(|| "exception stack underflow".to_owned())?;
    let frame = [
        read_register(engine, RegisterARM::R0)? as u32,
        read_register(engine, RegisterARM::R1)? as u32,
        read_register(engine, RegisterARM::R2)? as u32,
        read_register(engine, RegisterARM::R3)? as u32,
        read_register(engine, RegisterARM::R12)? as u32,
        read_register(engine, RegisterARM::LR)? as u32,
        pc,
        xpsr | (padding << 9),
    ];
    for (index, word) in frame.iter().enumerate() {
        write_u32(engine, frame_sp + (index as u32 * 4), *word)?;
    }
    if padding != 0 {
        write_u32(engine, frame_sp + BASIC_FRAME_BYTES, 0)?;
    }
    write_register(engine, stack_register, u64::from(frame_sp))?;

    let vtor = read_u32(engine, SCB_VTOR)? & !0x7f;
    let vector = read_u32(engine, vtor + u32::from(exception) * 4)?;
    if vector & 1 == 0 {
        return Err(format!(
            "exception {exception} has an invalid vector {vector:#010x} at VTOR {vtor:#010x}"
        ));
    }
    let exc_return = if old_ipsr != 0 {
        EXC_RETURN_HANDLER_MSP
    } else if use_psp {
        EXC_RETURN_THREAD_PSP
    } else {
        EXC_RETURN_THREAD_MSP
    };
    write_register(engine, RegisterARM::LR, u64::from(exc_return))?;
    write_register(engine, RegisterARM::IPSR, u64::from(exception))?;
    let handler_xpsr = read_register(engine, RegisterARM::XPSR)? | (1 << 24);
    write_register(engine, RegisterARM::XPSR, handler_xpsr)?;
    let cpsr = read_register(engine, RegisterARM::CPSR)? | (1 << 5);
    write_register(engine, RegisterARM::CPSR, cpsr)?;
    write_register(engine, RegisterARM::PC, u64::from(vector & !1))?;
    Ok(())
}

fn return_from_exception<'a>(engine: &mut Unicorn<'a, ()>) -> Result<(), String> {
    let exc_return = read_register(engine, RegisterARM::LR)? as u32;
    if exc_return & 0xffff_ffe0 != 0xffff_ffe0 || exc_return & 1 == 0 {
        return Err(format!("invalid exception-return token {exc_return:#010x}"));
    }
    let stack_register = if exc_return & 4 != 0 {
        RegisterARM::PSP
    } else {
        RegisterARM::MSP
    };
    let frame_sp = read_register(engine, stack_register)? as u32;
    let mut frame = [0_u32; 8];
    for (index, word) in frame.iter_mut().enumerate() {
        *word = read_u32(engine, frame_sp + (index as u32 * 4))?;
    }
    let padding = u32::from(frame[7] & (1 << 9) != 0);
    for (register, value) in [
        (RegisterARM::R0, frame[0]),
        (RegisterARM::R1, frame[1]),
        (RegisterARM::R2, frame[2]),
        (RegisterARM::R3, frame[3]),
        (RegisterARM::R12, frame[4]),
        (RegisterARM::LR, frame[5]),
    ] {
        write_register(engine, register, u64::from(value))?;
    }
    write_register(
        engine,
        stack_register,
        u64::from(frame_sp + BASIC_FRAME_BYTES + padding * 4),
    )?;
    write_register(engine, RegisterARM::XPSR, u64::from(frame[7]))?;
    let cpsr = read_register(engine, RegisterARM::CPSR)?;
    let cpsr = if frame[7] & (1 << 24) != 0 {
        cpsr | (1 << 5)
    } else {
        cpsr & !(1 << 5)
    };
    write_register(engine, RegisterARM::CPSR, cpsr)?;
    write_register(engine, RegisterARM::PC, u64::from(frame[6] & !1))?;
    Ok(())
}

fn read_register(engine: &Unicorn<'_, ()>, register: RegisterARM) -> Result<u64, String> {
    engine
        .reg_read(register)
        .map_err(|error| format!("could not read {register:?}: {error}"))
}

fn write_register(
    engine: &mut Unicorn<'_, ()>,
    register: RegisterARM,
    value: u64,
) -> Result<(), String> {
    engine
        .reg_write(register, value)
        .map_err(|error| format!("could not write {register:?}: {error}"))
}

fn read_u32(engine: &Unicorn<'_, ()>, address: u32) -> Result<u32, String> {
    let mut bytes = [0; 4];
    engine
        .mem_read(u64::from(address), &mut bytes)
        .map_err(|error| format!("could not read exception memory at {address:#010x}: {error}"))?;
    Ok(u32::from_le_bytes(bytes))
}

fn write_u32(engine: &mut Unicorn<'_, ()>, address: u32, value: u32) -> Result<(), String> {
    engine
        .mem_write(u64::from(address), &value.to_le_bytes())
        .map_err(|error| format!("could not write exception frame at {address:#010x}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svc_dispatches_through_vtor_and_returns_to_the_stacked_thread() {
        let mut core = ArmV7Core::new().unwrap();
        core.map_region(0x1000, PAGE_SIZE as usize, Prot::ALL)
            .unwrap();
        core.map_region(0x2000, PAGE_SIZE as usize, Prot::ALL)
            .unwrap();
        core.map_region(u32::from(SCS_BASE), PAGE_SIZE as usize, Prot::ALL)
            .unwrap();
        core.load_bytes(0x1000, &[0x00, 0xdf, 0x2a, 0x20]).unwrap();
        core.load_bytes(0x1100, &[0x00, 0xb5, 0x00, 0xbd]).unwrap();
        core.load_bytes(SCB_VTOR, &0x2000_u32.to_le_bytes())
            .unwrap();
        core.load_bytes(
            0x2000 + u32::from(SVC_EXCEPTION) * 4,
            &0x1101_u32.to_le_bytes(),
        )
        .unwrap();
        core.set_register(RegisterARM::MSP, 0x1f00).unwrap();
        core.set_register(RegisterARM::PSP, 0x1ff4).unwrap();
        core.set_register(RegisterARM::CONTROL, 2).unwrap();

        assert_eq!(core.run(0x1001, 4).unwrap(), 0x1004);
        assert_eq!(core.register(RegisterARM::R0).unwrap(), 0x2a);
        assert_eq!(core.register(RegisterARM::IPSR).unwrap(), 0);
        assert_eq!(core.register(RegisterARM::PSP).unwrap(), 0x1ff4);
    }

    #[test]
    fn enabled_external_irq_uses_vtor_and_restores_interrupted_task() {
        let mut core = ArmV7Core::new().unwrap();
        core.map_region(0x1000, PAGE_SIZE as usize, Prot::ALL)
            .unwrap();
        core.map_region(0x2000, PAGE_SIZE as usize, Prot::ALL)
            .unwrap();
        core.map_region(u32::from(SCS_BASE), PAGE_SIZE as usize, Prot::ALL)
            .unwrap();
        let code = [
            0x02, 0x48, // ldr r0, [pc, #8]
            0x01, 0x21, // movs r1, #1
            0x01, 0x60, // str r1, [r0]
            0xc0, 0x46, // nop
            0xfe, 0xe7, // b .
        ];
        core.load_bytes(0x1000, &code).unwrap();
        core.load_bytes(0x100c, &NVIC_ISER.to_le_bytes()).unwrap();
        core.load_bytes(0x1100, &[0x70, 0x47]).unwrap();
        core.load_bytes(SCB_VTOR, &0x2000_u32.to_le_bytes())
            .unwrap();
        core.load_bytes(0x2000 + 16 * 4, &0x1101_u32.to_le_bytes())
            .unwrap();
        core.set_register(RegisterARM::MSP, 0x1ff0).unwrap();
        core.pend_interrupt(0).unwrap();

        core.run(0x1001, 5).unwrap();

        assert_eq!(core.register(RegisterARM::IPSR).unwrap(), 0);
        assert_eq!(core.register(RegisterARM::PC).unwrap(), 0x1008);
        assert_eq!(core.register(RegisterARM::MSP).unwrap(), 0x1ff0);
    }
}
