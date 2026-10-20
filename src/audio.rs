//! Dummy SWX03 serial audio output sinks.
//!
//! Register routing is provisional: bit 9 of 0x4000400e is treated as the
//! SDO0/SDO1 selector, and writes to 0x4000400c are forwarded as opaque words.

use std::cell::RefCell;
use std::rc::Rc;

use unicorn_engine::{uc_error, unicorn_const::HookType, Unicorn};

pub const AUDIO_REGISTER_BASE: u64 = 0x4000_4008;
pub const AUDIO_DATA_REGISTER: u64 = 0x4000_400c;
pub const AUDIO_CONTROL_REGISTER: u64 = 0x4000_400e;
const SDO1_SELECT: u16 = 1 << 9;
const RECENT_WORD_LIMIT: usize = 32;

#[derive(Debug, Default)]
struct DummySink {
    words_received: u64,
    recent_words: Vec<u16>,
}

impl DummySink {
    fn push(&mut self, word: u16) {
        self.words_received = self.words_received.wrapping_add(1);
        if self.recent_words.len() == RECENT_WORD_LIMIT {
            self.recent_words.remove(0);
        }
        self.recent_words.push(word);
    }
}

#[derive(Debug, Default)]
pub struct VirtualAudioOutputs {
    control: u16,
    sdo0_dac: DummySink,
    sdo1_digital_amp: DummySink,
}

impl VirtualAudioOutputs {
    fn write(&mut self, address: u64, size: usize, value: i64) {
        let value = value as u16;
        match address {
            AUDIO_CONTROL_REGISTER => {
                let old_route = self.control & SDO1_SELECT;
                self.control = value;
                let new_route = self.control & SDO1_SELECT;
                if old_route != new_route {
                    let destination = if new_route == 0 { "SDO0/DAC" } else { "SDO1/D.AMP" };
                    eprintln!("[AUDIO] 0x4000400e bit 9 selects {destination} (dummy sink)");
                }
            }
            AUDIO_DATA_REGISTER => {
                if size == 2 {
                    if self.control & SDO1_SELECT == 0 {
                        self.sdo0_dac.push(value);
                    } else {
                        self.sdo1_digital_amp.push(value);
                    }
                }
            }
            _ => {}
        }
    }

    pub fn summary(&self) -> String {
        format!(
            "SDO0/DAC dummy words={}, recent={:04x?}; SDO1/D.AMP dummy words={}, recent={:04x?}",
            self.sdo0_dac.words_received,
            self.sdo0_dac.recent_words,
            self.sdo1_digital_amp.words_received,
            self.sdo1_digital_amp.recent_words,
        )
    }
}

/// Hook writes to the observed audio transfer registers. The MMIO page remains
/// mapped by the caller; this hook only observes and routes the data register.
pub fn attach(
    engine: &mut Unicorn<'static, ()>,
) -> Result<Rc<RefCell<VirtualAudioOutputs>>, uc_error> {
    let outputs = Rc::new(RefCell::new(VirtualAudioOutputs::default()));
    let hook_outputs = Rc::clone(&outputs);
    engine.add_mem_hook(
        HookType::MEM_WRITE,
        AUDIO_REGISTER_BASE,
        AUDIO_CONTROL_REGISTER,
        move |_, _, address, size, value| {
            hook_outputs.borrow_mut().write(address, size, value);
            false
        },
    )?;
    Ok(outputs)
}
