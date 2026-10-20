//! Cortex-M peripheral bit-band alias handling.

use std::{cell::RefCell, rc::Rc};

use unicorn_engine::unicorn_const::{HookType, Prot};

use crate::{armv7::ArmV7Core, usb::VirtualUsb};

pub const PERIPHERAL_ALIAS_BASE: u32 = 0x4200_0000;
pub const PERIPHERAL_ALIAS_SIZE: usize = 0x0200_0000;
const PERIPHERAL_BASE: u64 = 0x4000_0000;

fn decode_alias(address: u64) -> Option<(u64, u8)> {
    let offset = address.checked_sub(u64::from(PERIPHERAL_ALIAS_BASE))?;
    if offset >= PERIPHERAL_ALIAS_SIZE as u64 {
        return None;
    }
    let target = PERIPHERAL_BASE + (offset >> 5);
    let bit = ((offset >> 2) & 7) as u8;
    Some((target, bit))
}

/// Map the Cortex-M peripheral alias window and translate bit-band loads and
/// stores to the corresponding byte/bit in the normal peripheral window.
pub fn attach(
    core: &mut ArmV7Core,
    usb: Rc<RefCell<VirtualUsb>>,
) -> Result<(), Box<dyn std::error::Error>> {
    core.map_region(PERIPHERAL_ALIAS_BASE, PERIPHERAL_ALIAS_SIZE, Prot::ALL)?;

    let base = u64::from(PERIPHERAL_ALIAS_BASE);
    let end = base + PERIPHERAL_ALIAS_SIZE as u64 - 1;

    core.engine_mut().add_mem_hook(
        HookType::MEM_WRITE,
        base,
        end,
        move |engine, _, address, _size, value| {
            let Some((target, bit)) = decode_alias(address) else {
                return false;
            };
            let mut byte = [0u8; 1];
            if engine.mem_read(target, &mut byte).is_ok() {
                if value & 1 != 0 {
                    byte[0] |= 1 << bit;
                } else {
                    byte[0] &= !(1 << bit);
                }
                let _ = engine.mem_write(target, &byte);

                if (u64::from(crate::usb::USB_CONTROL_REGISTER)
                    ..u64::from(crate::usb::USB_CONTROL_REGISTER + 4))
                    .contains(&target)
                {
                    let mut control = [0u8; 4];
                    if engine
                        .mem_read(u64::from(crate::usb::USB_CONTROL_REGISTER), &mut control)
                        .is_ok()
                    {
                        let control_value = u32::from_le_bytes(control);
                        usb.borrow_mut().write(
                            u64::from(crate::usb::USB_CONTROL_REGISTER),
                            4,
                            i64::from(control_value),
                        );
                    }
                }
            }
            false
        },
    )?;

    core.engine_mut().add_mem_hook(
        HookType::MEM_READ,
        base,
        end,
        move |engine, _, address, _size, _value| {
            let Some((target, bit)) = decode_alias(address) else {
                return false;
            };
            let mut byte = [0u8; 1];
            let alias_word = if engine.mem_read(target, &mut byte).is_ok() {
                u32::from((byte[0] >> bit) & 1)
            } else {
                0
            };
            let _ = engine.mem_write(address, &alias_word.to_le_bytes());
            false
        },
    )?;

    eprintln!(
        "[CPU] Cortex-M peripheral bit-band aliases mapped at {:#010x}..{:#010x}",
        PERIPHERAL_ALIAS_BASE,
        PERIPHERAL_ALIAS_BASE + PERIPHERAL_ALIAS_SIZE as u32
    );
    Ok(())
}
