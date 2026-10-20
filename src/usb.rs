//! Disconnected virtual USB ports for the PSR-E463 firmware runtime.
//!
//! Ghidra evidence suggests 0x40005028 is a shared USB control/enable word:
//! bit 1 is set in the USB000 storage-volume path and bit 2 in the "Digital
//! Keyboard" USB-device descriptor setup. The exact register definition is
//! not available, so these assignments remain provisional.

use std::{cell::RefCell, rc::Rc};

use unicorn_engine::{uc_error, unicorn_const::HookType, Unicorn};

pub const USB_REGISTER_PAGE: u64 = 0x4000_5000;
pub const USB_CONTROL_REGISTER: u64 = 0x4000_5028;
pub const USB_HOST_ENABLE_BIT: u32 = 1 << 1;
pub const USB_DEVICE_ENABLE_BIT: u32 = 1 << 2;

#[derive(Debug)]
struct VirtualPort {
    name: &'static str,
    role: &'static str,
    function: &'static str,
    enabled: bool,
    connected: bool,
}

#[derive(Debug)]
pub struct VirtualUsb {
    control: u32,
    to_device: VirtualPort,
    to_host: VirtualPort,
}

impl Default for VirtualUsb {
    fn default() -> Self {
        Self {
            control: 0,
            to_device: VirtualPort {
                name: "USB TO DEVICE",
                role: "host",
                function: "USB mass storage",
                enabled: false,
                connected: false,
            },
            to_host: VirtualPort {
                name: "USB TO HOST",
                role: "device",
                function: "USB MIDI/audio",
                enabled: false,
                connected: false,
            },
        }
    }
}

impl VirtualUsb {
    pub(crate) fn write(&mut self, address: u64, size: usize, value: i64) {
        if address != USB_CONTROL_REGISTER || size != 4 {
            return;
        }

        let old_host = self.to_device.enabled;
        let old_device = self.to_host.enabled;
        self.control = value as u32;
        self.to_device.enabled = self.control & USB_HOST_ENABLE_BIT != 0;
        self.to_host.enabled = self.control & USB_DEVICE_ENABLE_BIT != 0;

        if old_host != self.to_device.enabled {
            log_port(&self.to_device);
        }
        if old_device != self.to_host.enabled {
            log_port(&self.to_host);
        }
    }

    pub fn summary(&self) -> String {
        format!(
            "{}: {} / {}; {}: {} / {}; control={:#010x}",
            self.to_device.name,
            if self.to_device.enabled {
                "enabled"
            } else {
                "disabled"
            },
            if self.to_device.connected {
                "connected"
            } else {
                "disconnected"
            },
            self.to_host.name,
            if self.to_host.enabled {
                "enabled"
            } else {
                "disabled"
            },
            if self.to_host.connected {
                "connected"
            } else {
                "disconnected"
            },
            self.control,
        )
    }
}

fn log_port(port: &VirtualPort) {
    eprintln!(
        "[USB] {} {} port initialized for {}; no device attached",
        port.name, port.role, port.function
    );
}

/// Observe the firmware's shared USB control word. The mapped backing page
/// supplies ordinary register storage; neither virtual port reports a device.
pub fn attach(engine: &mut Unicorn<'static, ()>) -> Result<Rc<RefCell<VirtualUsb>>, uc_error> {
    let usb = Rc::new(RefCell::new(VirtualUsb::default()));
    let hook_usb = Rc::clone(&usb);
    engine.add_mem_hook(
        HookType::MEM_WRITE,
        USB_CONTROL_REGISTER,
        USB_CONTROL_REGISTER,
        move |_, _, address, size, value| {
            hook_usb.borrow_mut().write(address, size, value);
            false
        },
    )?;
    eprintln!(
        "[USB] virtual ports attached: TO DEVICE=host/mass-storage, TO HOST=device/MIDI+audio; both disconnected"
    );
    Ok(usb)
}
