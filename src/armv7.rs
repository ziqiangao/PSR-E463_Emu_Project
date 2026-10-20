//! ARMv7-M little-endian instruction core backed by Unicorn Engine 2.
//!
//! This is an instruction executor, not a complete SWX03 machine. Memory and
//! device regions are mapped explicitly by the caller; no firmware is loaded
//! automatically.

use unicorn_engine::{
    uc_error,
    unicorn_const::{Arch, Mode, Prot},
    RegisterARM, Unicorn,
};

const PAGE_SIZE: u64 = 0x1000;

pub struct ArmV7Core {
    engine: Unicorn<'static, ()>,
}

impl ArmV7Core {
    /// Create a 32-bit little-endian ARMv7-M CPU context.
    pub fn new() -> Result<Self, uc_error> {
        let engine = Unicorn::new(Arch::ARM, Mode::MCLASS | Mode::LITTLE_ENDIAN)?;
        Ok(Self { engine })
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

    /// Execute at most `instruction_limit` ARM or Thumb instructions.
    /// Setting bit 0 of `pc` selects Thumb state; otherwise ARM state is used.
    /// Returns the resulting PC.
    pub fn run(&mut self, pc: u32, instruction_limit: usize) -> Result<u32, uc_error> {
        if instruction_limit == 0 {
            return Ok(pc);
        }
        let thumb = pc & 1 != 0;
        let address = u64::from(pc & !1);
        let mut cpsr = self.engine.reg_read(RegisterARM::CPSR)?;
        if thumb {
            cpsr |= 1 << 5;
        } else {
            cpsr &= !(1 << 5);
        }
        self.engine.reg_write(RegisterARM::CPSR, cpsr)?;
        let start = address | u64::from(thumb);
        self.engine.emu_start(start, 0, 0, instruction_limit)?;
        Ok(self.engine.reg_read(RegisterARM::PC)? as u32)
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
