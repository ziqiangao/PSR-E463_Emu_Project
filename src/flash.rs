//! File-backed subset of the PSR-E463's memory-mapped MX29GL128F flash.
//!
//! The physical part is 16 MiB, exposed as a complete window backed by
//! `flash.bin`. The command decoder implements the AMD-compatible x16
//! reset, ID, word-program, and erase sequences used by the firmware.

use std::{cell::RefCell, fs, path::Path, rc::Rc};

use unicorn_engine::unicorn_const::{HookType, Prot};

use crate::armv7::ArmV7Core;

pub const FLASH_BASE: u32 = 0x7000_0000;
pub const FLASH_SIZE: usize = 16 * 1024 * 1024;
const SECTOR_SIZE: usize = 128 * 1024;
const DEVICE_ID: u16 = 0x227e;

/// Return one byte from the MX29GL128F CFI query table. CFI data is presented
/// on the low byte lane of the x16 device; each table entry therefore occupies
/// a halfword in the memory-mapped window.
fn cfi_byte(offset: usize) -> u8 {
    let word_offset = offset & !1;
    let word = match word_offset {
        // Identification data: QRY, AMD command set, primary table at 0x40.
        0x20 => 0x0051,
        0x22 => 0x0052,
        0x24 => 0x0059,
        0x26 => 0x0002,
        0x28 => 0x0000,
        0x2a => 0x0040,
        0x2c..=0x34 => 0x0000,
        // System interface timing values used by the firmware's CFI parser.
        0x36 => 0x0027,
        0x38 => 0x0036,
        0x3a | 0x3c => 0x0000,
        0x3e => 0x0003,
        0x40 => 0x0006,
        0x42 => 0x0009,
        0x44 => 0x0013,
        0x46 => 0x0003,
        0x48 => 0x0005,
        0x4a => 0x0003,
        0x4c => 0x0002,
        // Device geometry: 128 Mbit, x8/x16, 64-byte write buffer,
        // one erase region containing 128 sectors of 128 KiB each.
        0x4e => 0x0018,
        0x50 => 0x0002,
        0x52 => 0x0000,
        0x54 => 0x0006,
        0x56 => 0x0000,
        0x58 => 0x0001,
        0x5a => 0x007f,
        0x5c => 0x0000,
        0x5e => 0x0000,
        0x60 => 0x0002,
        0x62..=0x78 => 0x0000,
        // Macronix primary extended query table (PRI, version 1.3).
        0x80 => 0x0050,
        0x82 => 0x0052,
        0x84 => 0x0049,
        0x86 => 0x0031,
        0x88 => 0x0033,
        0x8a => 0x0014,
        0x8c => 0x0002,
        0x8e => 0x0001,
        0x90 => 0x0000,
        0x92 => 0x0008,
        0x94 | 0x96 => 0x0000,
        0x98 => 0x0002,
        0x9a => 0x0095,
        0x9c => 0x00a5,
        0x9e => 0x0004,
        0xa0 => 0x0001,
        _ => 0x0000,
    };
    (word >> ((offset & 1) * 8)) as u8
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    ReadArray,
    Unlock1,
    Unlock2,
    AutoSelect,
    ProgramNext,
    EraseUnlock1,
    EraseUnlock2,
    EraseConfirm,
    Cfi,
}

struct FlashDevice {
    path: std::path::PathBuf,
    bytes: Vec<u8>,
    mode: Mode,
}

impl FlashDevice {
    fn open(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let mut bytes = if path.exists() {
            fs::read(path)?
        } else {
            Vec::new()
        };
        if bytes.len() > FLASH_SIZE {
            return Err(format!(
                "{} is {} bytes; this configured flash window is only {} bytes (refusing to truncate)",
                path.display(), bytes.len(), FLASH_SIZE
            ).into());
        }
        if bytes.len() < FLASH_SIZE {
            bytes.resize(FLASH_SIZE, 0xff);
            fs::write(path, &bytes)?;
        } else if !path.exists() {
            fs::write(path, &bytes)?;
        }
        eprintln!("[FLASH] using {} ({} bytes, erased fill 0xff)", path.display(), bytes.len());
        Ok(Self { path: path.to_path_buf(), bytes, mode: Mode::ReadArray })
    }

    fn write(&mut self, address: u32, size: usize, value: i64) {
        let offset = (address - FLASH_BASE) as usize;
        let data = value as u16;
        // Reset is accepted outside command sequences and returns the device
        // to array-read mode without changing stored data.
        if data == 0x00f0 {
            self.mode = Mode::ReadArray;
            eprintln!("[FLASH] reset command at {address:#010x}");
            return;
        }
        self.mode = match self.mode {
            Mode::ReadArray => if offset == 0x0aaa && data == 0x00aa { Mode::Unlock1 } else { Mode::ReadArray },
            Mode::Unlock1 => if offset == 0x0554 && data == 0x0055 { Mode::Unlock2 } else { Mode::ReadArray },
            Mode::Unlock2 => {
                if offset != 0x0aaa { Mode::ReadArray }
                else { match data {
                    0x0090 => { eprintln!("[FLASH] auto-select/ID mode entered"); Mode::AutoSelect },
                    0x00a0 => Mode::ProgramNext,
                    0x0080 => Mode::EraseUnlock1,
                    0x0098 => Mode::Cfi,
                    _ => Mode::ReadArray,
                }}
            }
            Mode::ProgramNext => {
                self.program(offset, size, value);
                Mode::ReadArray
            }
            Mode::EraseUnlock1 => if offset == 0x0aaa && data == 0x00aa { Mode::EraseUnlock2 } else { Mode::ReadArray },
            Mode::EraseUnlock2 => if offset == 0x0554 && data == 0x0055 { Mode::EraseConfirm } else { Mode::ReadArray },
            Mode::EraseConfirm => {
                if offset == 0x0aaa && data == 0x0010 {
                    self.erase_all();
                } else if data == 0x0030 {
                    self.erase_sector(offset);
                }
                Mode::ReadArray
            }
            Mode::AutoSelect => Mode::AutoSelect,
            Mode::Cfi => Mode::Cfi,
        };
    }

    fn program(&mut self, offset: usize, size: usize, value: i64) {
        if size != 2 || offset + 1 >= self.bytes.len() {
            eprintln!("[FLASH] ignored unsupported program width/range at {:#010x} size={size}", FLASH_BASE + offset as u32);
            return;
        }
        let previous = u16::from_le_bytes([self.bytes[offset], self.bytes[offset + 1]]);
        let programmed = previous & value as u16;
        self.bytes[offset..offset + 2].copy_from_slice(&programmed.to_le_bytes());
        self.persist();
    }

    fn erase_sector(&mut self, offset: usize) {
        let start = offset / SECTOR_SIZE * SECTOR_SIZE;
        let end = (start + SECTOR_SIZE).min(self.bytes.len());
        self.bytes[start..end].fill(0xff);
        self.persist();
        eprintln!("[FLASH] erased emulated sector {start:#08x}..{end:#08x}");
    }

    fn erase_all(&mut self) {
        self.bytes.fill(0xff);
        self.persist();
        eprintln!("[FLASH] erased the configured 16 MiB device");
    }

    fn persist(&self) {
        if let Err(error) = fs::write(&self.path, &self.bytes) {
            eprintln!("[FLASH] failed saving {}: {error}", self.path.display());
        }
    }

    fn read_data(&self, offset: usize, size: usize) -> Vec<u8> {
        let mut out = vec![0xff; size];
        for (i, byte) in out.iter_mut().enumerate() {
            let at = offset + i;
            *byte = match self.mode {
                Mode::AutoSelect => match at {
                    0 => 0xc2,
                    1 => 0,
                    2 => (DEVICE_ID & 0xff) as u8,
                    3 => (DEVICE_ID >> 8) as u8,
                    _ => 0,
                },
                Mode::Cfi => cfi_byte(at),
                _ => self.bytes.get(at).copied().unwrap_or(0xff),
            };
        }
        out
    }
}

/// Attach a persistent 16 MiB flash device at `0x70000000` to Unicorn.
/// Existing data is preserved; newly created bytes use the flash-erased value.
pub fn attach(
    core: &mut ArmV7Core,
    image_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let flash = Rc::new(RefCell::new(FlashDevice::open(image_path)?));
    core.map_region(FLASH_BASE, FLASH_SIZE, Prot::ALL)?;
    core.load_bytes(FLASH_BASE, &flash.borrow().bytes)?;

    let read_flash = Rc::clone(&flash);
    core.engine_mut().add_mem_hook(
        HookType::MEM_READ,
        u64::from(FLASH_BASE),
        u64::from(FLASH_BASE) + FLASH_SIZE as u64 - 1,
        move |engine, _, address, size, _| {
            let offset = (address as u32 - FLASH_BASE) as usize;
            let bytes = read_flash.borrow().read_data(offset, size);
            let _ = engine.mem_write(address, &bytes);
            false
        },
    )?;

    let write_flash = Rc::clone(&flash);
    core.engine_mut().add_mem_hook(
        HookType::MEM_WRITE,
        u64::from(FLASH_BASE),
        u64::from(FLASH_BASE) + FLASH_SIZE as u64 - 1,
        move |_, _, address, size, value| {
            let mut flash = write_flash.borrow_mut();
            flash.write(address as u32, size, value);
            false
        },
    )?;
    Ok(())
}
