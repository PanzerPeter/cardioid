//! The window: a channel strip with the chain across the top and the selected
//! stage's controls below it.

use crate::chain::{
    self, Band, COMPRESSOR, DEESSER, EQ, FILTER, GATE, LIMITER, NODE, NOISE, Param, Settings, Unit,
};
use crate::pw::{self, Graph, Meter};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Pos2, Rect,
    RichText, Sense, Stroke, StrokeKind, Ui, UiBuilder, Vec2, pos2, vec2,
};
use std::collections::{BTreeMap, VecDeque};
use std::io::BufRead;
use std::path::PathBuf;
use std::process::Child;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BG: Color32 = Color32::from_rgb(0x1B, 0x20, 0x28);
const PANEL: Color32 = Color32::from_rgb(0x23, 0x29, 0x34);
const RAISED: Color32 = Color32::from_rgb(0x2E, 0x36, 0x43);
const LINE: Color32 = Color32::from_rgb(0x3D, 0x47, 0x57);
const TEXT: Color32 = Color32::from_rgb(0xE8, 0xEC, 0xF2);
const MUTED: Color32 = Color32::from_rgb(0xA3, 0xAE, 0xBD);
const WARN: Color32 = Color32::from_rgb(0xF2, 0xB1, 0x3C);
const METER_OK: Color32 = Color32::from_rgb(0x4F, 0xC7, 0x8B);
const METER_HOT: Color32 = Color32::from_rgb(0xF2, 0xB1, 0x3C);
const METER_CLIP: Color32 = Color32::from_rgb(0xFF, 0x6B, 0x5E);

/// One cap colour per stage, like the coloured knobs on a console strip.
const CAPS: [Color32; 7] = [
    Color32::from_rgb(0xA8, 0x96, 0xFF),
    Color32::from_rgb(0x5A, 0xA9, 0xFF),
    Color32::from_rgb(0x38, 0xC6, 0xB4),
    Color32::from_rgb(0xF2, 0xB1, 0x3C),
    Color32::from_rgb(0xFF, 0x8F, 0xB1),
    Color32::from_rgb(0x7F, 0xD4, 0x6B),
    Color32::from_rgb(0xFF, 0x6B, 0x5E),
];
const SHORT: [&str; 7] = [
    "Noise",
    "Filter",
    "Gate",
    "Compressor",
    "De-esser",
    "EQ",
    "Limiter",
];
const BLURBS: [&str; 7] = [
    "Removes fans, keyboards and room noise. Sound below the voice threshold is muted.",
    "Cuts rumble and desk thumps below the cutoff.",
    "Turns your mic down when you stop talking.",
    "Evens out loud and quiet speech, then brings the level back up.",
    "Softens harsh S and T sounds.",
    "Shapes the tone. Drag a point on the curve, scroll over it to change its width.",
    "Stops loud moments from clipping. This is the last stage before your apps.",
];

const METER_FLOOR: f32 = -60.0;
const LEVEL_STEP: Duration = Duration::from_millis(33);
const HISTORY: usize = 300;
const HISTORY_SECONDS: u32 = 10;
const METER_DECAY_DB: f32 = 1.5;
const SILENCE_STEPS: u32 = 60;
const TEST_SECONDS: f32 = 10.0;
const EQ_RANGE: f32 = 12.0;
const FONT_MEDIUM: &str = "medium";

fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FONT_MEDIUM.into()))
}

fn load_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let medium = FontFamily::Name(FONT_MEDIUM.into());
    let fallback = fonts.families[&FontFamily::Proportional].clone();
    for (name, bytes, family) in [
        (
            "fira",
            &include_bytes!("../assets/fonts/FiraSans-Regular.ttf")[..],
            FontFamily::Proportional,
        ),
        (
            FONT_MEDIUM,
            &include_bytes!("../assets/fonts/FiraSans-Medium.ttf")[..],
            medium.clone(),
        ),
    ] {
        fonts
            .font_data
            .insert(name.into(), Arc::new(FontData::from_static(bytes)));
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, name.into());
    }
    fonts.families.entry(medium).or_default().extend(fallback);
    ctx.set_fonts(fonts);
}

fn style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.global_style_mut(|style| {
        let v = &mut style.visuals;
        v.panel_fill = BG;
        v.window_fill = PANEL;
        v.extreme_bg_color = RAISED;
        v.override_text_color = Some(TEXT);
        v.selection.bg_fill = LINE;
        v.selection.stroke = Stroke::new(1.0, TEXT);
        v.window_stroke = Stroke::new(1.0, LINE);
        for w in [&mut v.widgets.inactive, &mut v.widgets.noninteractive] {
            w.bg_fill = RAISED;
            w.weak_bg_fill = RAISED;
            w.bg_stroke = Stroke::NONE;
        }
        v.widgets.hovered.weak_bg_fill = LINE;
        v.widgets.hovered.bg_fill = LINE;
        v.widgets.active.weak_bg_fill = LINE;
        v.widgets.active.bg_fill = LINE;
        style.spacing.button_padding = vec2(10.0, 5.0);
        style.spacing.item_spacing = vec2(8.0, 8.0);
        for (text, size) in [
            (egui::TextStyle::Body, 14.0),
            (egui::TextStyle::Button, 14.0),
            (egui::TextStyle::Small, 12.0),
        ] {
            style.text_styles.insert(text, FontId::proportional(size));
        }
    });
}

pub fn run() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Cardioid")
            .with_app_id("cardioid")
            .with_inner_size([1000.0, 700.0])
            .with_min_inner_size([900.0, 660.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Cardioid",
        options,
        Box::new(|cc| Ok(Box::new(App::new(&cc.egui_ctx)))),
    )
}

/// Applies changes off the UI thread, newest first, and saves once idle.
fn engine(rx: Receiver<Settings>, mut applied: Settings) {
    let mut dirty = false;
    loop {
        match rx.recv_timeout(Duration::from_millis(400)) {
            Ok(mut latest) => {
                while let Ok(next) = rx.try_recv() {
                    latest = next;
                }
                let old = applied.ports();
                let changes: BTreeMap<String, f32> = latest
                    .ports()
                    .into_iter()
                    .filter(|(k, v)| old.get(k) != Some(v))
                    .collect();
                if !changes.is_empty() {
                    pw::set_ports(&changes);
                }
                if latest.device != applied.device {
                    pw::set_device(&latest.device);
                }
                applied = latest;
                dirty = true;
                std::thread::sleep(Duration::from_millis(30));
            }
            Err(RecvTimeoutError::Timeout) => {
                if dirty {
                    if let Err(e) = applied.save() {
                        eprintln!("save settings: {e}");
                    }
                    dirty = false;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Meter state, advanced by the clock. egui repaints on every mouse move, so
/// anything counted per frame would run faster while the pointer moves.
struct Levels {
    last: Instant,
    input: f32,
    output: f32,
    history: VecDeque<(f32, f32)>,
    silent_steps: u32,
}

impl Levels {
    fn new(now: Instant) -> Self {
        Self {
            last: now,
            input: METER_FLOOR,
            output: METER_FLOOR,
            history: VecDeque::with_capacity(HISTORY),
            silent_steps: 0,
        }
    }

    /// `read` returns the linear input and output peaks since it last ran.
    fn advance(&mut self, now: Instant, read: impl FnOnce() -> (f32, f32)) {
        let steps = (now - self.last).as_millis() as u32 / LEVEL_STEP.as_millis() as u32;
        if steps == 0 {
            return;
        }
        self.last += LEVEL_STEP * steps;
        let (input, output) = read();
        self.silent_steps = if input == 0.0 {
            self.silent_steps + steps
        } else {
            0
        };
        let to_db = |peak: f32| (20.0 * peak.max(1e-6).log10()).max(METER_FLOOR);
        let (mut input, mut output) = (to_db(input), to_db(output));
        for _ in 0..steps.min(HISTORY as u32) {
            self.input = input.max(self.input - METER_DECAY_DB);
            self.output = output.max(self.output - METER_DECAY_DB);
            (input, output) = (METER_FLOOR, METER_FLOOR);
            if self.history.len() == HISTORY {
                self.history.pop_front();
            }
            self.history.push_back((self.input, self.output));
        }
    }

    fn mic_is_silent(&self) -> bool {
        self.silent_steps >= SILENCE_STEPS
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Take {
    Filtered,
    Original,
}

impl Take {
    fn path(self) -> PathBuf {
        pw::runtime_dir().join(match self {
            Take::Filtered => "test-filtered.wav",
            Take::Original => "test-original.wav",
        })
    }
}

enum TestState {
    Idle,
    Recording {
        filtered: Child,
        original: Child,
        started: Instant,
    },
    Playing {
        child: Child,
        take: Take,
        started: Instant,
    },
}

/// Records the mic before and after the filters at the same time, so the two
/// takes are the same words and can be compared.
struct TestRecorder {
    state: TestState,
    length: Option<f32>,
    error: Option<&'static str>,
}

impl TestRecorder {
    fn record(&mut self, device: &str) {
        self.stop_playback();
        self.length = None;
        self.error = None;
        let started =
            pw::record(NODE, &Take::Filtered.path()).and_then(|filtered| {
                match pw::record(device, &Take::Original.path()) {
                    Ok(original) => Ok((filtered, original)),
                    Err(e) => {
                        pw::stop_recording(filtered);
                        Err(e)
                    }
                }
            });
        match started {
            Ok((filtered, original)) => {
                self.state = TestState::Recording {
                    filtered,
                    original,
                    started: Instant::now(),
                };
            }
            Err(_) => self.error = Some("Recording needs pw-record. Install the PipeWire tools."),
        }
    }

    fn finish_recording(&mut self) {
        if let TestState::Recording {
            filtered,
            original,
            started,
        } = std::mem::replace(&mut self.state, TestState::Idle)
        {
            pw::stop_recording(filtered);
            pw::stop_recording(original);
            let wrote = |take: Take| std::fs::metadata(take.path()).is_ok_and(|m| m.len() > 4096);
            if wrote(Take::Filtered) && wrote(Take::Original) {
                self.length = Some(started.elapsed().as_secs_f32());
            } else {
                self.error = Some("Nothing was recorded. Check that the microphone is connected.");
            }
        }
    }

    fn play(&mut self, take: Take) {
        self.stop_playback();
        match pw::play(&take.path()) {
            Ok(child) => {
                self.state = TestState::Playing {
                    child,
                    take,
                    started: Instant::now(),
                }
            }
            Err(_) => self.error = Some("Playback needs pw-play. Install the PipeWire tools."),
        }
    }

    fn stop_playback(&mut self) {
        if let TestState::Playing { child, .. } = &mut self.state {
            let _ = child.kill();
            let _ = child.wait();
            self.state = TestState::Idle;
        }
    }

    fn advance(&mut self) {
        match &mut self.state {
            TestState::Recording { started, .. } => {
                if started.elapsed().as_secs_f32() >= TEST_SECONDS {
                    self.finish_recording();
                }
            }
            TestState::Playing { child, .. } => {
                if !matches!(child.try_wait(), Ok(None)) {
                    self.state = TestState::Idle;
                }
            }
            TestState::Idle => {}
        }
    }
}

struct App {
    settings: Settings,
    sent: Settings,
    tx: Sender<Settings>,
    graph: Arc<Mutex<Option<Graph>>>,
    selected: usize,
    band: usize,
    input: Option<Meter>,
    output: Option<Meter>,
    metered_device: String,
    levels: Levels,
    test: TestRecorder,
    mic_ready: bool,
    /// (settings before a restore, the restored settings)
    undo: Option<(Settings, Settings)>,
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        load_fonts(ctx);
        style(ctx);
        let settings = Settings::load();
        let (tx, rx) = mpsc::channel();
        let applied = settings.clone();
        std::thread::spawn(move || engine(rx, applied));

        let graph = Arc::new(Mutex::new(None));
        let shared = graph.clone();
        std::thread::spawn(move || {
            loop {
                *shared.lock().expect("graph lock") = Some(pw::graph());
                std::thread::sleep(Duration::from_secs(2));
            }
        });

        // The service writes a line here when asked to show an open window.
        let raise = ctx.clone();
        std::thread::spawn(move || {
            for _ in std::io::stdin().lock().lines() {
                raise.send_viewport_cmd(egui::ViewportCommand::Focus);
                raise.request_repaint();
            }
        });

        Self {
            sent: settings.clone(),
            settings,
            tx,
            graph,
            selected: EQ,
            band: 0,
            input: None,
            output: None,
            metered_device: String::new(),
            levels: Levels::new(Instant::now()),
            test: TestRecorder {
                state: TestState::Idle,
                length: None,
                error: None,
            },
            mic_ready: false,
            undo: None,
        }
    }

    fn update_meters(&mut self) {
        if self.metered_device != self.settings.device {
            self.input = None;
            self.metered_device = self.settings.device.clone();
        }
        restart_if_dead(&mut self.input, &self.metered_device);
        restart_if_dead(&mut self.output, NODE);
        let (input, output) = (&self.input, &self.output);
        self.levels.advance(Instant::now(), || {
            let peak = |meter: &Option<Meter>| meter.as_ref().map_or(0.0, Meter::take);
            (peak(input), peak(output))
        });
    }

    fn header(&mut self, ui: &mut Ui) {
        let graph = self.graph.lock().expect("graph lock").clone();
        let mics = graph.as_ref().map(|g| g.mics.clone()).unwrap_or_default();
        let current = mics.iter().find(|(name, _)| *name == self.settings.device);
        let shown = current.map_or("Choose a microphone", |(_, label)| label.as_str());
        row(ui, |ui| {
            ui.label(RichText::new("Microphone").color(MUTED));
            egui::ComboBox::from_id_salt("mic")
                .width(280.0)
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    for (name, label) in &mics {
                        ui.selectable_value(&mut self.settings.device, name.clone(), label);
                    }
                });
            ui.add_space(12.0);
            ui.label(RichText::new("Input gain").color(MUTED));
            ui.add(
                egui::Slider::new(&mut self.settings.input_gain, -20.0..=20.0)
                    .suffix(" dB")
                    .fixed_decimals(1),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                match self.undo.take() {
                    Some((before, restored)) if restored == self.settings => {
                        if ui.button("Undo restore").clicked() {
                            self.settings = before;
                        } else {
                            self.undo = Some((before, restored));
                        }
                    }
                    _ => {
                        if ui.button("Restore preset").clicked() {
                            let before = self.settings.clone();
                            self.settings = Settings {
                                device: before.device.clone(),
                                ..Default::default()
                            };
                            self.undo = Some((before, self.settings.clone()));
                        }
                    }
                }
                let bypass = self.settings.bypass;
                let (fill, text) = if bypass { (WARN, BG) } else { (RAISED, TEXT) };
                let button = egui::Button::new(RichText::new("Bypass all").color(text)).fill(fill);
                if ui.add(button).clicked() {
                    self.settings.bypass = !bypass;
                }
            });
        });

        let running = graph.as_ref().is_none_or(|g| g.capture_id.is_some());
        let connected = graph.is_none() || current.is_some();
        self.mic_ready = running && connected;
        let (message, color) = if !running {
            (
                "The filters aren't running. Check that lsp-plugins-lv2, calf and noise-suppression-for-voice are installed.",
                WARN,
            )
        } else if !connected {
            (
                "This microphone isn't connected. Cardioid stays silent until it's back.",
                WARN,
            )
        } else if self.levels.mic_is_silent() {
            (
                "This microphone is sending pure silence. Check its mute switch and gain.",
                WARN,
            )
        } else if self.settings.bypass {
            (
                "All filters are bypassed. Apps hear your raw microphone.",
                WARN,
            )
        } else {
            (
                "Pick Cardioid as the input in OBS, Discord or any other app.",
                MUTED,
            )
        };
        ui.label(RichText::new(message).color(color));
    }

    fn strip(&mut self, ui: &mut Ui) {
        const HEIGHT: f32 = 112.0;
        const METER_W: f32 = 58.0;
        const GAP: f32 = 16.0;
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), HEIGHT), Sense::hover());
        let painter = ui.painter().clone();
        let left = Rect::from_min_size(rect.min, vec2(METER_W, HEIGHT));
        let right = Rect::from_min_size(
            pos2(rect.max.x - METER_W, rect.min.y),
            vec2(METER_W, HEIGHT),
        );
        let (input_db, output_db) = (self.levels.input, self.levels.output);
        draw_meter(&painter, left, "In", input_db);
        draw_meter(&painter, right, "Out", output_db);

        let start = left.max.x + GAP;
        let width = (right.min.x - GAP - start - GAP * 6.0) / 7.0;

        // Each piece of wire lights up in its upstream stage's colour, at the
        // level heard at that point in the chain.
        let y = rect.center().y;
        for gap in 0..8 {
            let x = start - GAP + gap as f32 * (width + GAP);
            let db = input_db + (output_db - input_db) * gap as f32 / 7.0;
            let signal = ((db - METER_FLOOR) / -METER_FLOOR).clamp(0.0, 1.0);
            let wire = LINE.lerp_to_gamma(CAPS[gap.max(1) - 1], signal);
            painter.line_segment([pos2(x, y), pos2(x + GAP, y)], Stroke::new(2.0, wire));
        }

        for i in 0..7 {
            let x = start + i as f32 * (width + GAP);
            let tile = Rect::from_min_size(pos2(x, rect.min.y), vec2(width, HEIGHT));
            self.stage_tile(ui, tile, i);
        }
    }

    fn stage_tile(&mut self, ui: &mut Ui, tile: Rect, i: usize) {
        let cap = CAPS[i];
        let id = ui.scope_id().with(("stage", i));
        let response = ui.interact(tile, id, Sense::click());
        if response.clicked() {
            self.selected = i;
        }
        let power = Rect::from_center_size(
            pos2(tile.max.x - 18.0, tile.min.y + 24.0),
            Vec2::splat(24.0),
        );
        let toggle = ui.interact(power, id.with("power"), Sense::click());
        if toggle.clicked() {
            self.settings.on[i] = !self.settings.on[i];
        }
        let live = self.settings.on[i] && !self.settings.bypass;
        let selected = self.selected == i;

        let painter = ui.painter();
        let fill = if selected {
            RAISED
        } else if response.hovered() {
            Color32::from_rgb(0x29, 0x30, 0x3C)
        } else {
            PANEL
        };
        let radius = CornerRadius::same(8);
        painter.rect_filled(tile, radius, fill);
        let border = if selected { cap } else { LINE };
        painter.rect_stroke(
            tile,
            radius,
            Stroke::new(if selected { 2.0 } else { 1.0 }, border),
            StrokeKind::Inside,
        );
        let cap_rect = Rect::from_min_size(tile.min + vec2(12.0, 14.0), vec2(22.0, 5.0));
        painter.rect_filled(
            cap_rect,
            CornerRadius::same(2),
            if live { cap } else { LINE },
        );

        let text = if live { TEXT } else { MUTED };
        painter.text(
            tile.min + vec2(12.0, 40.0),
            Align2::LEFT_TOP,
            SHORT[i],
            medium(14.5),
            text,
        );
        let summary = if live { self.summary(i) } else { "Off".into() };
        painter.text(
            tile.min + vec2(12.0, 62.0),
            Align2::LEFT_TOP,
            summary,
            FontId::proportional(13.5),
            MUTED,
        );

        let center = power.center();
        let ring = if toggle.hovered() {
            TEXT
        } else if live {
            cap
        } else {
            MUTED
        };
        power_icon(painter, center, ring);
        toggle.clone().on_hover_text(if self.settings.on[i] {
            "Turn off"
        } else {
            "Turn on"
        });
        if toggle.has_focus() || response.has_focus() {
            ui.painter().rect_stroke(
                tile.expand(2.0),
                radius,
                Stroke::new(1.5, TEXT),
                StrokeKind::Outside,
            );
        }
        let label = format!("{}, {}", chain::STAGES[i], if live { "on" } else { "off" });
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
    }

    fn summary(&self, stage: usize) -> String {
        let s = &self.settings;
        match stage {
            NOISE => format_value(Unit::Percent, s.get("nr.vad")),
            FILTER => format_value(Unit::Hz, s.get("hp.freq")),
            GATE => format_value(Unit::Db, s.get("gate.threshold")),
            COMPRESSOR => format_value(Unit::Ratio, s.get("comp.ratio")),
            DEESSER => format_value(Unit::Hz, s.get("ds.peak")),
            EQ => match s.eq.iter().filter(|b| b.kind != 0).count() {
                1 => "1 band".into(),
                n => format!("{n} bands"),
            },
            _ => format_value(Unit::Db, s.get("lim.threshold")),
        }
    }

    fn controls(&mut self, ui: &mut Ui) {
        let stage = self.selected;
        ui.label(RichText::new(chain::STAGES[stage]).font(medium(20.0)));
        ui.label(RichText::new(BLURBS[stage]).color(MUTED));
        ui.add_space(6.0);
        if stage == EQ {
            self.eq_controls(ui);
            return;
        }
        let knobs: Vec<&Param> = chain::PARAMS
            .iter()
            .filter(|p| p.stage == stage && p.unit != Unit::Choice)
            .collect();
        for row_of_knobs in knobs.chunks(4) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for p in row_of_knobs {
                    let mut value = self.settings.get(p.key);
                    let usable = matters(&self.settings, p.key);
                    let spec = Spec::of(p);
                    let changed = ui.add_enabled_ui(usable, |ui| {
                        knob(ui, CAPS[stage], p.label, spec, &mut value)
                    });
                    if changed.inner {
                        self.settings.values.insert(p.key.into(), value);
                    }
                }
            });
        }
        for p in chain::PARAMS
            .iter()
            .filter(|p| p.stage == stage && p.unit == Unit::Choice)
        {
            ui.add_space(4.0);
            row(ui, |ui| {
                ui.label(RichText::new(p.label).color(MUTED));
                let mut current = self.settings.get(p.key);
                if segmented(ui, CAPS[stage], p.choices, &mut current) {
                    self.settings.values.insert(p.key.into(), current);
                }
            });
        }
    }

    fn eq_controls(&mut self, ui: &mut Ui) {
        let cap = CAPS[EQ];
        row(ui, |ui| {
            ui.label(RichText::new("Band").color(MUTED));
            let bands: Vec<(String, usize)> = (0..8).map(|i| ((i + 1).to_string(), i)).collect();
            segmented(ui, cap, &bands, &mut self.band);
        });
        let band = &mut self.settings.eq[self.band];
        row(ui, |ui| {
            ui.label(RichText::new("Shape").color(MUTED));
            segmented(ui, cap, chain::BAND_KINDS, &mut band.kind);
        });
        let preset = chain::PRESET_EQ[self.band];
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let spec = |unit, min, max, preset| Spec {
                unit,
                min,
                max,
                preset,
            };
            knob(
                ui,
                cap,
                "Frequency",
                spec(Unit::Hz, 20.0, 20000.0, preset.freq),
                &mut band.freq,
            );
            knob(
                ui,
                cap,
                "Gain",
                spec(Unit::Db, -EQ_RANGE, EQ_RANGE, preset.gain),
                &mut band.gain,
            );
            knob(
                ui,
                cap,
                "Width (Q)",
                spec(Unit::Plain, 0.2, 10.0, preset.q),
                &mut band.q,
            );
        });
    }

    fn display(&mut self, ui: &mut Ui, rect: Rect) {
        ui.allocate_rect(rect, Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::same(8), PANEL);
        let cap = CAPS[self.selected];
        let s = &self.settings;
        let plot = Rect::from_min_max(rect.min + vec2(44.0, 16.0), rect.max - vec2(16.0, 30.0));
        match self.selected {
            FILTER => {
                let (cutoff, slope) = (s.get("hp.freq"), s.get("hp.slope"));
                freq_grid(&painter, plot, 24.0);
                freq_curve(&painter, plot, 24.0, cap, |f| {
                    chain::highpass_db(cutoff, slope, f)
                });
            }
            EQ => {
                freq_grid(&painter, plot, EQ_RANGE);
                let bands = s.eq;
                freq_curve(&painter, plot, EQ_RANGE, cap, |f| chain::eq_db(&bands, f));
                self.eq_points(ui, plot);
            }
            GATE => {
                let (t, r) = (s.get("gate.threshold"), s.get("gate.reduction"));
                transfer(&painter, plot, cap, |x| chain::gate_out(x, t, r));
            }
            COMPRESSOR => {
                let (t, r, k, m) = (
                    s.get("comp.threshold"),
                    s.get("comp.ratio"),
                    s.get("comp.knee"),
                    s.get("comp.makeup"),
                );
                transfer(&painter, plot, cap, |x| {
                    chain::compressor_out(x, t, r, k, m)
                });
            }
            LIMITER => {
                let t = s.get("lim.threshold");
                let lift = if s.get("lim.boost") > 0.5 { -t } else { 0.0 };
                transfer(&painter, plot, cap, |x| x.min(t) + lift);
            }
            NOISE | DEESSER => history(&painter, plot, cap, &self.levels.history),
            _ => {}
        }
    }

    fn eq_points(&mut self, ui: &mut Ui, plot: Rect) {
        for i in 0..8 {
            let band: Band = self.settings.eq[i];
            if band.kind == 0 {
                continue;
            }
            let center = pos2(freq_x(plot, band.freq), db_y(plot, EQ_RANGE, band.gain));
            let id = ui.scope_id().with(("band", i));
            let hit = ui.interact(
                Rect::from_center_size(center, Vec2::splat(22.0)),
                id,
                Sense::click_and_drag(),
            );
            if hit.clicked() || hit.drag_started() {
                self.band = i;
            }
            if hit.dragged()
                && let Some(p) = hit.interact_pointer_pos()
            {
                let band = &mut self.settings.eq[i];
                band.freq = x_freq(plot, p.x).clamp(20.0, 20000.0);
                band.gain = y_db(plot, EQ_RANGE, p.y).clamp(-EQ_RANGE, EQ_RANGE);
            }
            if hit.hovered() {
                let scroll = ui.input(|input| input.smooth_scroll_delta.y);
                let band = &mut self.settings.eq[i];
                band.q = (band.q * (1.0 + scroll * 0.004)).clamp(0.2, 10.0);
            }
            let chosen = self.band == i;
            let painter = ui.painter();
            painter.circle_filled(
                center,
                if chosen { 8.0 } else { 6.0 },
                if chosen { CAPS[EQ] } else { PANEL },
            );
            painter.circle_stroke(
                center,
                if chosen { 8.0 } else { 6.0 },
                Stroke::new(1.6, CAPS[EQ]),
            );
            let number = if chosen { BG } else { TEXT };
            painter.text(
                center,
                Align2::CENTER_CENTER,
                (i + 1).to_string(),
                FontId::proportional(10.0),
                number,
            );
        }
    }
}

/// Whether a control has any effect with the current settings.
fn matters(settings: &Settings, key: &str) -> bool {
    match key {
        "gate.hyst_th" => settings.get("gate.hyst") > 0.5,
        "ds.split" => settings.get("ds.mode") > 0.5,
        _ => true,
    }
}

/// A row with labels centred against the taller buttons beside them.
fn row(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let size = vec2(ui.available_width(), 30.0);
    ui.allocate_ui_with_layout(size, egui::Layout::left_to_right(egui::Align::Center), add);
}

fn restart_if_dead(meter: &mut Option<Meter>, target: &str) {
    if !meter.as_mut().is_some_and(Meter::alive) && !target.is_empty() {
        *meter = Meter::start(target).ok();
    }
}

impl App {
    fn test_bar(&mut self, ui: &mut Ui) {
        enum Action {
            Record,
            StopRecording,
            Play(Take),
            StopPlayback,
        }
        self.test.advance();
        let mut action = None;
        let recording = matches!(self.test.state, TestState::Recording { .. });
        let playing = match self.test.state {
            TestState::Playing { take, .. } => Some(take),
            _ => None,
        };
        row(ui, |ui| {
            if recording {
                if icon_button(ui, Icon::Stop, METER_CLIP, "Stop recording").clicked() {
                    action = Some(Action::StopRecording);
                }
            } else {
                let label = if self.test.length.is_some() {
                    "Record again"
                } else {
                    "Record a test"
                };
                let ready = self.mic_ready;
                let button =
                    ui.add_enabled_ui(ready, |ui| icon_button(ui, Icon::Record, METER_CLIP, label));
                if button.inner.clicked() {
                    action = Some(Action::Record);
                }
            }
            if self.test.length.is_some() {
                for (take, label) in [
                    (Take::Filtered, "With filters"),
                    (Take::Original, "Original"),
                ] {
                    let active = playing == Some(take);
                    let icon = if active { Icon::Stop } else { Icon::Play };
                    if icon_button(ui, icon, CAPS[GATE], label).clicked() {
                        action = Some(if active {
                            Action::StopPlayback
                        } else {
                            Action::Play(take)
                        });
                    }
                }
            }
            ui.add_space(6.0);
            let (message, color, progress) = self.test_status();
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
            let painter = ui.painter();
            let text_at = if progress.is_some() {
                rect.left_top() + vec2(0.0, 3.0)
            } else {
                rect.left_center()
            };
            let anchor = if progress.is_some() {
                Align2::LEFT_TOP
            } else {
                Align2::LEFT_CENTER
            };
            painter.text(text_at, anchor, message, FontId::proportional(13.5), color);
            if let Some(t) = progress {
                let track = Rect::from_min_max(pos2(rect.min.x, rect.max.y - 5.0), rect.max);
                painter.rect_filled(track, CornerRadius::same(2), RAISED);
                let done = Rect::from_min_size(
                    track.min,
                    vec2(track.width() * t.clamp(0.0, 1.0), track.height()),
                );
                painter.rect_filled(
                    done,
                    CornerRadius::same(2),
                    if recording { METER_CLIP } else { CAPS[GATE] },
                );
            }
        });
        match action {
            Some(Action::Record) => {
                let device = self.settings.device.clone();
                self.test.record(&device);
            }
            Some(Action::StopRecording) => self.test.finish_recording(),
            Some(Action::Play(take)) => self.test.play(take),
            Some(Action::StopPlayback) => self.test.stop_playback(),
            None => {}
        }
    }

    fn test_status(&self) -> (String, Color32, Option<f32>) {
        match &self.test.state {
            TestState::Recording { started, .. } => {
                let elapsed = started.elapsed().as_secs_f32();
                let left = (TEST_SECONDS - elapsed).ceil();
                (
                    format!("Talk normally. {left:.0} s left"),
                    TEXT,
                    Some(elapsed / TEST_SECONDS),
                )
            }
            TestState::Playing { started, .. } => {
                let length = self.test.length.unwrap_or(TEST_SECONDS);
                let elapsed = started.elapsed().as_secs_f32();
                (
                    "Playing on your default output".into(),
                    TEXT,
                    Some(elapsed / length),
                )
            }
            TestState::Idle => match (self.test.error, self.test.length) {
                (Some(error), _) => (error.into(), WARN, None),
                (None, Some(_)) => (
                    "Same words, before and after the filters.".into(),
                    MUTED,
                    None,
                ),
                (None, None) => (
                    "Records 10 seconds of your mic, before and after the filters.".into(),
                    MUTED,
                    None,
                ),
            },
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.update_meters();
        egui::Panel::top("top")
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin {
                left: 18,
                right: 18,
                top: 14,
                bottom: 10,
            }))
            .show_separator_line(false)
            .show(ui, |ui| {
                self.header(ui);
                ui.add_space(4.0);
                self.strip(ui);
            });
        egui::Panel::bottom("test")
            .frame(egui::Frame::new().fill(PANEL).inner_margin(egui::Margin {
                left: 18,
                right: 18,
                top: 10,
                bottom: 10,
            }))
            .show_separator_line(false)
            .show(ui, |ui| self.test_bar(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin {
                left: 18,
                right: 18,
                top: 8,
                bottom: 14,
            }))
            .show(ui, |ui| {
                let area = ui.available_rect_before_wrap();
                let split = area.min.x + 432.0;
                let left = Rect::from_min_max(area.min, pos2(split, area.max.y));
                let right = Rect::from_min_max(pos2(split + 16.0, area.min.y), area.max);
                ui.scope_builder(UiBuilder::new().max_rect(left), |ui| self.controls(ui));
                self.display(ui, right);
            });
        if self.settings != self.sent {
            self.sent = self.settings.clone();
            let _ = self.tx.send(self.sent.clone());
        }
        ui.ctx().request_repaint_after(Duration::from_millis(33));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Err(e) = self.settings.save() {
            eprintln!("save settings: {e}");
        }
        self.test.finish_recording();
        self.test.stop_playback();
        for take in [Take::Filtered, Take::Original] {
            let _ = std::fs::remove_file(take.path());
        }
    }
}

#[derive(Clone, Copy)]
struct Spec {
    unit: Unit,
    min: f32,
    max: f32,
    preset: f32,
}

impl Spec {
    fn of(p: &Param) -> Self {
        Self {
            unit: p.unit,
            min: p.min,
            max: p.max,
            preset: p.preset,
        }
    }

    /// Time, frequency and ratio feel right on a log scale when the range allows it.
    fn log(self) -> bool {
        self.min > 0.0 && matches!(self.unit, Unit::Hz | Unit::Ms | Unit::Ratio | Unit::Plain)
    }

    fn position(self, v: f32) -> f32 {
        let t = if self.log() {
            (v / self.min).ln() / (self.max / self.min).ln()
        } else {
            (v - self.min) / (self.max - self.min)
        };
        t.clamp(0.0, 1.0)
    }

    fn value_at(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        if self.log() {
            self.min * (self.max / self.min).powf(t)
        } else {
            self.min + (self.max - self.min) * t
        }
    }
}

fn format_value(unit: Unit, v: f32) -> String {
    match unit {
        Unit::Db => format!("{v:.1} dB"),
        Unit::Ms if v >= 100.0 => format!("{v:.0} ms"),
        Unit::Ms => format!("{v:.1} ms"),
        Unit::Hz if v >= 1000.0 => format!("{:.2} kHz", v / 1000.0),
        Unit::Hz => format!("{v:.0} Hz"),
        Unit::Ratio => format!("{v:.1}:1"),
        Unit::Plain => format!("{v:.1}"),
        Unit::Percent => format!("{v:.0} %"),
        Unit::Choice => String::new(),
    }
}

/// Drag up or down, scroll, or use the arrow keys. Shift is fine control,
/// double-click restores the preset. Returns true when the value changed.
fn knob(ui: &mut Ui, cap: Color32, label: &str, spec: Spec, value: &mut f32) -> bool {
    use std::f32::consts::PI;
    const START: f32 = 0.75 * PI;
    const SWEEP: f32 = 1.5 * PI;

    let (rect, response) = ui.allocate_exact_size(vec2(100.0, 106.0), Sense::click_and_drag());
    let before = *value;
    let mut t = spec.position(*value);
    let fine = ui.input(|i| i.modifiers.shift);
    if response.dragged() {
        t -= response.drag_delta().y / if fine { 900.0 } else { 180.0 };
    }
    if response.hovered() {
        t += ui.input(|i| i.smooth_scroll_delta.y) / 1200.0;
    }
    if response.has_focus() {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                response.id,
                egui::EventFilter {
                    vertical_arrows: true,
                    ..Default::default()
                },
            )
        });
        let step = if fine { 0.002 } else { 0.01 };
        let (up, down) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
            )
        });
        t += step * (up as i32 - down as i32) as f32;
    }
    if t != spec.position(*value) {
        *value = spec.value_at(t);
    }
    if response.double_clicked() {
        *value = spec.preset;
    }

    let painter = ui.painter();
    painter.text(
        rect.center_top() + vec2(0.0, 2.0),
        Align2::CENTER_TOP,
        label,
        FontId::proportional(13.0),
        MUTED,
    );
    let center = rect.center() + vec2(0.0, 1.0);
    let radius = 26.0;
    let arc = |from: f32, to: f32| -> Vec<Pos2> {
        (0..=40)
            .map(|i| {
                let a = START + SWEEP * (from + (to - from) * i as f32 / 40.0);
                center + radius * Vec2::angled(a)
            })
            .collect()
    };
    let shown = spec.position(*value);
    painter.add(egui::Shape::line(arc(0.0, 1.0), Stroke::new(5.0, RAISED)));
    painter.add(egui::Shape::line(arc(0.0, shown), Stroke::new(5.0, cap)));
    let dir = Vec2::angled(START + SWEEP * shown);
    painter.line_segment(
        [center + dir * 8.0, center + dir * 18.0],
        Stroke::new(2.5, TEXT),
    );
    let hot = response.hovered() || response.dragged();
    painter.text(
        rect.center_bottom() - vec2(0.0, 2.0),
        Align2::CENTER_BOTTOM,
        format_value(spec.unit, *value),
        FontId::proportional(13.5),
        if hot { cap } else { TEXT },
    );
    if response.has_focus() {
        painter.rect_stroke(
            rect,
            CornerRadius::same(6),
            Stroke::new(1.5, TEXT),
            StrokeKind::Inside,
        );
    }
    let text = format!("{label} {}", format_value(spec.unit, *value));
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Slider, true, &text));
    response
        .on_hover_text("Drag or scroll. Hold Shift for fine control. Double-click for the preset.");
    *value != before
}

/// One-of-several control that takes the stage's cap colour when selected.
fn segmented<S: AsRef<str>, T: Copy + PartialEq>(
    ui: &mut Ui,
    cap: Color32,
    options: &[(S, T)],
    current: &mut T,
) -> bool {
    const PADDING: f32 = 11.0;
    const HEIGHT: f32 = 28.0;
    let font = FontId::proportional(13.5);
    let widths: Vec<f32> = options
        .iter()
        .map(|(label, _)| {
            let galley = ui
                .painter()
                .layout_no_wrap(label.as_ref().into(), font.clone(), TEXT);
            galley.size().x + PADDING * 2.0
        })
        .collect();
    let (rect, _) = ui.allocate_exact_size(vec2(widths.iter().sum(), HEIGHT), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(6), RAISED);
    let mut x = rect.min.x;
    let mut changed = false;
    for ((label, value), width) in options.iter().zip(widths) {
        let segment = Rect::from_min_size(pos2(x, rect.min.y), vec2(width, HEIGHT));
        x += width;
        let id = ui
            .scope_id()
            .with(("segment", label.as_ref(), rect.min.y as i32));
        let response = ui.interact(segment, id, Sense::click());
        if response.clicked() && *current != *value {
            *current = *value;
            changed = true;
        }
        let selected = *current == *value;
        let painter = ui.painter();
        let inner = segment.shrink(2.0);
        if selected {
            painter.rect_filled(inner, CornerRadius::same(4), cap);
        } else if response.hovered() {
            painter.rect_filled(inner, CornerRadius::same(4), LINE);
        }
        let color = if selected { BG } else { TEXT };
        painter.text(
            segment.center(),
            Align2::CENTER_CENTER,
            label.as_ref(),
            font.clone(),
            color,
        );
        if response.has_focus() {
            painter.rect_stroke(
                segment,
                CornerRadius::same(5),
                Stroke::new(1.5, TEXT),
                StrokeKind::Inside,
            );
        }
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::RadioButton, true, selected, label.as_ref())
        });
    }
    changed
}

#[derive(Clone, Copy)]
enum Icon {
    Record,
    Play,
    Stop,
}

fn icon_button(ui: &mut Ui, icon: Icon, color: Color32, label: &str) -> egui::Response {
    let font = FontId::proportional(14.0);
    let text_width = ui
        .painter()
        .layout_no_wrap(label.into(), font.clone(), TEXT)
        .size()
        .x;
    let (rect, response) = ui.allocate_exact_size(vec2(text_width + 46.0, 32.0), Sense::click());
    let painter = ui.painter();
    let fill = if response.hovered() { LINE } else { RAISED };
    painter.rect_filled(rect, CornerRadius::same(6), fill);
    let center = pos2(rect.min.x + 19.0, rect.center().y);
    match icon {
        Icon::Record => {
            painter.circle_filled(center, 5.5, color);
        }
        Icon::Stop => {
            painter.rect_filled(
                Rect::from_center_size(center, Vec2::splat(10.0)),
                CornerRadius::same(1),
                color,
            );
        }
        Icon::Play => {
            let points = vec![
                center + vec2(-4.5, -6.0),
                center + vec2(6.5, 0.0),
                center + vec2(-4.5, 6.0),
            ];
            painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
        }
    }
    painter.text(
        pos2(rect.min.x + 33.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font,
        TEXT,
    );
    if response.has_focus() {
        painter.rect_stroke(
            rect,
            CornerRadius::same(6),
            Stroke::new(1.5, TEXT),
            StrokeKind::Inside,
        );
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    response
}

fn power_icon(painter: &egui::Painter, center: Pos2, color: Color32) {
    use std::f32::consts::{FRAC_PI_2, PI};
    let stroke = Stroke::new(1.8, color);
    let gap = 0.28 * PI;
    let arc: Vec<Pos2> = (0..=24)
        .map(|i| {
            let a = -FRAC_PI_2 + gap + (2.0 * PI - 2.0 * gap) * i as f32 / 24.0;
            center + 7.0 * Vec2::angled(a)
        })
        .collect();
    painter.add(egui::Shape::line(arc, stroke));
    painter.line_segment([center + vec2(0.0, -8.5), center + vec2(0.0, -2.0)], stroke);
}

fn draw_meter(painter: &egui::Painter, rect: Rect, label: &str, db: f32) {
    painter.text(
        rect.center_top(),
        Align2::CENTER_TOP,
        label,
        FontId::proportional(13.0),
        MUTED,
    );
    let readout = if db <= METER_FLOOR {
        "silent".to_string()
    } else {
        format!("{db:.0} dB")
    };
    painter.text(
        rect.center_bottom(),
        Align2::CENTER_BOTTOM,
        readout,
        FontId::proportional(12.5),
        TEXT,
    );
    let bar = Rect::from_center_size(rect.center(), vec2(16.0, rect.height() - 42.0));
    painter.rect_filled(bar, CornerRadius::same(3), RAISED);
    let t = ((db - METER_FLOOR) / -METER_FLOOR).clamp(0.0, 1.0);
    let segments = 24;
    for i in 0..segments {
        let lo = i as f32 / segments as f32;
        if lo >= t {
            break;
        }
        let level = METER_FLOOR * (1.0 - lo);
        let color = match level {
            l if l > -3.0 => METER_CLIP,
            l if l > -12.0 => METER_HOT,
            _ => METER_OK,
        };
        let h = bar.height() / segments as f32;
        let y = bar.max.y - (i + 1) as f32 * h;
        let seg = Rect::from_min_size(
            pos2(bar.min.x + 2.0, y + 0.5),
            vec2(bar.width() - 4.0, h - 1.0),
        );
        painter.rect_filled(seg, CornerRadius::ZERO, color);
    }
}

fn freq_x(plot: Rect, f: f32) -> f32 {
    plot.min.x + plot.width() * (f / 20.0).log(1000.0)
}

fn x_freq(plot: Rect, x: f32) -> f32 {
    20.0 * 1000f32.powf((x - plot.min.x) / plot.width())
}

fn db_y(plot: Rect, range: f32, db: f32) -> f32 {
    plot.center().y - db / range * plot.height() / 2.0
}

fn y_db(plot: Rect, range: f32, y: f32) -> f32 {
    (plot.center().y - y) / (plot.height() / 2.0) * range
}

fn grid_label(painter: &egui::Painter, pos: Pos2, anchor: Align2, text: String) {
    painter.text(pos, anchor, text, FontId::proportional(12.0), MUTED);
}

fn freq_grid(painter: &egui::Painter, plot: Rect, range: f32) {
    let line = Stroke::new(1.0, RAISED);
    for f in [
        20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0,
    ] {
        let x = freq_x(plot, f);
        painter.line_segment([pos2(x, plot.min.y), pos2(x, plot.max.y)], line);
        let text = if f >= 1000.0 {
            format!("{}k", f / 1000.0)
        } else {
            format!("{f}")
        };
        grid_label(painter, pos2(x, plot.max.y + 6.0), Align2::CENTER_TOP, text);
    }
    let step = range / 3.0;
    for i in -3..=3 {
        let db = i as f32 * step;
        let y = db_y(plot, range, db);
        let stroke = if i == 0 { Stroke::new(1.0, LINE) } else { line };
        painter.line_segment([pos2(plot.min.x, y), pos2(plot.max.x, y)], stroke);
        grid_label(
            painter,
            pos2(plot.min.x - 8.0, y),
            Align2::RIGHT_CENTER,
            format!("{db:+.0}"),
        );
    }
}

fn freq_curve(
    painter: &egui::Painter,
    plot: Rect,
    range: f32,
    cap: Color32,
    db: impl Fn(f32) -> f32,
) {
    let points: Vec<Pos2> = (0..=plot.width() as usize)
        .map(|i| {
            let x = plot.min.x + i as f32;
            pos2(
                x,
                db_y(plot, range, db(x_freq(plot, x)).clamp(-range, range)),
            )
        })
        .collect();
    painter.add(egui::Shape::line(points, Stroke::new(2.5, cap)));
}

/// Input level against output level, -80 to 0 dB on both axes.
fn transfer(painter: &egui::Painter, plot: Rect, cap: Color32, out: impl Fn(f32) -> f32) {
    let x_of = |db: f32| plot.min.x + plot.width() * (db + 80.0) / 80.0;
    let y_of = |db: f32| plot.max.y - plot.height() * (db + 80.0) / 80.0;
    let line = Stroke::new(1.0, RAISED);
    for db in (-80..=0).step_by(20).map(|d| d as f32) {
        painter.line_segment(
            [pos2(x_of(db), plot.min.y), pos2(x_of(db), plot.max.y)],
            line,
        );
        painter.line_segment(
            [pos2(plot.min.x, y_of(db)), pos2(plot.max.x, y_of(db))],
            line,
        );
        grid_label(
            painter,
            pos2(x_of(db), plot.max.y + 6.0),
            Align2::CENTER_TOP,
            format!("{db:.0}"),
        );
        grid_label(
            painter,
            pos2(plot.min.x - 8.0, y_of(db)),
            Align2::RIGHT_CENTER,
            format!("{db:.0}"),
        );
    }
    painter.line_segment(
        [pos2(x_of(-80.0), y_of(-80.0)), pos2(x_of(0.0), y_of(0.0))],
        Stroke::new(1.0, LINE),
    );
    let points: Vec<Pos2> = (0..=320)
        .map(|i| {
            let input = -80.0 + i as f32 / 4.0;
            pos2(x_of(input), y_of(out(input).clamp(-80.0, 0.0)))
        })
        .collect();
    painter.add(egui::Shape::line(points, Stroke::new(2.5, cap)));
}

fn history(painter: &egui::Painter, plot: Rect, cap: Color32, levels: &VecDeque<(f32, f32)>) {
    let y_of = |db: f32| plot.max.y - plot.height() * (db - METER_FLOOR) / -METER_FLOOR;
    for db in (-60..=0).step_by(20).map(|d| d as f32) {
        painter.line_segment(
            [pos2(plot.min.x, y_of(db)), pos2(plot.max.x, y_of(db))],
            Stroke::new(1.0, RAISED),
        );
        grid_label(
            painter,
            pos2(plot.min.x - 8.0, y_of(db)),
            Align2::RIGHT_CENTER,
            format!("{db:.0}"),
        );
    }
    let bottom = plot.max.y + 6.0;
    grid_label(
        painter,
        pos2(plot.min.x, bottom),
        Align2::LEFT_TOP,
        format!("{HISTORY_SECONDS} s ago"),
    );
    grid_label(
        painter,
        pos2(plot.max.x, bottom),
        Align2::RIGHT_TOP,
        "Now".into(),
    );

    let trace = |pick: fn(&(f32, f32)) -> f32| -> Vec<Pos2> {
        let step = plot.width() / (HISTORY - 1) as f32;
        let offset = HISTORY - levels.len();
        levels
            .iter()
            .enumerate()
            .map(|(i, v)| pos2(plot.min.x + (i + offset) as f32 * step, y_of(pick(v))))
            .collect()
    };
    painter.add(egui::Shape::line(trace(|v| v.0), Stroke::new(1.5, MUTED)));
    painter.add(egui::Shape::line(trace(|v| v.1), Stroke::new(2.5, cap)));

    let y = bottom + 7.0;
    let mut x = plot.center().x - 105.0;
    for (label, color) in [("Your mic", MUTED), ("What apps hear", cap)] {
        painter.line_segment([pos2(x, y), pos2(x + 18.0, y)], Stroke::new(2.5, color));
        let font = FontId::proportional(12.5);
        x = painter
            .text(pos2(x + 24.0, y), Align2::LEFT_CENTER, label, font, TEXT)
            .max
            .x
            + 20.0;
    }
    if levels
        .iter()
        .all(|v| v.0 <= METER_FLOOR && v.1 <= METER_FLOOR)
    {
        painter.text(
            plot.center(),
            Align2::CENTER_CENTER,
            "Waiting for sound",
            FontId::proportional(14.0),
            MUTED,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knob_scale_round_trips() {
        for spec in [
            Spec {
                unit: Unit::Hz,
                min: 20.0,
                max: 20000.0,
                preset: 1000.0,
            },
            Spec {
                unit: Unit::Db,
                min: -80.0,
                max: 0.0,
                preset: -50.0,
            },
            Spec {
                unit: Unit::Ms,
                min: 0.0,
                max: 1000.0,
                preset: 200.0,
            },
        ] {
            let back = spec.value_at(spec.position(spec.preset));
            assert!((back - spec.preset).abs() < spec.preset.abs() * 1e-4 + 1e-3);
        }
    }

    #[test]
    fn history_follows_the_clock_and_not_the_frame_rate() {
        let start = Instant::now();
        let mut levels = Levels::new(start);
        for ms in 1..=1000 {
            levels.advance(start + Duration::from_millis(ms), || (0.5, 0.5));
        }
        let fast = levels.history.len();

        let mut levels = Levels::new(start);
        for frame in 1..=10 {
            levels.advance(start + Duration::from_millis(frame * 100), || (0.5, 0.5));
        }
        assert_eq!(fast, levels.history.len());
        assert_eq!(fast, 30);
    }

    #[test]
    fn meter_falls_at_the_same_speed_at_any_frame_rate() {
        let start = Instant::now();
        let fall = |frame_ms: u64| {
            let mut levels = Levels::new(start);
            levels.advance(start + LEVEL_STEP, || (1.0, 1.0));
            for ms in (frame_ms..=330).step_by(frame_ms as usize) {
                levels.advance(start + LEVEL_STEP + Duration::from_millis(ms), || {
                    (0.0, 0.0)
                });
            }
            levels.input
        };
        assert!((fall(1) - fall(100)).abs() <= METER_DECAY_DB);
    }

    #[test]
    fn silence_warning_needs_two_seconds_of_exact_zero() {
        let start = Instant::now();
        let mut levels = Levels::new(start);
        levels.advance(start + Duration::from_millis(1000), || (0.0, 0.0));
        assert!(!levels.mic_is_silent());
        levels.advance(start + Duration::from_millis(2100), || (0.0, 0.0));
        assert!(levels.mic_is_silent());
        levels.advance(start + Duration::from_millis(2200), || (0.0001, 0.0));
        assert!(!levels.mic_is_silent());
    }

    #[test]
    fn frequency_axis_round_trips() {
        let plot = Rect::from_min_size(Pos2::ZERO, vec2(500.0, 200.0));
        assert!((x_freq(plot, freq_x(plot, 3000.0)) - 3000.0).abs() < 0.5);
        assert!((y_db(plot, 18.0, db_y(plot, 18.0, 6.0)) - 6.0).abs() < 1e-3);
    }
}
