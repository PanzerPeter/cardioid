//! Filter chain model. `ports()` is the single source of truth: it feeds both
//! the generated PipeWire config and the live parameter updates.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::{env, fs};

pub const NODE: &str = "cardioid";
pub const CAPTURE: &str = "cardioid_capture";
pub const SAMPLE_RATE: f32 = 48000.0;

pub const NOISE: usize = 0;
pub const FILTER: usize = 1;
pub const GATE: usize = 2;
pub const COMPRESSOR: usize = 3;
pub const DEESSER: usize = 4;
pub const EQ: usize = 5;
pub const LIMITER: usize = 6;
pub const STAGES: [&str; 7] = [
    "Noise reduction",
    "Filter",
    "Gate",
    "Compressor",
    "De-esser",
    "Equalizer",
    "Limiter",
];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Unit {
    Db,
    Ms,
    Hz,
    Ratio,
    Plain,
    Percent,
    Choice,
}

pub struct Param {
    pub stage: usize,
    pub key: &'static str,
    pub label: &'static str,
    pub unit: Unit,
    pub min: f32,
    pub max: f32,
    pub preset: f32,
    pub port: &'static str,
    pub choices: &'static [(&'static str, f32)],
}

const fn knob(
    stage: usize,
    key: &'static str,
    label: &'static str,
    unit: Unit,
    range: (f32, f32),
    preset: f32,
    port: &'static str,
) -> Param {
    Param {
        stage,
        key,
        label,
        unit,
        min: range.0,
        max: range.1,
        preset,
        port,
        choices: &[],
    }
}

const fn choice(
    stage: usize,
    key: &'static str,
    label: &'static str,
    preset: f32,
    port: &'static str,
    choices: &'static [(&'static str, f32)],
) -> Param {
    Param {
        stage,
        key,
        label,
        unit: Unit::Choice,
        min: 0.0,
        max: 0.0,
        preset,
        port,
        choices,
    }
}

const OFF_ON: &[(&str, f32)] = &[("Off", 0.0), ("On", 1.0)];

/// Preset values were tuned by ear on a HyperX DuoCast.
#[rustfmt::skip]
pub const PARAMS: &[Param] = &[
    knob(NOISE, "nr.vad", "Voice threshold", Unit::Percent, (0.0, 99.0), 65.0, "nr:VAD Threshold (%)"),
    knob(NOISE, "nr.grace", "Hold after speech", Unit::Ms, (0.0, 1000.0), 200.0, "nr:VAD Grace Period (ms)"),
    knob(NOISE, "nr.retro", "Keep word starts", Unit::Ms, (0.0, 200.0), 0.0, "nr:Retroactive VAD Grace (ms)"),
    knob(FILTER, "hp.freq", "Cutoff", Unit::Hz, (20.0, 500.0), 90.0, "hp:f"),
    choice(FILTER, "hp.slope", "Slope", 1.0, "hp:s", &[("12 dB", 0.0), ("24 dB", 1.0), ("36 dB", 2.0), ("48 dB", 3.0)]),
    knob(GATE, "gate.threshold", "Threshold", Unit::Db, (-80.0, 0.0), -50.0, "gate:gt"),
    knob(GATE, "gate.reduction", "Reduction", Unit::Db, (-72.0, 0.0), -40.0, "gate:gr"),
    knob(GATE, "gate.attack", "Attack", Unit::Ms, (0.1, 500.0), 10.0, "gate:at"),
    knob(GATE, "gate.hold", "Hold", Unit::Ms, (0.0, 1000.0), 0.0, "gate:hold"),
    knob(GATE, "gate.release", "Release", Unit::Ms, (1.0, 5000.0), 145.0, "gate:rt"),
    knob(GATE, "gate.hyst_th", "Close below", Unit::Db, (-40.0, 0.0), -10.0, "gate:ht"),
    choice(GATE, "gate.hyst", "Hysteresis", 1.0, "gate:gh", OFF_ON),
    knob(COMPRESSOR, "comp.threshold", "Threshold", Unit::Db, (-60.0, 0.0), -18.0, "comp:al"),
    knob(COMPRESSOR, "comp.ratio", "Ratio", Unit::Ratio, (1.0, 20.0), 3.2, "comp:cr"),
    knob(COMPRESSOR, "comp.knee", "Knee", Unit::Db, (-24.0, 0.0), -6.0, "comp:kn"),
    knob(COMPRESSOR, "comp.attack", "Attack", Unit::Ms, (0.1, 200.0), 10.0, "comp:at"),
    knob(COMPRESSOR, "comp.release", "Release", Unit::Ms, (1.0, 2000.0), 100.0, "comp:rt"),
    knob(COMPRESSOR, "comp.makeup", "Makeup", Unit::Db, (0.0, 24.0), 5.0, "comp:mk"),
    knob(DEESSER, "ds.threshold", "Threshold", Unit::Db, (-60.0, 0.0), -22.0, "ds:threshold"),
    knob(DEESSER, "ds.ratio", "Ratio", Unit::Ratio, (1.0, 20.0), 3.0, "ds:ratio"),
    knob(DEESSER, "ds.peak", "Sibilance", Unit::Hz, (2000.0, 12000.0), 4800.0, "ds:f2_freq"),
    knob(DEESSER, "ds.split", "Split", Unit::Hz, (2000.0, 12000.0), 6000.0, "ds:f1_freq"),
    choice(DEESSER, "ds.mode", "Reduce", 0.0, "ds:mode", &[("Whole signal", 0.0), ("Highs only", 1.0)]),
    knob(LIMITER, "lim.threshold", "Threshold", Unit::Db, (-24.0, 0.0), -3.0, "lim:th"),
    knob(LIMITER, "lim.attack", "Attack", Unit::Ms, (0.25, 20.0), 5.0, "lim:at"),
    knob(LIMITER, "lim.release", "Release", Unit::Ms, (0.25, 20.0), 20.0, "lim:rt"),
    knob(LIMITER, "lim.lookahead", "Lookahead", Unit::Ms, (0.1, 20.0), 5.0, "lim:lk"),
    choice(LIMITER, "lim.boost", "Lift to 0 dB", 1.0, "lim:boost", OFF_ON),
];

/// Ports that never change at runtime.
const FIXED: &[(&str, f32)] = &[
    ("hp:ft", 1.0),   // hi-pass
    ("hp:fm", 3.0),   // BWC (MT)
    ("lim:alr", 0.0), // automatic level regulation off
    ("lim:ovs", 12.0),
];

/// (label, LSP `ft` value)
pub const BAND_KINDS: &[(&str, u8)] =
    &[("Off", 0), ("Bell", 1), ("Low shelf", 5), ("High shelf", 3)];
const BELL: u8 = 1;
const LOW_SHELF: u8 = 5;
const HIGH_SHELF: u8 = 3;

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Band {
    pub kind: u8,
    pub freq: f32,
    pub gain: f32,
    pub q: f32,
}

const fn bell(freq: f32, gain: f32) -> Band {
    Band {
        kind: BELL,
        freq,
        gain,
        q: 4.36,
    }
}

const OFF_BAND: Band = Band {
    kind: 0,
    freq: 1000.0,
    gain: 0.0,
    q: 1.0,
};

/// A small cut around 270 Hz and a lift across 3 to 5 kHz and near 9 kHz.
pub const PRESET_EQ: [Band; 8] = [
    bell(239.0, -2.05),
    bell(297.0, -1.9),
    bell(3192.0, 2.02),
    bell(3961.0, 2.93),
    bell(4915.0, 2.02),
    bell(9394.0, 1.39),
    OFF_BAND,
    OFF_BAND,
];

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub device: String,
    pub bypass: bool,
    pub input_gain: f32,
    pub on: [bool; 7],
    pub values: BTreeMap<String, f32>,
    pub eq: [Band; 8],
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            device: String::new(),
            bypass: false,
            input_gain: 0.0,
            on: [true; 7],
            values: BTreeMap::new(),
            eq: PRESET_EQ,
        }
    }
}

pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

pub fn param(key: &str) -> &'static Param {
    PARAMS
        .iter()
        .find(|p| p.key == key)
        .expect("known param key")
}

impl Settings {
    pub fn get(&self, key: &str) -> f32 {
        self.values.get(key).copied().unwrap_or(param(key).preset)
    }

    fn active(&self, stage: usize) -> bool {
        self.on[stage] && !self.bypass
    }

    pub fn ports(&self) -> BTreeMap<String, f32> {
        let mut ports: BTreeMap<String, f32> =
            FIXED.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        for p in PARAMS {
            let v = self.get(p.key);
            ports.insert(
                p.port.into(),
                if p.unit == Unit::Db { db_to_gain(v) } else { v },
            );
        }
        for (i, b) in self.eq.iter().enumerate() {
            ports.insert(format!("eq:ft_{i}"), b.kind as f32);
            ports.insert(format!("eq:f_{i}"), b.freq);
            ports.insert(format!("eq:g_{i}"), db_to_gain(b.gain));
            ports.insert(format!("eq:q_{i}"), b.q);
        }
        ports.insert("mix:Gain 1".into(), db_to_gain(self.input_gain));
        let enabled = |stage| if self.active(stage) { 1.0 } else { 0.0 };
        ports.insert("nr:Dry Mix".into(), 1.0 - enabled(NOISE));
        ports.insert("hp:enabled".into(), enabled(FILTER));
        ports.insert("gate:enabled".into(), enabled(GATE));
        ports.insert("comp:enabled".into(), enabled(COMPRESSOR));
        ports.insert("ds:bypass".into(), 1.0 - enabled(DEESSER));
        ports.insert("eq:enabled".into(), enabled(EQ));
        ports.insert("lim:enabled".into(), enabled(LIMITER));
        ports
    }

    pub fn load() -> Self {
        fs::read(settings_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = settings_path();
        fs::create_dir_all(path.parent().expect("settings file has a parent"))?;
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        fs::rename(tmp, path)
    }
}

fn settings_path() -> PathBuf {
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env::var_os("HOME").expect("HOME is set")).join(".config")
        });
    base.join("cardioid/settings.json")
}

#[rustfmt::skip]
const NODES: &[(&str, &str, &str)] = &[
    ("mix", "builtin", "label = mixer"),
    ("nr", "ladspa", "plugin = librnnoise_ladspa label = noise_suppressor_mono"),
    ("hp", "lv2", "plugin = \"http://lsp-plug.in/plugins/lv2/filter_mono\""),
    ("gate", "lv2", "plugin = \"http://lsp-plug.in/plugins/lv2/gate_mono\""),
    ("comp", "lv2", "plugin = \"http://lsp-plug.in/plugins/lv2/compressor_mono\""),
    ("ds", "lv2", "plugin = \"http://calf.sourceforge.net/plugins/Deesser\""),
    ("eq", "lv2", "plugin = \"http://lsp-plug.in/plugins/lv2/para_equalizer_x8_mono\""),
    ("lim", "lv2", "plugin = \"http://lsp-plug.in/plugins/lv2/limiter_mono\""),
];

const LINKS: &[(&str, &str)] = &[
    ("mix:Out", "nr:Input"),
    ("nr:Output", "hp:in"),
    ("hp:out", "gate:in"),
    ("gate:out", "comp:in"),
    ("comp:out", "ds:in_l"),
    ("ds:out_l", "eq:in"),
    ("eq:out", "lim:in"),
];

/// Config for `pipewire -c`. The capture side is pinned to one device:
/// `dont-fallback` + `linger` make it wait silently when the mic is gone, so
/// it can never pick up another source or a desktop monitor. `passive` keeps
/// the mic closed while no app is listening.
pub fn conf(s: &Settings) -> String {
    let ports = s.ports();
    let nodes: String = NODES
        .iter()
        .map(|(name, kind, plugin)| {
            let prefix = format!("{name}:");
            let control: String = ports
                .iter()
                .filter_map(|(k, v)| k.strip_prefix(&prefix).map(|k| format!("\"{k}\" = {v} ")))
                .collect();
            format!("{{ type = {kind} name = {name} {plugin} control = {{ {control}}} }}\n")
        })
        .collect();
    let links: String = LINKS
        .iter()
        .map(|(o, i)| format!("{{ output = \"{o}\" input = \"{i}\" }}\n"))
        .collect();
    let target = serde_json::to_string(&s.device).expect("string serializes");
    format!(
        r#"context.properties = {{ log.level = 2 }}
context.spa-libs = {{ audio.convert.* = audioconvert/libspa-audioconvert support.* = support/libspa-support }}
context.modules = [
{{ name = libpipewire-module-rt flags = [ ifexists nofail ] }}
{{ name = libpipewire-module-protocol-native }}
{{ name = libpipewire-module-client-node }}
{{ name = libpipewire-module-adapter }}
{{ name = libpipewire-module-filter-chain args = {{
  node.description = "Cardioid"
  media.name = "Cardioid"
  audio.rate = 48000
  filter.graph = {{
    nodes = [
{nodes}    ]
    links = [
{links}    ]
    inputs = [ "mix:In 1" ]
    outputs = [ "lim:out" ]
  }}
  capture.props = {{
    node.name = "{CAPTURE}"
    node.passive = true
    audio.position = [ MONO ]
    target.object = {target}
    node.dont-fallback = true
    node.linger = true
  }}
  playback.props = {{
    node.name = "{NODE}"
    media.class = Audio/Source
    audio.position = [ MONO ]
  }}
}} }}
]
"#
    )
}

fn biquad_db(b: &Band, f: f32) -> f32 {
    let a = 10f64.powf(b.gain as f64 / 40.0);
    let w0 = std::f64::consts::TAU * b.freq as f64 / SAMPLE_RATE as f64;
    let (sin, cos) = w0.sin_cos();
    let alpha = sin / (2.0 * b.q.max(0.1) as f64);
    let root = 2.0 * a.sqrt() * alpha;
    let (b0, b1, b2, a0, a1, a2) = match b.kind {
        BELL => (
            1.0 + alpha * a,
            -2.0 * cos,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cos,
            1.0 - alpha / a,
        ),
        LOW_SHELF => (
            a * ((a + 1.0) - (a - 1.0) * cos + root),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
            a * ((a + 1.0) - (a - 1.0) * cos - root),
            (a + 1.0) + (a - 1.0) * cos + root,
            -2.0 * ((a - 1.0) + (a + 1.0) * cos),
            (a + 1.0) + (a - 1.0) * cos - root,
        ),
        HIGH_SHELF => (
            a * ((a + 1.0) + (a - 1.0) * cos + root),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
            a * ((a + 1.0) + (a - 1.0) * cos - root),
            (a + 1.0) - (a - 1.0) * cos + root,
            2.0 * ((a - 1.0) - (a + 1.0) * cos),
            (a + 1.0) - (a - 1.0) * cos - root,
        ),
        _ => return 0.0,
    };
    let w = std::f64::consts::TAU * f as f64 / SAMPLE_RATE as f64;
    let (s1, c1) = w.sin_cos();
    let (s2, c2) = (2.0 * w).sin_cos();
    let mag =
        |x0: f64, x1: f64, x2: f64| (x0 + x1 * c1 + x2 * c2).powi(2) + (x1 * s1 + x2 * s2).powi(2);
    (10.0 * (mag(b0, b1, b2) / mag(a0, a1, a2)).log10()) as f32
}

/// Display approximation of the LSP filters (RBJ biquads).
pub fn eq_db(bands: &[Band], f: f32) -> f32 {
    bands.iter().map(|b| biquad_db(b, f)).sum()
}

/// Butterworth hi-pass magnitude. Slope index 0 is 12 dB/oct.
pub fn highpass_db(cutoff: f32, slope: f32, f: f32) -> f32 {
    let order = 2.0 * (slope + 1.0);
    -10.0 * (1.0 + (cutoff / f).powf(2.0 * order)).log10()
}

pub fn gate_out(input: f32, threshold: f32, reduction: f32) -> f32 {
    if input >= threshold {
        input
    } else {
        input + reduction
    }
}

/// Soft-knee downward compressor curve, makeup included.
pub fn compressor_out(input: f32, threshold: f32, ratio: f32, knee: f32, makeup: f32) -> f32 {
    let width = -knee * 2.0;
    let over = input - threshold;
    let reduced = if over <= -width / 2.0 {
        input
    } else if over >= width / 2.0 {
        threshold + over / ratio
    } else {
        input + (1.0 / ratio - 1.0) * (over + width / 2.0).powi(2) / (2.0 * width)
    };
    reduced + makeup
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32, tol: f32) {
        assert!((a - b).abs() < tol, "{a} != {b}");
    }

    #[test]
    fn preset_maps_to_plugin_units() {
        let p = Settings::default().ports();
        close(p["gate:gt"], 0.00316, 1e-4);
        close(p["gate:gr"], 0.01, 1e-4);
        close(p["comp:mk"], 1.778, 1e-3);
        close(p["comp:cr"], 3.2, 1e-6);
        close(p["lim:th"], 0.708, 1e-3);
        close(p["ds:threshold"], 0.0794, 1e-3);
        assert_eq!(p["hp:ft"], 1.0);
    }

    #[test]
    fn every_preset_is_within_its_range() {
        for p in PARAMS.iter().filter(|p| p.unit != Unit::Choice) {
            assert!(p.min <= p.preset && p.preset <= p.max, "{}", p.key);
        }
    }

    #[test]
    fn bypass_disables_every_stage_but_keeps_input_gain() {
        let s = Settings {
            bypass: true,
            input_gain: 6.0,
            ..Default::default()
        };
        let p = s.ports();
        for k in [
            "hp:enabled",
            "gate:enabled",
            "comp:enabled",
            "eq:enabled",
            "lim:enabled",
        ] {
            assert_eq!(p[k], 0.0, "{k}");
        }
        assert_eq!(p["ds:bypass"], 1.0);
        assert_eq!(p["nr:Dry Mix"], 1.0);
        close(p["mix:Gain 1"], 1.995, 1e-3);
    }

    #[test]
    fn single_stage_toggle_only_touches_that_stage() {
        let mut s = Settings::default();
        s.on[DEESSER] = false;
        let p = s.ports();
        assert_eq!(p["ds:bypass"], 1.0);
        assert_eq!(p["gate:enabled"], 1.0);
        assert_eq!(p["nr:Dry Mix"], 0.0);
    }

    #[test]
    fn conf_pins_capture_to_the_mic_only() {
        let s = Settings {
            device: "alsa_input.a\"b".into(),
            ..Default::default()
        };
        let c = conf(&s);
        assert!(c.contains(r#"target.object = "alsa_input.a\"b""#));
        assert!(c.contains("node.dont-fallback = true"));
        assert!(c.contains("node.linger = true"));
        assert!(c.contains("node.passive = true"));
        assert!(!c.contains("stream.capture.sink"));
        assert!(c.contains(r#""VAD Threshold (%)" = 65"#));
    }

    #[test]
    fn saved_settings_without_new_fields_still_load() {
        let s: Settings = serde_json::from_str(r#"{"device":"mic"}"#).unwrap();
        assert_eq!(s.device, "mic");
        assert_eq!(s.eq, PRESET_EQ);
        assert_eq!(s.get("comp.ratio"), 3.2);
    }

    #[test]
    fn eq_curve_hits_band_gain_at_center() {
        let bands = [bell(1000.0, 6.0)];
        close(eq_db(&bands, 1000.0), 6.0, 0.01);
        close(eq_db(&bands, 100.0), 0.0, 0.1);
        let shelf = [Band {
            kind: HIGH_SHELF,
            freq: 2000.0,
            gain: 6.0,
            q: 0.7,
        }];
        close(eq_db(&shelf, 18000.0), 6.0, 0.3);
        close(eq_db(&shelf, 100.0), 0.0, 0.1);
        close(eq_db(&[OFF_BAND], 1000.0), 0.0, 1e-6);
    }

    #[test]
    fn highpass_is_3db_down_at_cutoff() {
        close(highpass_db(90.0, 1.0, 90.0), -3.01, 0.02);
        assert!(highpass_db(90.0, 1.0, 45.0) < -20.0);
    }

    #[test]
    fn dynamics_curves() {
        assert_eq!(gate_out(-30.0, -50.0, -40.0), -30.0);
        assert_eq!(gate_out(-60.0, -50.0, -40.0), -100.0);
        close(compressor_out(-40.0, -20.0, 4.0, -3.0, 0.0), -40.0, 1e-4);
        close(compressor_out(0.0, -20.0, 4.0, -3.0, 0.0), -15.0, 1e-4);
        close(compressor_out(-40.0, -20.0, 4.0, -3.0, 5.0), -35.0, 1e-4);
    }
}
