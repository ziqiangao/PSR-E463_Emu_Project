//! Software model of the OKI ML9040A-B01GA character LCD controller.
//!
//! The host-side model accepts the controller's RS/data-byte writes. It models
//! the instruction register, address counter, display data RAM, character
//! generator RAM and the common display-control commands. It does not model
//! analog LCD waveforms or the external MSM5259 expansion path.

use crate::PanelOutput;
use std::collections::BTreeMap;

pub const DDRAM_SIZE: usize = 0x50;
pub const CGRAM_SIZE: usize = 0x40;
pub const VISIBLE_COLUMNS: usize = 8;

/// Configurable 5x8 character ROM. Each row uses the low five bits, with bit 4
/// as the leftmost dot. Unspecified character codes use the host fallback.
#[derive(Debug, Clone, Default)]
pub struct CharacterRom {
    glyphs: BTreeMap<u8, [u8; 8]>,
}

impl CharacterRom {
    pub fn set_glyph(&mut self, code: u8, rows: [u8; 8]) {
        self.glyphs.insert(code, rows.map(|row| row & 0x1f));
    }

    pub fn glyph(&self, code: u8) -> Option<&[u8; 8]> {
        self.glyphs.get(&code)
    }
}

#[derive(Debug, Clone)]
pub struct Ml9040a {
    ddram: [u8; DDRAM_SIZE],
    cgram: [u8; CGRAM_SIZE],
    address: u8,
    cgram_selected: bool,
    increment: bool,
    display_on: bool,
    cursor_on: bool,
    blink_on: bool,
    display_shift: i8,
    character_rom: CharacterRom,
}

impl Default for Ml9040a {
    fn default() -> Self {
        Self {
            ddram: [b' '; DDRAM_SIZE],
            cgram: [0; CGRAM_SIZE],
            address: 0,
            cgram_selected: false,
            increment: true,
            display_on: true,
            cursor_on: false,
            blink_on: false,
            display_shift: 0,
            character_rom: CharacterRom::default(),
        }
    }
}

impl Ml9040a {
    pub const MODEL: &'static str = "ML9040A-B01GA";

    pub fn with_character_rom(character_rom: CharacterRom) -> Self {
        Self {
            character_rom,
            ..Self::default()
        }
    }

    pub fn character_rom(&self) -> &CharacterRom {
        &self.character_rom
    }

    /// Write one byte using RS=0 for instruction or RS=1 for data.
    /// Returns an LCD update whenever visible character contents change.
    pub fn write(&mut self, register_select: bool, value: u8) -> Option<PanelOutput> {
        let changed = if register_select {
            self.write_data(value)
        } else {
            self.write_instruction(value)
        };
        changed.then(|| PanelOutput::LcdText {
            text: self.visible_line(),
        })
    }

    pub fn write_instruction(&mut self, instruction: u8) -> bool {
        match instruction {
            0x01 => {
                self.ddram.fill(b' ');
                self.address = 0;
                self.cgram_selected = false;
                self.increment = true;
                true
            }
            0x02 | 0x03 => {
                self.address = 0;
                self.cgram_selected = false;
                self.display_shift = 0;
                false
            }
            value if value & 0xfc == 0x04 => {
                self.increment = value & 0x02 != 0;
                false
            }
            value if value & 0xf8 == 0x08 => {
                let was_on = self.display_on;
                self.display_on = value & 0x04 != 0;
                self.cursor_on = value & 0x02 != 0;
                self.blink_on = value & 0x01 != 0;
                was_on != self.display_on
            }
            value if value & 0xf0 == 0x10 => {
                let right = value & 0x04 != 0;
                if value & 0x08 != 0 {
                    self.display_shift = (self.display_shift + if right { 1 } else { -1 })
                        .rem_euclid(VISIBLE_COLUMNS as i8);
                    true
                } else {
                    self.move_address(right);
                    false
                }
            }
            value if value & 0xe0 == 0x20 => false, // function set
            value if value & 0xc0 == 0x40 => {
                self.address = value & 0x3f;
                self.cgram_selected = true;
                false
            }
            value if value & 0x80 != 0 => {
                self.address = value & 0x7f;
                self.cgram_selected = false;
                false
            }
            _ => false,
        }
    }

    pub fn write_data(&mut self, value: u8) -> bool {
        let changed = if self.cgram_selected {
            let address = usize::from(self.address & 0x3f);
            self.cgram[address] = value;
            false
        } else if let Some(slot) = ddram_slot(self.address) {
            let before = self.ddram[slot];
            self.ddram[slot] = value;
            before != value
        } else {
            false
        };
        self.move_address(self.increment);
        changed
    }

    pub fn visible_line(&self) -> String {
        if !self.display_on {
            return " ".repeat(VISIBLE_COLUMNS);
        }
        let mut visible = String::with_capacity(VISIBLE_COLUMNS);
        for column in 0..VISIBLE_COLUMNS {
            let source =
                (column as i8 + self.display_shift).rem_euclid(VISIBLE_COLUMNS as i8) as usize;
            visible.push(render_character(self.ddram[source]));
        }
        visible
    }

    pub fn ddram(&self) -> &[u8; DDRAM_SIZE] {
        &self.ddram
    }

    pub fn cgram(&self) -> &[u8; CGRAM_SIZE] {
        &self.cgram
    }

    pub fn character_glyph(&self, code: u8) -> Option<&[u8; 8]> {
        self.character_rom.glyph(code)
    }

    fn move_address(&mut self, increment: bool) {
        if self.cgram_selected {
            self.address = if increment {
                self.address.wrapping_add(1) & 0x3f
            } else {
                self.address.wrapping_sub(1) & 0x3f
            };
        } else {
            self.address = if increment {
                self.address.wrapping_add(1) & 0x7f
            } else {
                self.address.wrapping_sub(1) & 0x7f
            };
        }
    }
}

fn ddram_slot(address: u8) -> Option<usize> {
    match address {
        0x00..=0x4f => Some(usize::from(address)),
        _ => None,
    }
}

fn render_character(code: u8) -> char {
    match code {
        0x00..=0x0f => ' ', // CGRAM glyphs need a pixel-font renderer.
        0x20..=0x7e => char::from(code),
        _ => ' ',
    }
}
