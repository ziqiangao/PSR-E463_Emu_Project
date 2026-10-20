//! Backend-independent I/O contract for the native PSR-E463 panel.
//!
//! A CPU or emulator core can implement [`PanelIo`] and feed output events
//! back to the frontend. [`JsonLinePanelIo`] is the standalone stdin/stdout
//! adapter used by the desktop executable.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    sync::mpsc::{Receiver, TryRecvError},
};

use serde::{Deserialize, Serialize};

pub mod armv7;
pub mod audio;
pub mod bitband;
pub mod cpu;
pub mod flash;
pub mod lcd;
pub mod usb;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PanelInput {
    Switch {
        svg_id: String,
        matrix_code: Option<String>,
        pressed: bool,
    },
    /// One physical key contact. Contact 1 closes on key-down; contact 2 is
    /// closed after the configured delay. The firmware derives velocity from
    /// the elapsed time between these two matrix transitions.
    PianoContact {
        key_id: String,
        midi_note: u8,
        contact: u8,
        pressed: bool,
    },
    Knob {
        svg_id: String,
        value: u8,
    },
    EncoderStep {
        direction: i8,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LcdSegmentState {
    pub svg_id: String,
    pub lit: bool,
}

/// Batch event produced by the LCD segment bank when segment state changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LcdSegmentsUpdated {
    pub segments: Vec<LcdSegmentState>,
}

impl LcdSegmentsUpdated {
    pub fn into_panel_output(self) -> PanelOutput {
        PanelOutput::LcdSegmentsUpdated {
            segments: self.segments,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PanelOutput {
    Led { svg_id: String, lit: bool },
    LcdSegment { svg_id: String, lit: bool },
    LcdSegmentsUpdated { segments: Vec<LcdSegmentState> },
    LcdText { text: String },
}

/// Machine-facing boundary for panel input and core-driven display output.
/// Implement this trait to connect the GTK frontend directly to CPU I/O.
pub trait PanelIo {
    fn emit_input(&mut self, input: PanelInput);
    fn poll_outputs(&mut self) -> Vec<PanelOutput>;
    /// Advance time-sensitive emulation work on the UI thread.
    fn tick(&mut self) {}
}

/// Standalone JSON-lines adapter. Replace it with a `PanelIo` implementation
/// backed by the emulator's CPU I/O when integrating the core in-process.
pub struct JsonLinePanelIo<W: Write> {
    writer: W,
    outputs: Receiver<PanelOutput>,
}

impl<W: Write> JsonLinePanelIo<W> {
    pub fn new(writer: W, outputs: Receiver<PanelOutput>) -> Self {
        Self { writer, outputs }
    }
}

impl<W: Write> PanelIo for JsonLinePanelIo<W> {
    fn emit_input(&mut self, input: PanelInput) {
        if serde_json::to_writer(&mut self.writer, &input).is_ok() {
            let _ = self.writer.write_all(b"\n");
            let _ = self.writer.flush();
        }
    }

    fn poll_outputs(&mut self) -> Vec<PanelOutput> {
        let mut outputs = Vec::new();
        loop {
            match self.outputs.try_recv() {
                Ok(output) => outputs.push(output),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        outputs
    }
}

/// Addressable LCD segment state. Batch updates return an event containing the
/// updated segment IDs and their new on/off state.
#[derive(Debug, Default, Clone)]
pub struct LcdSegmentBank {
    states: BTreeMap<String, bool>,
}

impl LcdSegmentBank {
    pub fn update_segments(
        &mut self,
        updates: impl IntoIterator<Item = LcdSegmentState>,
    ) -> LcdSegmentsUpdated {
        let segments = updates.into_iter().collect::<Vec<_>>();
        for segment in &segments {
            self.states.insert(segment.svg_id.clone(), segment.lit);
        }
        LcdSegmentsUpdated { segments }
    }

    pub fn is_lit(&self, svg_id: &str) -> bool {
        self.states.get(svg_id).copied().unwrap_or(false)
    }

    pub fn states(&self) -> &BTreeMap<String, bool> {
        &self.states
    }
}

/// Display state mirrored by the supplied SVG artwork.
#[derive(Debug, Default, Clone)]
pub struct PanelDisplayState {
    pub lit_leds: BTreeSet<String>,
    pub lit_lcd_segments: BTreeSet<String>,
    pub lcd_text: String,
}

impl PanelDisplayState {
    pub fn apply(&mut self, output: PanelOutput) {
        match output {
            PanelOutput::Led { svg_id, lit } => set_membership(&mut self.lit_leds, svg_id, lit),
            PanelOutput::LcdSegment { svg_id, lit } => {
                set_membership(&mut self.lit_lcd_segments, svg_id, lit)
            }
            PanelOutput::LcdSegmentsUpdated { segments } => {
                for segment in segments {
                    set_membership(&mut self.lit_lcd_segments, segment.svg_id, segment.lit);
                }
            }
            PanelOutput::LcdText { text } => self.lcd_text = text,
        }
    }
}

fn set_membership(set: &mut BTreeSet<String>, id: String, enabled: bool) {
    if enabled {
        set.insert(id);
    } else {
        set.remove(&id);
    }
}
