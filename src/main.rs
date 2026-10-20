use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    env, fs,
    io::{BufRead, BufReader},
    path::PathBuf,
    rc::Rc,
    sync::mpsc,
    thread,
    time::Duration,
};

use cairo::Context;
use gtk::{gdk, gio, glib, prelude::*, Application, ApplicationWindow, DrawingArea};
use psr_e463_ui::{
    armv7::ArmV7Core,
    cpu::CpuEmulator,
    lcd::{CharacterRom, Ml9040a},
    PanelDisplayState, PanelInput, PanelIo, PanelOutput,
};
use roxmltree::{Document, Node};
use serde::Deserialize;
use unicorn_engine::{unicorn_const::Prot, RegisterARM};

#[derive(Debug, Deserialize)]
struct Config {
    artwork: String,
    character_rom: String,
    controls: Vec<Control>,
    interactive_groups: Vec<InteractiveGroup>,
    knobs: KnobConfig,
    piano: PianoConfig,
    shortcuts: ShortcutConfig,
    #[serde(default)]
    switch_codes_by_id: HashMap<String, String>,
}
#[derive(Debug, Deserialize)]
struct CharacterRomFile {
    characters: HashMap<String, Vec<u8>>,
}
#[derive(Debug, Deserialize, Clone)]
struct Control {
    svg_id: String,
}
#[derive(Debug, Deserialize)]
struct PianoConfig {
    selector: String,
    computer_keys: Vec<String>,
    minimum_octave: u8,
    maximum_octave: u8,
    initial_octave: u8,
    initial_contact_delay_ms: u16,
    contact_delay_step_ms: u16,
}
#[derive(Debug, Deserialize)]
struct ShortcutConfig {
    octave_up: String,
    octave_down: String,
    ten_key: HashMap<String, String>,
}
#[derive(Debug, Deserialize)]
struct InteractiveGroup {
    svg_id: String,
}
#[derive(Debug, Deserialize)]
struct KnobConfig {
    regular: RegularKnobs,
    rotary: RotaryKnob,
}
#[derive(Debug, Deserialize)]
struct RegularKnobs {
    min_angle_deg: f64,
    max_angle_deg: f64,
    center_value: f64,
    value_min: f64,
    value_max: f64,
    #[serde(default = "default_center_snap_radius")]
    center_snap_radius: f64,
    #[serde(default)]
    center_snap_ids: Vec<String>,
    centers: Vec<KnobCenter>,
}
#[derive(Debug, Deserialize, Clone)]
struct KnobCenter {
    svg_id: String,
    x: f64,
    y: f64,
}
#[derive(Debug, Deserialize)]
struct RotaryKnob {
    svg_id: String,
    center_x: f64,
    center_y: f64,
    step_deg: f64,
}
#[derive(Clone)]
struct Shape {
    id: Option<String>,
    data_name: Option<String>,
    bounds: (f64, f64, f64, f64),
    groups: Vec<String>,
}
#[derive(Clone)]
struct KnobState {
    center: KnobCenter,
    surface: cairo::ImageSurface,
    surface_size: f64,
    angle: f64,
    value: f64,
    center_snap_active: bool,
    center_snap_enabled: bool,
}
#[derive(Clone)]
enum ActiveInput {
    Switch {
        svg_id: String,
        source_id: String,
    },
    Piano {
        key_id: String,
        midi_note: u8,
        source_id: String,
    },
}
struct PianoContactState {
    midi_note: u8,
    key_id: String,
    token: u64,
    first_down: bool,
    second_down: bool,
}
struct Panel {
    shapes: Vec<Shape>,
    artwork_base: cairo::ImageSurface,
    bindings: HashMap<String, Control>,
    keys: HashMap<String, u8>,
    switch_codes: HashMap<String, String>,
    knobs: Vec<KnobState>,
    rotary_angle: f64,
    rotary_center: (f64, f64),
    dial_surface: cairo::ImageSurface,
    dial_surface_size: f64,
    lcd_pixels: Vec<Vec<String>>,
    lcd_character_rom: HashMap<char, [u8; 8]>,
    pressed_switches: HashMap<String, HashSet<String>>,
    piano_contacts: HashMap<String, PianoContactState>,
    next_contact_token: u64,
    io: Rc<RefCell<dyn PanelIo>>,
    active_mouse: Option<ActiveInput>,
    active_keys: HashMap<String, ActiveInput>,
    display: PanelDisplayState,
    octave: u8,
    contact_delay_ms: u16,
    dragging: Option<DragTarget>,
    width: f64,
    height: f64,
}

fn default_center_snap_radius() -> f64 {
    6.0
}
#[derive(Clone, Copy)]
enum DragTarget {
    Regular(usize),
    Rotary,
}

fn main() -> glib::ExitCode {
    let force_test_mode = env::args().any(|arg| arg == "--force-test-mode");
    let debug_firmware = env::args().any(|arg| arg == "--debug-firmware");
    let diagnostic_wake_event4 = env::args().any(|arg| arg == "--diagnostic-wake-event4");
    if diagnostic_wake_event4 && !debug_firmware {
        eprintln!("--diagnostic-wake-event4 is available only with --debug-firmware");
        return glib::ExitCode::FAILURE;
    }
    if debug_firmware {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        return match psr_e463_ui::cpu::run_firmware_debugger(
            &root,
            force_test_mode,
            diagnostic_wake_event4,
        ) {
            Ok(()) => glib::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("[CPU] firmware debugger failed: {error}");
                glib::ExitCode::FAILURE
            }
        };
    }
    if let Some(instruction_limit) = parse_firmware_test_arg() {
        if force_test_mode {
            eprintln!("--force-test-mode is available with the live UI or debugger, not --test-firmware");
            return glib::ExitCode::FAILURE;
        }
        return match run_firmware_test(instruction_limit) {
            Ok(()) => glib::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("[CPU] firmware test failed: {error}");
                glib::ExitCode::FAILURE
            }
        };
    }
    let test_display_text = parse_test_display_arg();
    let config_path = PathBuf::from(env!("PSR_UI_CONFIG"));
    let config: Config = serde_json::from_slice(&fs::read(&config_path).expect("read UI config"))
        .expect("parse UI config");
    let svg_path = config_path.parent().unwrap().join(&config.artwork);
    let rom_path = config_path.parent().unwrap().join(&config.character_rom);
    let rom_data: CharacterRomFile =
        serde_json::from_slice(&fs::read(&rom_path).expect("read LCD character ROM configuration"))
            .expect("parse LCD character ROM configuration");
    let mut lcd_character_rom = HashMap::new();
    let mut driver_character_rom = CharacterRom::default();
    for (character, rows) in rom_data.characters {
        let mut chars = character.chars();
        let Some(character) = chars.next().filter(|_| chars.next().is_none()) else {
            eprintln!("[LCD] skipping ROM entry with non-single-character key {character:?}");
            continue;
        };
        let Ok(rows): Result<[u8; 8], _> = rows.try_into() else {
            eprintln!("[LCD] skipping ROM entry for {character:?}: expected 8 rows");
            continue;
        };
        let rows = rows.map(|row| row & 0x1f);
        lcd_character_rom.insert(character, rows);
        driver_character_rom.set_glyph(character as u8, rows);
    }
    let svg_text = fs::read_to_string(&svg_path).expect("read panel SVG");
    let document = Document::parse(&svg_text).expect("parse panel SVG");
    let (width, height) = viewbox(
        document
            .root_element()
            .attribute("viewBox")
            .unwrap_or("0 0 1969.9 460.24"),
    );
    let mut shapes = Vec::new();
    collect_shapes(
        document.root_element(),
        Vec::new(),
        (0.0, 0.0, 1.0, 1.0),
        &mut shapes,
    );
    let lcd_pixels = collect_lcd_pixels(document.root_element());
    let lcd_segment_ids = collect_lcd_segment_ids(document.root_element());
    let dial_surface_size = 112.0;
    let dial_surface = render_dial_surface(
        &svg_text,
        &config.knobs.rotary.svg_id,
        config.knobs.rotary.center_x,
        config.knobs.rotary.center_y,
        dial_surface_size,
    )
    .expect("render rotary dial artwork");

    let mut bindings = HashMap::new();
    for control in &config.controls {
        if shapes
            .iter()
            .any(|shape| shape.id.as_deref() == Some(&control.svg_id))
        {
            bindings.insert(control.svg_id.clone(), control.clone());
        }
    }
    for shape in &shapes {
        if let Some(id) = shape.id.as_ref() {
            for group in &config.interactive_groups {
                if shape
                    .groups
                    .iter()
                    .any(|ancestor| ancestor == &group.svg_id)
                    && !bindings.contains_key(id)
                {
                    bindings.insert(id.clone(), Control { svg_id: id.clone() });
                }
            }
        }
    }

    let keys = shapes
        .iter()
        .filter(|shape| {
            shape
                .groups
                .iter()
                .any(|group| group.starts_with(&config.piano.selector))
        })
        .filter_map(|shape| {
            let id = shape.id.as_ref()?;
            let note = shape.data_name.as_deref().unwrap_or(id);
            midi_from_key_id(note).map(|midi| (id.clone(), midi))
        })
        .filter(|(_, midi)| (24..=84).contains(midi))
        .collect::<HashMap<_, _>>();

    let knobs = config
        .knobs
        .regular
        .centers
        .iter()
        .cloned()
        .map(|center| KnobState {
            center_snap_enabled: config
                .knobs
                .regular
                .center_snap_ids
                .contains(&center.svg_id),
            surface: render_svg_node_surface(&svg_text, &center.svg_id, center.x, center.y, 80.0)
                .expect("render regular knob artwork"),
            surface_size: 80.0,
            center,
            angle: 0.0,
            value: config.knobs.regular.center_value,
            center_snap_active: false,
        })
        .collect();
    let artwork_base = render_static_panel(
        &svg_text,
        &config.knobs.regular.centers,
        &config.knobs.rotary.svg_id,
        &lcd_segment_ids,
        width,
        height,
    )
    .expect("render static panel artwork");
    // Keep GTK on the main thread; the firmware bridge advances in bounded
    // slices from the UI timer and external output JSON remains supported.
    let (output_tx, output_rx) = mpsc::channel::<PanelOutput>();
    thread::spawn(move || {
        for line in BufReader::new(std::io::stdin()).lines().flatten() {
            match serde_json::from_str::<PanelOutput>(&line) {
                Ok(output) => {
                    if output_tx.send(output).is_err() {
                        break;
                    }
                }
                Err(error) => eprintln!("Ignoring invalid panel output: {error}"),
            }
        }
    });
    let mut cpu_io = CpuEmulator::with_firmware(
            std::io::stdout(),
            output_rx,
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).as_path(),
            force_test_mode,
        )
        .expect("load firmware for the UI emulator");
    cpu_io.set_character_rom(driver_character_rom.clone());
    let panel_io: Rc<RefCell<dyn PanelIo>> = Rc::new(RefCell::new(cpu_io));
    let mut display = PanelDisplayState::default();
    if let Some(text) = test_display_text {
        let mut lcd = Ml9040a::with_character_rom(driver_character_rom);
        let _ = lcd.write(false, 0x01);
        let _ = lcd.write(false, 0x80);
        for byte in text.bytes().take(8) {
            if let Some(output) = lcd.write(true, byte) {
                display.apply(output);
            }
        }
        eprintln!("[LCD] test display enabled: {:?}", lcd.visible_line());
    }
    let panel = Rc::new(RefCell::new(Panel {
        shapes,
        artwork_base,
        bindings,
        keys,
        switch_codes: config.switch_codes_by_id.clone(),
        knobs,
        rotary_angle: 0.0,
        rotary_center: (config.knobs.rotary.center_x, config.knobs.rotary.center_y),
        dial_surface,
        dial_surface_size,
        lcd_pixels,
        lcd_character_rom,
        pressed_switches: HashMap::new(),
        piano_contacts: HashMap::new(),
        next_contact_token: 1,
        io: panel_io,
        active_mouse: None,
        active_keys: HashMap::new(),
        display,
        octave: config
            .piano
            .initial_octave
            .clamp(config.piano.minimum_octave, config.piano.maximum_octave),
        contact_delay_ms: config.piano.initial_contact_delay_ms.clamp(10, 150),
        dragging: None,
        width,
        height,
    }));
    let knobs_config = Rc::new(config.knobs);
    let shortcuts = Rc::new(config.shortcuts);
    let piano = Rc::new(config.piano);

    let app = Application::new(
        Some("org.psre463.emulator-ui"),
        gio::ApplicationFlags::FLAGS_NONE,
    );
    let panel_activate = panel.clone();
    let knobs_activate = knobs_config.clone();
    let shortcuts_activate = shortcuts.clone();
    let piano_activate = piano.clone();
    app.connect_activate(move |app| {
        let window = ApplicationWindow::new(app);
        window.set_title("PSR-E463 Emulator");
        let (panel_width, panel_height) = {
            let panel = panel_activate.borrow();
            (panel.width, panel.height)
        };
        let (available_width, available_height) = gdk::Display::default()
            .and_then(|display| display.primary_monitor())
            .map(|monitor| {
                let geometry = monitor.geometry();
                (geometry.width(), geometry.height())
            })
            .filter(|(width, height)| *width > 0 && *height > 0)
            .unwrap_or((panel_width as i32, panel_height as i32));
        let fit_scale = (f64::from(available_width) / panel_width)
            .min(f64::from(available_height) / panel_height);
        let initial_width = (panel_width * fit_scale).round() as i32;
        let initial_height = (panel_height * fit_scale).round() as i32;
        window.set_default_size(initial_width, initial_height);
        window.set_decorated(false);
        window.set_resizable(true);
        if let Some(screen) = gdk::Screen::default() {
            if let Some(visual) = screen.rgba_visual() {
                window.set_visual(Some(&visual));
            }
        }
        let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
        layout.set_border_width(0);
        let area = DrawingArea::new();
        area.set_size_request(1, 1);
        area.set_hexpand(true);
        area.set_vexpand(true);
        area.set_app_paintable(true);
        area.add_events(
            gdk::EventMask::BUTTON_PRESS_MASK
                | gdk::EventMask::BUTTON1_MOTION_MASK
                | gdk::EventMask::POINTER_MOTION_MASK
                | gdk::EventMask::BUTTON_RELEASE_MASK
                | gdk::EventMask::SCROLL_MASK,
        );
        eprintln!(
            "Registered {} panel controls and {} piano keys",
            panel_activate.borrow().bindings.len(),
            panel_activate.borrow().keys.len()
        );
        let panel_draw = panel_activate.clone();
        area.connect_draw(move |widget, ctx| {
            draw(
                ctx,
                widget.allocated_width() as f64,
                widget.allocated_height() as f64,
                &panel_draw.borrow(),
            );
            glib::Propagation::Proceed
        });

        let panel_click = panel_activate.clone();
        let area_click = area.clone();
        let close_window = window.clone();
        area.connect_button_press_event(move |_, event| {
            let mut panel = panel_click.borrow_mut();
            let pos = to_svg(
                &panel,
                event.position(),
                (
                    area_click.allocated_width() as f64,
                    area_click.allocated_height() as f64,
                ),
            );
            if event.button() == 3
                || (event.button() == 1 && event.state().contains(gdk::ModifierType::MOD1_MASK))
            {
                close_window.begin_move_drag(
                    event.button() as i32,
                    event.root().0 as i32,
                    event.root().1 as i32,
                    event.time(),
                );
                return glib::Propagation::Stop;
            }
            let local = event.position();
            if local.0 >= area_click.allocated_width() as f64 - 18.0
                && local.1 >= area_click.allocated_height() as f64 - 18.0
            {
                close_window.begin_resize_drag(
                    gdk::WindowEdge::SouthEast,
                    1,
                    event.root().0 as i32,
                    event.root().1 as i32,
                    event.time(),
                );
                return glib::Propagation::Stop;
            }
            if let Some(index) = regular_knob_at(&panel, pos) {
                panel.dragging = Some(DragTarget::Regular(index));
                return glib::Propagation::Proceed;
            }
            if rotary_at(&panel, pos) {
                panel.dragging = Some(DragTarget::Rotary);
                return glib::Propagation::Proceed;
            }
            if let Some(id) = hit_svg(&panel, pos) {
                // Clear an earlier mouse hold if a rapid pointer transition
                // delivered a new press before its release was processed.
                if let Some(previous) = panel.active_mouse.take() {
                    release_input(&mut panel, previous);
                }
                if id == "Power-2" {
                    let _ = switch_input(&mut panel, &id, true);
                    let _ = switch_input(&mut panel, &id, false);
                    close_window.close();
                } else if let Some(midi_note) = panel.keys.get(&id).copied() {
                    let key_id = piano_key_name(midi_note);
                    let source_id = format!("mouse:{id}");
                    let input = ActiveInput::Piano {
                        key_id,
                        midi_note,
                        source_id: source_id.clone(),
                    };
                    let delay = panel.contact_delay_ms;
                    let token = activate_input(&mut panel, input.clone());
                    panel.active_mouse = Some(input);
                    if let Some(token) = token {
                        schedule_second_contact(
                            panel_click.clone(),
                            area_click.clone(),
                            source_id,
                            token,
                            delay,
                        );
                    }
                } else if is_panel_switch(&panel, &id) {
                    let input = ActiveInput::Switch {
                        svg_id: id.clone(),
                        source_id: "mouse".into(),
                    };
                    let _ = activate_input(&mut panel, input.clone());
                    panel.active_mouse = Some(input);
                }
            }
            area_click.queue_draw();
            glib::Propagation::Proceed
        });

        let panel_motion = panel_activate.clone();
        let area_motion = area.clone();
        let knobs_motion = knobs_activate.clone();
        area.connect_motion_notify_event(move |_, event| {
            let mut panel = panel_motion.borrow_mut();
            let pos = to_svg(
                &panel,
                event.position(),
                (
                    area_motion.allocated_width() as f64,
                    area_motion.allocated_height() as f64,
                ),
            );
            match panel.dragging {
                Some(DragTarget::Regular(i)) => {
                    let center = panel.knobs[i].center.clone();
                    let spec = &knobs_motion.regular;
                    let angle = pointer_angle(pos, center.x, center.y)
                        .clamp(spec.min_angle_deg, spec.max_angle_deg);
                    let value = if angle < 0.0 {
                        spec.center_value + (angle / 135.0) * (spec.center_value - spec.value_min)
                    } else {
                        spec.center_value + (angle / 135.0) * (spec.value_max - spec.center_value)
                    }
                    .round()
                    .clamp(spec.value_min, spec.value_max);
                    // Snap on entry to the center band and hold the detent
                    // until the pointer moves outside the same +/- radius.
                    let radius = if panel.knobs[i].center_snap_enabled {
                        spec.center_snap_radius.max(0.0)
                    } else {
                        0.0
                    };
                    let center_value = spec.center_value;
                    let was_snapped = panel.knobs[i].center_snap_active;
                    let snapped = panel.knobs[i].center_snap_enabled
                        && if was_snapped {
                            (value - center_value).abs() <= radius + 1.0
                        } else {
                            (value - center_value).abs() <= radius
                        };
                    let value = if snapped { center_value } else { value };
                    if (value - panel.knobs[i].value).abs() >= 1.0 {
                        panel.knobs[i].angle = if snapped { 0.0 } else { angle };
                        panel.knobs[i].value = value;
                        panel.knobs[i].center_snap_active = snapped;
                        panel.io.borrow_mut().emit_input(PanelInput::Knob {
                            svg_id: center.svg_id,
                            value: value as u8,
                        });
                        area_motion.queue_draw();
                    } else {
                        panel.knobs[i].center_snap_active = snapped;
                        if snapped {
                            panel.knobs[i].angle = 0.0;
                            area_motion.queue_draw();
                        }
                    }
                }
                Some(DragTarget::Rotary) => {
                    let angle = pointer_angle(
                        pos,
                        knobs_motion.rotary.center_x,
                        knobs_motion.rotary.center_y,
                    );
                    let step = knobs_motion.rotary.step_deg;
                    let snapped = (angle / step).round() * step;
                    let mut delta = snapped - panel.rotary_angle;
                    if delta > 180.0 {
                        delta -= 360.0;
                    }
                    if delta < -180.0 {
                        delta += 360.0;
                    }
                    let steps = (delta / step).round() as i32;
                    if steps != 0 {
                        panel.rotary_angle = snapped;
                        for _ in 0..steps.abs() {
                            panel.io.borrow_mut().emit_input(PanelInput::EncoderStep {
                                direction: if steps > 0 { 1 } else { -1 },
                            });
                        }
                        area_motion.queue_draw();
                    }
                }
                None => {}
            }
            glib::Propagation::Proceed
        });

        let panel_release = panel_activate.clone();
        let area_release = area.clone();
        area.connect_button_release_event(move |_, event| {
            if event.button() == 1 {
                let mut panel = panel_release.borrow_mut();
                panel.dragging = None;
                if let Some(input) = panel.active_mouse.take() {
                    release_input(&mut panel, input);
                }
                area_release.queue_draw();
            }
            glib::Propagation::Proceed
        });

        let panel_scroll = panel_activate.clone();
        let area_scroll = area.clone();
        let piano_scroll = piano_activate.clone();
        area.connect_scroll_event(move |_, event| {
            let direction = match event.direction() {
                gdk::ScrollDirection::Up => 1_i16,
                gdk::ScrollDirection::Down => -1_i16,
                _ => return glib::Propagation::Proceed,
            };
            let mut panel = panel_scroll.borrow_mut();
            let step = i32::from(piano_scroll.contact_delay_step_ms.max(1));
            // Scrolling up raises velocity by shortening the contact interval.
            panel.contact_delay_ms = (i32::from(panel.contact_delay_ms)
                - i32::from(direction) * step)
                .clamp(10, 150) as u16;
            area_scroll.queue_draw();
            glib::Propagation::Stop
        });

        let panel_key_down = panel_activate.clone();
        let shortcuts_key_down = shortcuts_activate.clone();
        let piano_key_down = piano_activate.clone();
        let area_key_down = area.clone();
        window.connect_key_press_event(move |_, event| {
            let name = event.keyval().name().unwrap_or_default().to_lowercase();
            let mut panel = panel_key_down.borrow_mut();
            if name == shortcuts_key_down.octave_up.to_lowercase() {
                panel.octave = (panel.octave + 1).min(piano_key_down.maximum_octave);
            } else if name == shortcuts_key_down.octave_down.to_lowercase() {
                panel.octave = panel
                    .octave
                    .saturating_sub(1)
                    .max(piano_key_down.minimum_octave);
            } else if let Some(id) = shortcuts_key_down.ten_key.get(&name) {
                if !panel.active_keys.contains_key(&name) {
                    let input = ActiveInput::Switch {
                        svg_id: id.clone(),
                        source_id: format!("key:{name}"),
                    };
                    let _ = activate_input(&mut panel, input.clone());
                    panel.active_keys.insert(name, input);
                }
            } else if let Some(index) = piano_key_down
                .computer_keys
                .iter()
                .position(|key| key.to_lowercase() == name)
            {
                if !panel.active_keys.contains_key(&name) {
                    let midi_note = (i16::from(panel.octave) + 1) as u8 * 12 + index as u8;
                    if midi_note <= 84 {
                        let key_id = piano_key_name(midi_note);
                        let source_id = format!("key:{name}");
                        let input = ActiveInput::Piano {
                            key_id,
                            midi_note,
                            source_id: source_id.clone(),
                        };
                        let delay = panel.contact_delay_ms;
                        let token = activate_input(&mut panel, input.clone());
                        panel.active_keys.insert(name, input);
                        if let Some(token) = token {
                            schedule_second_contact(
                                panel_key_down.clone(),
                                area_key_down.clone(),
                                source_id,
                                token,
                                delay,
                            );
                        }
                    }
                }
            }
            area_key_down.queue_draw();
            glib::Propagation::Proceed
        });
        let panel_key_up = panel_activate.clone();
        let area_key_up = area.clone();
        window.connect_key_release_event(move |_, event| {
            let name = event.keyval().name().unwrap_or_default().to_lowercase();
            let mut panel = panel_key_up.borrow_mut();
            if let Some(input) = panel.active_keys.remove(&name) {
                release_input(&mut panel, input);
            }
            area_key_up.queue_draw();
            glib::Propagation::Proceed
        });
        let panel_focus = panel_activate.clone();
        let area_focus = area.clone();
        window.connect_focus_out_event(move |_, _| {
            let mut panel = panel_focus.borrow_mut();
            let active = std::mem::take(&mut panel.active_keys);
            for (_, input) in active {
                release_input(&mut panel, input);
            }
            if let Some(input) = panel.active_mouse.take() {
                release_input(&mut panel, input);
            }
            area_focus.queue_draw();
            glib::Propagation::Proceed
        });

        let panel_outputs = panel_activate.clone();
        let area_outputs = area.clone();
        glib::timeout_add_local(Duration::from_millis(20), move || {
            let mut panel = panel_outputs.borrow_mut();
            panel.io.borrow_mut().tick();
            let outputs = panel.io.borrow_mut().poll_outputs();
            let changed = !outputs.is_empty();
            for output in outputs {
                panel.display.apply(output);
            }
            drop(panel);
            if changed {
                area_outputs.queue_draw();
            }
            glib::ControlFlow::Continue
        });

        layout.pack_start(&area, true, true, 0);
        let css = gtk::CssProvider::new();
        let _ = css.load_from_data(b"window, box, drawingarea { background-color: transparent; }");
        if let Some(screen) = gdk::Screen::default() {
            gtk::StyleContext::add_provider_for_screen(
                &screen,
                &css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        window.add(&layout);
        window.show_all();
    });
    app.run_with_args(&[env::args().next().as_deref().unwrap_or("psr-e463-ui")])
}

fn parse_firmware_test_arg() -> Option<usize> {
    let mut args = env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        if arg == "--test-firmware" {
            let limit = match args.peek().and_then(|value| value.parse::<usize>().ok()) {
                Some(limit) => {
                    args.next();
                    limit
                }
                None => 100_000,
            };
            return Some(limit);
        }
        if let Some(limit) = arg.strip_prefix("--test-firmware=") {
            return Some(limit.parse().unwrap_or(100_000));
        }
        if arg == "--help" || arg == "-h" {
            println!("Usage: psr-e463-ui [--test-display [TEXT]] [--test-firmware [INSTRUCTIONS]] [--debug-firmware [--force-test-mode] [--diagnostic-wake-event4]]");
            std::process::exit(0);
        }
    }
    None
}

fn run_firmware_test(instruction_limit: usize) -> Result<(), Box<dyn std::error::Error>> {
    use std::{cell::Cell, collections::VecDeque, fs, io::Write, path::Path, rc::Rc};
    use unicorn_engine::unicorn_const::HookType;

    const PAGE: usize = 0x1000;
    const RAM_BASE: u32 = 0x0000_0000;
    const RAM_SIZE: usize = 0x0013_4000;
    const SDRAM_BASE: u32 = 0x2000_0000;
    const SDRAM_SIZE: usize = 0x0020_0000;
    const STARTUP_RAM_BASE: u32 = 0x6800_0000;
    const STARTUP_RAM_SIZE: usize = 0x0039_4000;
    const OBSERVED_PERIPHERAL_BASE: u32 = 0x4000_0000;
    const UNKNOWN_STARTUP_PERIPHERAL_BASE: u32 = 0x4000_2000;
    const BOOT_CONTROL_BASE: u32 = 0x4000_4000;
    const STARTUP_CONTROL_BASE: u32 = 0x4000_8000;
    const STARTUP_CONTROL2_BASE: u32 = 0x4000_9000;
    const STARTUP_CONTROL3_BASE: u32 = 0x4000_A000;
    const CORTEX_M_SCS_BASE: u32 = 0xE000_E000;
    const DRIVER_BASE: u32 = 0x0100_0000;
    const FIRMWARE_BASE: u32 = 0x6000_0000;
    const TRACE_CAPACITY: usize = 64;
    const STACK_DUMP_SIZE: usize = 0x1000;

    fn map_image(
        core: &mut ArmV7Core,
        path: &Path,
        address: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let bytes = fs::read(path)?;
        let size = bytes.len().div_ceil(PAGE) * PAGE;
        core.map_region(address, size, Prot::ALL)?;
        core.load_bytes(address, &bytes)?;
        eprintln!(
            "[CPU] mapped {} bytes from {} at {address:#010x}",
            bytes.len(),
            path.display()
        );
        Ok(())
    }

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut core = ArmV7Core::new()?;
    core.map_region(RAM_BASE, RAM_SIZE, Prot::ALL)?;
    core.map_region(SDRAM_BASE, SDRAM_SIZE, Prot::ALL)?;
    eprintln!(
        "[CPU] mapped SDRAM {SDRAM_BASE:#010x}..{:#010x}",
        SDRAM_BASE + SDRAM_SIZE as u32
    );
    // The firmware startup zero-range table clears 0x393470 bytes beginning
    // at 0x68000830. Map the enclosing page-aligned guest RAM range.
    core.map_region(STARTUP_RAM_BASE, STARTUP_RAM_SIZE, Prot::ALL)?;
    eprintln!(
        "[CPU] mapped startup RAM {STARTUP_RAM_BASE:#010x}..{:#010x}",
        STARTUP_RAM_BASE + STARTUP_RAM_SIZE as u32
    );
    // Startup polls status at 0x40000028. This zeroed page is only a smoke-test
    // placeholder; it is not a model of the SWX03 peripheral block.
    core.map_region(OBSERVED_PERIPHERAL_BASE, PAGE, Prot::ALL)?;
    // GPIO and input-scan registers occupy the next 4 KiB page.
    core.map_region(0x4000_1000, PAGE, Prot::ALL)?;
    // The live UI run reaches this observed startup block before its current
    // fault. Keep the smoke-test map aligned while leaving device semantics
    // explicitly unknown.
    core.map_region(UNKNOWN_STARTUP_PERIPHERAL_BASE, PAGE, Prot::ALL)?;
    // The startup image handoff also reads a control halfword at 0x4000400e.
    core.map_region(BOOT_CONTROL_BASE, PAGE, Prot::ALL)?;
    // The USB driver uses the shared control word at 0x40005028 to enable the
    // host (TO DEVICE) and device (TO HOST) controllers. Both remain detached.
    core.map_region(psr_e463_ui::usb::USB_REGISTER_PAGE as u32, PAGE, Prot::ALL)?;
    // Early startup writes at 0x40008000. Keep this page a local stub only.
    core.map_region(STARTUP_CONTROL_BASE, PAGE, Prot::ALL)?;
    // Firmware initialization at 0x60086824 writes the 0x40009000 block.
    core.map_region(STARTUP_CONTROL2_BASE, PAGE, Prot::ALL)?;
    core.map_region(STARTUP_CONTROL3_BASE, PAGE, Prot::ALL)?;
    // Backing memory for Cortex-M core peripheral registers (SCB/NVIC/SysTick)
    // used by firmware startup. This is a smoke-test stub, not a device model.
    core.map_region(CORTEX_M_SCS_BASE, PAGE, Prot::ALL)?;
    let usb = psr_e463_ui::usb::attach(core.engine_mut())?;
    psr_e463_ui::bitband::attach(&mut core, usb)?;
    let virtual_device_error = psr_e463_ui::cpu::attach_virtual_startup_model(&mut core)?;
    psr_e463_ui::flash::attach(&mut core, &root.join("flash.bin"))?;
    // The real reset/boot chain establishes banked stacks before entering the
    // image. This direct entry test uses the top of the mapped low RAM.
    core.set_register(RegisterARM::SP, RAM_SIZE as u64)?;
    map_image(&mut core, &root.join("driver-01000000.bin"), DRIVER_BASE)?;
    map_image(
        &mut core,
        &root.join("firmware-0x60000000.bin"),
        FIRMWARE_BASE,
    )?;
    let trace = Rc::new(RefCell::new(VecDeque::<u32>::with_capacity(TRACE_CAPACITY)));
    let hook_trace = Rc::clone(&trace);
    core.engine_mut()
        .add_code_hook(1, 0, move |_, address, _| {
            let mut trace = hook_trace.borrow_mut();
            if trace.len() == TRACE_CAPACITY {
                trace.pop_front();
            }
            trace.push_back(address as u32);
        })?;
    let fault = Rc::new(Cell::new(None::<(u32, usize, i64, u32)>));
    let hook_fault = Rc::clone(&fault);
    core.engine_mut().add_mem_hook(
        HookType::MEM_UNMAPPED,
        1,
        0,
        move |engine, kind, address, size, value| {
            let pc = engine.reg_read(RegisterARM::PC).unwrap_or_default() as u32;
            hook_fault.set(Some((address as u32, size, value, pc)));
            eprintln!("[CPU] unmapped memory access {kind:?} address={address:#010x} size={size} value={value:#x} pc={pc:#010x}");
            false
        },
    )?;
    eprintln!("[CPU] starting Thumb execution at {FIRMWARE_BASE:#010x}, instruction limit={instruction_limit}");
    match core.run(FIRMWARE_BASE | 1, instruction_limit) {
        Ok(pc) => eprintln!(
            "[CPU] firmware test stopped at PC={pc:#010x} (instruction limit or guest stop)"
        ),
        Err(error) => {
            let pc = core.register(RegisterARM::PC).unwrap_or_default() as u32;
            eprintln!("[CPU] firmware test stopped at PC={pc:#010x}: {error}");
        }
    }
    let virtual_device_error = virtual_device_error.borrow_mut().take();
    if let Some(error) = &virtual_device_error {
        eprintln!("[CPU] virtual startup model failed: {error}");
    }
    let registers = [
        ("r0", RegisterARM::R0),
        ("r1", RegisterARM::R1),
        ("r2", RegisterARM::R2),
        ("r3", RegisterARM::R3),
        ("r4", RegisterARM::R4),
        ("r5", RegisterARM::R5),
        ("r6", RegisterARM::R6),
        ("r7", RegisterARM::R7),
        ("r8", RegisterARM::R8),
        ("r9", RegisterARM::R9),
        ("r10", RegisterARM::R10),
        ("r11", RegisterARM::R11),
        ("r12", RegisterARM::R12),
        ("sp", RegisterARM::SP),
        ("lr", RegisterARM::LR),
        ("pc", RegisterARM::PC),
        ("cpsr", RegisterARM::CPSR),
    ];
    let mut dump = Vec::new();
    writeln!(dump, "PSR-E463 Unicorn core dump\n")?;
    if let Some(error) = virtual_device_error {
        writeln!(dump, "virtual startup model error: {error}\n")?;
    }
    if let Some((address, size, value, pc)) = fault.get() {
        writeln!(
            dump,
            "unmapped access: address={address:#010x} size={size} value={value:#x} pc={pc:#010x}\n"
        )?;
    }
    for (name, register) in registers {
        let value = core.register(register)? as u32;
        writeln!(dump, "{name:>4} = {value:#010x}")?;
    }
    let sp = core.register(RegisterARM::SP)? as u32;
    let ram_end = RAM_BASE + RAM_SIZE as u32;
    let (stack_start, stack_size) = if (RAM_BASE..ram_end).contains(&sp) {
        let start = sp.saturating_sub(STACK_DUMP_SIZE as u32).max(RAM_BASE) & !0xf;
        (start, STACK_DUMP_SIZE.min((ram_end - start) as usize))
    } else {
        (
            sp.saturating_sub(STACK_DUMP_SIZE as u32) & !0xf,
            STACK_DUMP_SIZE,
        )
    };
    writeln!(
        dump,
        "\nstack memory: {stack_start:#010x}..{:#010x}",
        stack_start + stack_size as u32
    )?;
    let mut stack = vec![0; stack_size];
    if core.read_bytes(stack_start, &mut stack).is_ok() {
        for (offset, word) in stack.chunks_exact(4).enumerate() {
            if offset % 4 == 0 {
                write!(dump, "\n{:#010x}: ", stack_start + (offset * 4) as u32)?;
            }
            writeln!(
                dump,
                "{:02x}{:02x}{:02x}{:02x} ",
                word[0], word[1], word[2], word[3]
            )?;
        }
    } else {
        writeln!(dump, "<unmapped>")?;
    }
    let firmware_end =
        FIRMWARE_BASE + fs::metadata(root.join("firmware-0x60000000.bin"))?.len() as u32;
    let driver_end = DRIVER_BASE + fs::metadata(root.join("driver-01000000.bin"))?.len() as u32;
    writeln!(dump, "\nstack-scanned return-address candidates:")?;
    for (offset, word) in stack.chunks_exact(4).enumerate() {
        let address = u32::from_le_bytes(word.try_into()?);
        if (FIRMWARE_BASE..firmware_end).contains(&address)
            || (DRIVER_BASE..driver_end).contains(&address)
        {
            writeln!(
                dump,
                "{:#010x}: {address:#010x}",
                stack_start + (offset * 4) as u32
            )?;
        }
    }
    writeln!(dump, "\nrecent instruction addresses (oldest first):")?;
    let trace = trace.borrow();
    for address in trace.iter() {
        writeln!(dump, "{address:#010x}")?;
    }
    eprintln!("[CPU] recent instruction trace:");
    for address in trace.iter() {
        eprintln!("[CPU]   {address:#010x}");
    }
    drop(trace);
    let dump_path = root.join("firmware-core-dump.txt");
    fs::write(&dump_path, dump)?;
    let memory_path = root.join("firmware-core-dump-ram.bin");
    let mut ram = vec![0; RAM_SIZE];
    core.read_bytes(RAM_BASE, &mut ram)?;
    fs::write(&memory_path, ram)?;
    let mut sdram = vec![0; SDRAM_SIZE];
    core.read_bytes(SDRAM_BASE, &mut sdram)?;
    fs::write(root.join("firmware-core-dump-sdram.bin"), sdram)?;
    let mut startup_ram = vec![0; STARTUP_RAM_SIZE];
    core.read_bytes(STARTUP_RAM_BASE, &mut startup_ram)?;
    fs::write(root.join("firmware-core-dump-startup-ram.bin"), startup_ram)?;
    for (name, address, size) in [
        (
            "driver",
            DRIVER_BASE,
            fs::metadata(root.join("driver-01000000.bin"))?.len() as usize,
        ),
        (
            "firmware",
            FIRMWARE_BASE,
            fs::metadata(root.join("firmware-0x60000000.bin"))?.len() as usize,
        ),
    ] {
        let mut image = vec![0; size];
        core.read_bytes(address, &mut image)?;
        fs::write(root.join(format!("firmware-core-dump-{name}.bin")), image)?;
    }
    eprintln!(
        "[CPU] wrote core dump {} and memory images with prefix firmware-core-dump-",
        dump_path.display()
    );
    Ok(())
}

fn parse_test_display_arg() -> Option<String> {
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--test-display" {
            return Some(args.next().unwrap_or_else(|| "LCD TEST".to_owned()));
        }
        if let Some(text) = arg.strip_prefix("--test-display=") {
            return Some(text.to_owned());
        }
        if arg == "--help" || arg == "-h" {
            println!("Usage: psr-e463-ui [--test-display [TEXT]]\n\nRender TEXT through the ML9040A LCD model (8 visible characters).");
            std::process::exit(0);
        }
    }
    None
}

fn switch_input(panel: &Panel, id: &str, pressed: bool) -> bool {
    let code = panel.switch_codes.get(id).cloned();
    panel.io.borrow_mut().emit_input(PanelInput::Switch {
        svg_id: id.to_owned(),
        matrix_code: code,
        pressed,
    });
    true
}
fn is_panel_switch(panel: &Panel, id: &str) -> bool {
    panel.shapes.iter().any(|shape| {
        shape.id.as_deref() == Some(id)
            && shape
                .groups
                .iter()
                .any(|group| group == "Buttons" || group == "LEDButtons")
    })
}
fn activate_input(panel: &mut Panel, input: ActiveInput) -> Option<u64> {
    match input {
        ActiveInput::Switch { svg_id, source_id } => {
            let sources = panel.pressed_switches.entry(svg_id.clone()).or_default();
            let was_released = sources.is_empty();
            if !sources.insert(source_id) {
                return None;
            }
            if was_released {
                switch_input(panel, &svg_id, true);
            }
            None
        }
        ActiveInput::Piano {
            key_id,
            midi_note,
            source_id,
        } => {
            if panel.piano_contacts.contains_key(&source_id) {
                return None;
            }
            let token = panel.next_contact_token;
            panel.next_contact_token = panel.next_contact_token.wrapping_add(1).max(1);
            panel.piano_contacts.insert(
                source_id,
                PianoContactState {
                    midi_note,
                    key_id: key_id.clone(),
                    token,
                    first_down: true,
                    second_down: false,
                },
            );
            panel.io.borrow_mut().emit_input(PanelInput::PianoContact {
                key_id,
                midi_note,
                contact: 1,
                pressed: true,
            });
            Some(token)
        }
    }
}
fn release_input(panel: &mut Panel, input: ActiveInput) {
    match input {
        ActiveInput::Switch { svg_id, source_id } => {
            if let Some(sources) = panel.pressed_switches.get_mut(&svg_id) {
                // Remove one held source. Each input path has one active
                // instance per key/button; releasing one must not release
                // another source that still physically holds the switch.
                sources.remove(&source_id);
                if sources.is_empty() {
                    panel.pressed_switches.remove(&svg_id);
                    switch_input(panel, &svg_id, false);
                }
            }
        }
        ActiveInput::Piano { source_id, .. } => {
            if let Some(state) = panel.piano_contacts.remove(&source_id) {
                if state.second_down {
                    panel.io.borrow_mut().emit_input(PanelInput::PianoContact {
                        key_id: state.key_id.clone(),
                        midi_note: state.midi_note,
                        contact: 2,
                        pressed: false,
                    });
                }
                if state.first_down {
                    panel.io.borrow_mut().emit_input(PanelInput::PianoContact {
                        key_id: state.key_id,
                        midi_note: state.midi_note,
                        contact: 1,
                        pressed: false,
                    });
                }
            }
        }
    }
}
fn schedule_second_contact(
    panel: Rc<RefCell<Panel>>,
    area: DrawingArea,
    source_id: String,
    token: u64,
    delay_ms: u16,
) {
    glib::timeout_add_local(Duration::from_millis(u64::from(delay_ms)), move || {
        let mut panel = panel.borrow_mut();
        let event = if let Some(state) = panel.piano_contacts.get_mut(&source_id) {
            if state.token == token && state.first_down && !state.second_down {
                state.second_down = true;
                Some(PanelInput::PianoContact {
                    key_id: state.key_id.clone(),
                    midi_note: state.midi_note,
                    contact: 2,
                    pressed: true,
                })
            } else {
                None
            }
        } else {
            None
        };
        if let Some(event) = event {
            panel.io.borrow_mut().emit_input(event);
        }
        area.queue_draw();
        glib::ControlFlow::Break
    });
}
fn piano_key_name(midi_note: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!("{}{}", NAMES[(midi_note % 12) as usize], midi_note / 12 - 1)
}
fn collect_lcd_pixels(root: Node<'_, '_>) -> Vec<Vec<String>> {
    (1..=8)
        .map(|position| {
            let id = format!("Char{position}");
            let Some(character) = root
                .descendants()
                .find(|node| node.attribute("id") == Some(&id))
            else {
                return Vec::new();
            };
            let mut pixels = character
                .descendants()
                .filter(|node| node.tag_name().name() == "path")
                .filter_map(|node| {
                    let id = node.attribute("id")?.to_owned();
                    let (x, y) = first_svg_point(node.attribute("d")?)?;
                    Some((y, x, id))
                })
                .collect::<Vec<_>>();
            pixels.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
            pixels.into_iter().map(|(_, _, id)| id).collect()
        })
        .collect()
}

fn collect_lcd_segment_ids(root: Node<'_, '_>) -> Vec<String> {
    let Some(lcd) = root
        .descendants()
        .find(|node| node.attribute("id") == Some("LCD"))
    else {
        return Vec::new();
    };
    lcd.descendants()
        .filter(|node| {
            node.tag_name().name() == "path" && node.attribute("id") != Some("Backlight")
        })
        .filter_map(|node| node.attribute("id").map(str::to_owned))
        .collect()
}

fn first_svg_point(path: &str) -> Option<(f64, f64)> {
    let mut values = path
        .trim_start()
        .strip_prefix('M')?
        .split(|c: char| c.is_whitespace() || c == ',');
    let x = values.next()?.parse().ok()?;
    let y = values.find(|value| !value.is_empty())?.parse().ok()?;
    Some((x, y))
}

fn lcd_glyph_pixels(character: char, character_rom: &HashMap<char, [u8; 8]>) -> [bool; 40] {
    if let Some(rows) = character_rom.get(&character) {
        let mut pixels = [false; 40];
        for (y, row) in rows.iter().enumerate() {
            for x in 0..5 {
                pixels[y * 5 + x] = row & (1 << (4 - x)) != 0;
            }
        }
        return pixels;
    }
    let mut pixels = [false; 40];
    let Ok(mut surface) = cairo::ImageSurface::create(cairo::Format::A8, 5, 8) else {
        return pixels;
    };
    let Ok(context) = Context::new(&surface) else {
        return pixels;
    };
    let _ = context.set_source_rgba(0.0, 0.0, 0.0, 0.0);
    let _ = context.paint();
    context.select_font_face(
        "monospace",
        cairo::FontSlant::Normal,
        cairo::FontWeight::Normal,
    );
    context.set_font_size(8.0);
    let _ = context.set_source_rgba(1.0, 1.0, 1.0, 1.0);
    context.move_to(-0.2, 7.0);
    let _ = context.show_text(&character.to_string());
    drop(context);
    surface.flush();
    let stride = surface.stride() as usize;
    let Ok(data) = surface.data() else {
        return pixels;
    };
    for y in 0..8 {
        for x in 0..5 {
            pixels[y * 5 + x] = data[y * stride + x] >= 80;
        }
    }
    pixels
}
fn to_svg(panel: &Panel, xy: (f64, f64), size: (f64, f64)) -> (f64, f64) {
    let scale = (size.0 / panel.width).min(size.1 / panel.height);
    (
        (xy.0 - (size.0 - panel.width * scale) / 2.0) / scale,
        (xy.1 - (size.1 - panel.height * scale) / 2.0) / scale,
    )
}
fn hit_svg(panel: &Panel, xy: (f64, f64)) -> Option<String> {
    let (x, y) = xy;
    let candidates = panel.shapes.iter().filter(|shape| {
        shape
            .id
            .as_ref()
            .is_some_and(|id| panel.bindings.contains_key(id) || panel.keys.contains_key(id))
    });
    let exact = candidates
        .clone()
        .filter(|shape| {
            let (a, b, w, h) = shape.bounds;
            x >= a && x <= a + w && y >= b && y <= b + h
        })
        .min_by(|a, b| (a.bounds.2 * a.bounds.3).total_cmp(&(b.bounds.2 * b.bounds.3)));
    if let Some(shape) = exact {
        return shape.id.clone();
    }
    candidates
        .filter(|shape| {
            shape
                .groups
                .iter()
                .any(|group| group == "Buttons" || group == "LEDButtons")
        })
        .filter(|shape| {
            let (a, b, w, h) = shape.bounds;
            let pad = 10.0;
            x >= a - pad && x <= a + w + pad && y >= b - pad && y <= b + h + pad
        })
        .min_by(|a, b| {
            let da = (x - (a.bounds.0 + a.bounds.2 / 2.0)).powi(2)
                + (y - (a.bounds.1 + a.bounds.3 / 2.0)).powi(2);
            let db = (x - (b.bounds.0 + b.bounds.2 / 2.0)).powi(2)
                + (y - (b.bounds.1 + b.bounds.3 / 2.0)).powi(2);
            da.total_cmp(&db)
        })
        .and_then(|shape| shape.id.clone())
}
fn regular_knob_at(panel: &Panel, xy: (f64, f64)) -> Option<usize> {
    panel.knobs.iter().position(|knob| {
        ((xy.0 - knob.center.x).powi(2) + (xy.1 - knob.center.y).powi(2)).sqrt() < 22.0
    })
}
fn rotary_at(panel: &Panel, xy: (f64, f64)) -> bool {
    ((xy.0 - panel.rotary_center.0).powi(2) + (xy.1 - panel.rotary_center.1).powi(2)).sqrt() < 48.0
}
fn pointer_angle(pos: (f64, f64), cx: f64, cy: f64) -> f64 {
    let mut angle = (pos.1 - cy).atan2(pos.0 - cx).to_degrees() + 90.0;
    if angle > 180.0 {
        angle -= 360.0;
    }
    angle.clamp(-180.0, 180.0)
}
fn midi_from_key_id(id: &str) -> Option<u8> {
    let start = id.find(|c: char| c.is_ascii_digit())?;
    let octave = id[start..].parse::<i16>().ok()?;
    let note = id[..start].replace('_', "#");
    let semitone = match note.as_str() {
        "C" => 0,
        "C#" => 1,
        "D" => 2,
        "D#" | "Eb" => 3,
        "E" => 4,
        "F" => 5,
        "F#" => 6,
        "G" => 7,
        "G#" | "Ab" => 8,
        "A" => 9,
        "A#" | "Bb" => 10,
        "B" => 11,
        _ => return None,
    };
    u8::try_from((octave + 1) * 12 + semitone).ok()
}
fn viewbox(value: &str) -> (f64, f64) {
    let numbers: Vec<f64> = value
        .split_whitespace()
        .filter_map(|part| part.parse().ok())
        .collect();
    (
        numbers.get(2).copied().unwrap_or(1969.9),
        numbers.get(3).copied().unwrap_or(460.24),
    )
}
fn collect_shapes(
    node: Node<'_, '_>,
    mut groups: Vec<String>,
    parent: (f64, f64, f64, f64),
    output: &mut Vec<Shape>,
) {
    if !node.is_element() {
        return;
    }
    if node.tag_name().name() == "g" {
        if let Some(id) = node.attribute("id") {
            groups.push(id.to_owned());
        }
    }
    let own = transform(node.attribute("transform").unwrap_or(""));
    let bounds = (
        parent.0 + own.0,
        parent.1 + own.1,
        parent.2 * own.2,
        parent.3 * own.3,
    );
    if node.tag_name().name() == "path" {
        if let Some(path) = node.attribute("d") {
            let box_bounds = quick_bounds(path);
            output.push(Shape {
                id: node.attribute("id").map(str::to_owned),
                data_name: node.attribute("data-name").map(str::to_owned),
                bounds: (
                    bounds.0 + box_bounds.0 * bounds.2,
                    bounds.1 + box_bounds.1 * bounds.3,
                    box_bounds.2 * bounds.2,
                    box_bounds.3 * bounds.3,
                ),
                groups: groups.clone(),
            });
        }
    }
    for child in node.children().filter(Node::is_element) {
        collect_shapes(child, groups.clone(), bounds, output);
    }
}
fn transform(value: &str) -> (f64, f64, f64, f64) {
    if let Some(args) = value
        .strip_prefix("translate(")
        .and_then(|v| v.strip_suffix(')'))
    {
        let n: Vec<f64> = args
            .split([',', ' '])
            .filter(|v| !v.is_empty())
            .filter_map(|v| v.parse().ok())
            .collect();
        return (
            n.first().copied().unwrap_or(0.0),
            n.get(1).copied().unwrap_or(0.0),
            1.0,
            1.0,
        );
    }
    if let Some(args) = value
        .strip_prefix("scale(")
        .and_then(|v| v.strip_suffix(')'))
    {
        let n: Vec<f64> = args
            .split([',', ' '])
            .filter(|v| !v.is_empty())
            .filter_map(|v| v.parse().ok())
            .collect();
        return (
            0.0,
            0.0,
            n.first().copied().unwrap_or(1.0),
            n.get(1)
                .copied()
                .unwrap_or_else(|| n.first().copied().unwrap_or(1.0)),
        );
    }
    (0.0, 0.0, 1.0, 1.0)
}
fn draw(ctx: &Context, width: f64, height: f64, panel: &Panel) {
    ctx.save().ok();
    ctx.set_operator(cairo::Operator::Clear);
    let _ = ctx.paint();
    ctx.set_operator(cairo::Operator::Over);
    let scale = (width / panel.width).min(height / panel.height);
    ctx.translate(
        (width - panel.width * scale) / 2.0,
        (height - panel.height * scale) / 2.0,
    );
    ctx.scale(scale, scale);
    let _ = ctx.set_source_surface(&panel.artwork_base, 0.0, 0.0);
    let _ = ctx.paint();
    for knob in &panel.knobs {
        draw_rotated_sprite(
            ctx,
            (knob.center.x, knob.center.y),
            &knob.surface,
            knob.surface_size,
            knob.angle,
        );
    }
    draw_rotated_sprite(
        ctx,
        panel.rotary_center,
        &panel.dial_surface,
        panel.dial_surface_size,
        panel.rotary_angle,
    );

    for id in &panel.display.lit_leds {
        if let Some(targets) = led_artwork_targets(id) {
            for target in targets {
                let opacity = if matches!(*target, "Voice" | "Style-2" | "Song-2" | "Groove-2") {
                    0.75
                } else {
                    0.9
                };
                fill_target_bounds(ctx, panel, target, (1.0, 0.33, 0.33, opacity));
            }
        } else {
            fill_target_bounds(ctx, panel, id, (1.0, 0.33, 0.33, 0.9));
        }
    }
    for id in &panel.display.lit_lcd_segments {
        fill_target_bounds(ctx, panel, id, (0.2, 0.2, 0.2, 1.0));
    }
    for (character_index, character) in panel.display.lcd_text.chars().take(8).enumerate() {
        let glyph = lcd_glyph_pixels(character, &panel.lcd_character_rom);
        if let Some(character_pixels) = panel.lcd_pixels.get(character_index) {
            for (pixel_index, id) in character_pixels.iter().take(40).enumerate() {
                if glyph[pixel_index] && !panel.display.lit_lcd_segments.contains(id) {
                    fill_target_bounds(ctx, panel, id, (0.2, 0.2, 0.2, 1.0));
                }
            }
        }
    }
    let _ = ctx.set_source_rgb(0.0, 0.92, 1.0);
    ctx.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
    ctx.set_font_size(20.0);
    ctx.move_to(18.0, 27.0);
    let _ = ctx.show_text(&format!(
        "Velocity delay: {} ms   Range: C{}–C{}",
        panel.contact_delay_ms,
        panel.octave,
        panel.octave + 1
    ));
    let _ = ctx.set_source_rgba(0.1, 0.8, 1.0, 0.7);
    let _ = ctx.set_line_width(2.0);
    ctx.move_to(panel.width - 14.0, panel.height - 2.0);
    ctx.line_to(panel.width - 2.0, panel.height - 14.0);
    ctx.move_to(panel.width - 8.0, panel.height - 2.0);
    ctx.line_to(panel.width - 2.0, panel.height - 8.0);
    let _ = ctx.stroke();
    ctx.restore().ok();
}

fn led_artwork_targets(id: &str) -> Option<&'static [&'static str]> {
    match id {
        "Voice" => Some(&["Voice"]),
        "Style" | "Style-2" => Some(&["Style", "Style-2"]),
        "Song" | "Song-2" => Some(&["Song", "Song-2"]),
        "Groove" | "Groove-2" => Some(&["Groove", "Groove-2"]),
        _ => None,
    }
}

fn draw_rotated_sprite(
    ctx: &Context,
    center: (f64, f64),
    surface: &cairo::ImageSurface,
    size: f64,
    angle_deg: f64,
) {
    ctx.save().ok();
    ctx.translate(center.0, center.1);
    ctx.rotate(angle_deg.to_radians());
    ctx.translate(-size / 2.0, -size / 2.0);
    let _ = ctx.set_source_surface(surface, 0.0, 0.0);
    let _ = ctx.paint();
    ctx.restore().ok();
}

fn fill_target_bounds(ctx: &Context, panel: &Panel, id: &str, color: (f64, f64, f64, f64)) {
    let matching = panel.shapes.iter().filter(|shape| {
        shape.id.as_deref() == Some(id) || shape.groups.iter().any(|group| group == id)
    });
    let mut bounds: Option<(f64, f64, f64, f64)> = None;
    for shape in matching {
        bounds = Some(match bounds {
            Some((x, y, w, h)) => {
                let right = (x + w).max(shape.bounds.0 + shape.bounds.2);
                let bottom = (y + h).max(shape.bounds.1 + shape.bounds.3);
                let left = x.min(shape.bounds.0);
                let top = y.min(shape.bounds.1);
                (left, top, right - left, bottom - top)
            }
            None => shape.bounds,
        });
    }
    if let Some((x, y, width, height)) = bounds {
        ctx.set_source_rgba(color.0, color.1, color.2, color.3);
        ctx.rectangle(x, y, width, height);
        let _ = ctx.fill();
    }
}

#[cfg(test)]
mod tests {
    use super::{collect_lcd_segment_ids, led_artwork_targets, Document};

    #[test]
    fn named_leds_drive_button_lights_and_indicator_artwork() {
        assert_eq!(led_artwork_targets("Voice"), Some(&["Voice"][..]));
        assert_eq!(led_artwork_targets("Style"), Some(&["Style", "Style-2"][..]));
        assert_eq!(led_artwork_targets("Song"), Some(&["Song", "Song-2"][..]));
        assert_eq!(led_artwork_targets("Groove"), Some(&["Groove", "Groove-2"][..]));
    }

    #[test]
    fn unknown_led_names_keep_the_generic_artwork_fallback() {
        assert_eq!(led_artwork_targets("unknown"), None);
    }

    #[test]
    fn track_status_artwork_ids_are_included_in_lcd_segments() {
        let document = Document::parse(include_str!("../PSR-E463.svg")).expect("parse panel SVG");
        let root = document.root_element();
        let tracks = root
            .descendants()
            .find(|node| node.attribute("id") == Some("Tracks"))
            .expect("find track status group");
        let track_ids = tracks
            .children()
            .filter(|node| node.tag_name().name() == "path")
            .filter_map(|node| node.attribute("id"))
            .collect::<Vec<_>>();
        let expected = [
            "LCDTrack1",
            "LCDTrack2",
            "LCDTrack3",
            "LCDTrack4",
            "LCDTrack5",
            "LCDTrack6",
            "LCDTrackA",
        ];

        assert_eq!(track_ids, expected);
        let lcd_segment_ids = collect_lcd_segment_ids(root);
        for id in expected {
            assert!(lcd_segment_ids.iter().any(|segment_id| segment_id == id));
        }
    }
}

fn render_static_panel(
    svg: &str,
    knobs: &[KnobCenter],
    dial_id: &str,
    lcd_segments: &[String],
    width: f64,
    height: f64,
) -> Option<cairo::ImageSurface> {
    let mut rules = String::from("<style>");
    for knob in knobs {
        rules.push_str(&format!("#{}{{display:none!important}}", knob.svg_id));
    }
    rules.push_str(&format!("#{dial_id}{{display:none!important}}"));
    for id in lcd_segments {
        rules.push_str(&format!("#{id}{{fill:#bee!important}}"));
    }
    rules.push_str("</style>");
    let mut static_svg = svg.to_owned();
    let end = static_svg.rfind("</svg>")?;
    static_svg.insert_str(end, &rules);
    render_svg_surface(&static_svg, width.ceil() as i32, height.ceil() as i32)
}

fn render_svg_node_surface(
    svg: &str,
    node_id: &str,
    center_x: f64,
    center_y: f64,
    size: f64,
) -> Option<cairo::ImageSurface> {
    let document = Document::parse(svg).ok()?;
    let root = document.root_element();
    let node = root
        .descendants()
        .find(|node| node.attribute("id") == Some(node_id))?;
    let defs = root
        .descendants()
        .find(|node| node.tag_name().name() == "defs")?;
    let x = center_x - size / 2.0;
    let y = center_y - size / 2.0;
    let isolated_svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{size}\" height=\"{size}\" viewBox=\"{x} {y} {size} {size}\">{}{}</svg>",
        &svg[defs.range()],
        &svg[node.range()]
    );
    render_svg_surface(&isolated_svg, size.ceil() as i32, size.ceil() as i32)
}

fn render_dial_surface(
    svg: &str,
    dial_id: &str,
    center_x: f64,
    center_y: f64,
    size: f64,
) -> Option<cairo::ImageSurface> {
    render_svg_node_surface(svg, dial_id, center_x, center_y, size)
}

fn render_svg_surface(svg: &str, width: i32, height: i32) -> Option<cairo::ImageSurface> {
    let mut error = std::ptr::null_mut();
    let bytes = svg.as_bytes();
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, width, height).ok()?;
    let render_context = Context::new(&surface).ok()?;
    unsafe {
        let handle = rsvg_handle_new_from_data(bytes.as_ptr(), bytes.len(), &mut error);
        if !handle.is_null() {
            rsvg_handle_render_cairo(handle, render_context.to_raw_none());
            g_object_unref(handle.cast());
        }
    }
    drop(render_context);
    surface.flush();
    Some(surface)
}
#[cfg_attr(
    target_os = "linux",
    link(name = "librsvg-2.so.2", kind = "dylib", modifiers = "+verbatim")
)]
#[cfg_attr(not(target_os = "linux"), link(name = "rsvg-2"))]
unsafe extern "C" {
    fn rsvg_handle_new_from_data(
        data: *const u8,
        len: usize,
        error: *mut *mut std::ffi::c_void,
    ) -> *mut std::ffi::c_void;
    fn rsvg_handle_render_cairo(handle: *mut std::ffi::c_void, cr: *mut cairo::ffi::cairo_t)
        -> i32;
}
#[link(name = "gobject-2.0")]
unsafe extern "C" {
    fn g_object_unref(object: *mut std::ffi::c_void);
}
fn quick_bounds(path: &str) -> (f64, f64, f64, f64) {
    let numbers: Vec<f64> = path
        .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .filter(|v| !v.is_empty())
        .filter_map(|v| v.parse().ok())
        .collect();
    let (mut left, mut top, mut right, mut bottom) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for pair in numbers.chunks_exact(2) {
        left = left.min(pair[0]);
        right = right.max(pair[0]);
        top = top.min(pair[1]);
        bottom = bottom.max(pair[1]);
    }
    if left.is_finite() {
        (left, top, right - left, bottom - top)
    } else {
        (0.0, 0.0, 0.0, 0.0)
    }
}
