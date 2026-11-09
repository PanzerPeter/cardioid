//! Thin wrappers over the PipeWire command line tools.

use crate::chain::CAPTURE;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

pub fn runtime_dir() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("cardioid")
}

/// Spawns a child that dies with the thread that spawned it.
pub fn spawn_tied(cmd: &mut Command) -> io::Result<Child> {
    // SAFETY: prctl is async-signal-safe and touches no shared state.
    unsafe {
        cmd.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        });
    }
    cmd.spawn()
}

#[derive(Clone, Default)]
pub struct Graph {
    /// (node name, description) of real capture devices.
    pub mics: Vec<(String, String)>,
    pub capture_id: Option<u32>,
}

/// Only hardware nodes qualify, so a virtual or monitor source can't be picked.
fn parse_graph(dump: &[u8]) -> Graph {
    let mut graph = Graph::default();
    let objects: Vec<Value> = serde_json::from_slice(dump).unwrap_or_default();
    for object in &objects {
        let props = &object["info"]["props"];
        let name = props["node.name"].as_str().unwrap_or_default();
        if name == CAPTURE {
            graph.capture_id = object["id"].as_u64().map(|id| id as u32);
        }
        if props["media.class"] == "Audio/Source" && props.get("device.id").is_some() {
            let description = props["node.description"].as_str().unwrap_or(name);
            graph.mics.push((name.into(), description.into()));
        }
    }
    graph.mics.sort_by(|a, b| a.1.cmp(&b.1));
    graph
}

pub fn graph() -> Graph {
    Command::new("pw-dump")
        .output()
        .map(|out| parse_graph(&out.stdout))
        .unwrap_or_default()
}

/// The default source, but only if it is a real mic. A virtual source (another
/// filter app, a sink monitor, Cardioid itself) must never become the input.
pub fn default_mic() -> Option<String> {
    let out = Command::new("pactl")
        .arg("get-default-source")
        .output()
        .ok()?;
    let name = String::from_utf8(out.stdout).ok()?.trim().to_string();
    graph()
        .mics
        .iter()
        .any(|(mic, _)| *mic == name)
        .then_some(name)
}

pub fn set_ports(ports: &BTreeMap<String, f32>) {
    let params: String = ports.iter().map(|(k, v)| format!("\"{k}\" {v} ")).collect();
    let pod = format!("{{ params = [ {params}] }}");
    if let Err(e) = Command::new("pw-cli")
        .args(["s", CAPTURE, "Props", &pod])
        .stdout(Stdio::null())
        .status()
    {
        eprintln!("pw-cli: {e}");
    }
}

/// Moves the capture side live. The value must be the bare node name: anything
/// that doesn't resolve leaves the chain waiting with no input.
pub fn set_device(name: &str) {
    let Some(id) = graph().capture_id else { return };
    if let Err(e) = Command::new("pw-metadata")
        .args([&id.to_string(), "target.object", name])
        .stdout(Stdio::null())
        .status()
    {
        eprintln!("pw-metadata: {e}");
    }
}

/// Same pinning as the filters: a missing target waits instead of falling back.
const PINNED: &str = "{ node.dont-fallback = true node.linger = true }";

/// Records one node into a WAV file until `stop_recording`.
pub fn record(target: &str, path: &Path) -> io::Result<Child> {
    spawn_tied(
        Command::new("pw-record")
            .args(["--rate", "48000", "--channels", "1", "--format", "s16"])
            .args(["--target", target, "-P", PINNED])
            .arg(path)
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
}

/// SIGTERM, not kill: pw-record has to close the file to finish the WAV header.
pub fn stop_recording(mut child: Child) {
    // SAFETY: plain syscall on a child we still own and have not reaped.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let _ = child.wait();
}

/// Plays a file on the default output. It never reaches the virtual mic.
pub fn play(path: &Path) -> io::Result<Child> {
    spawn_tied(
        Command::new("pw-play")
            .arg(path)
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
}

/// Records one node and tracks its peak until dropped.
pub struct Meter {
    child: Child,
    peak: Arc<AtomicU32>,
}

impl Meter {
    pub fn start(target: &str) -> io::Result<Self> {
        let mut child = spawn_tied(
            Command::new("pw-record")
                .args([
                    "--raw",
                    "--format",
                    "f32",
                    "--rate",
                    "48000",
                    "--channels",
                    "1",
                ])
                .args(["--latency", "20ms", "--target", target])
                .args(["-P", PINNED, "-"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )?;
        let peak = Arc::new(AtomicU32::new(0));
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let shared = peak.clone();
        std::thread::spawn(move || {
            // read_exact keeps every read on a sample boundary; a short read
            // would shift all later samples and show garbage peaks.
            let mut buf = [0u8; 2048];
            while stdout.read_exact(&mut buf).is_ok() {
                let block = buf
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_ne_bytes(*b).abs())
                    .fold(0.0, f32::max);
                // Non-negative floats order the same as their bits.
                shared.fetch_max(block.to_bits(), Ordering::Relaxed);
            }
        });
        Ok(Self { child, peak })
    }

    /// Peak since the last call, linear.
    pub fn take(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Ordering::Relaxed))
    }

    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_hardware_sources_count_as_mics() {
        let dump = br#"[
          {"id":1,"info":{"props":{"node.name":"alsa_input.mic","node.description":"Mic","media.class":"Audio/Source","device.id":5}}},
          {"id":2,"info":{"props":{"node.name":"rnnoise_source","media.class":"Audio/Source"}}},
          {"id":3,"info":{"props":{"node.name":"cardioid","media.class":"Audio/Source"}}},
          {"id":4,"info":{"props":{"node.name":"alsa_output.x","media.class":"Audio/Sink","device.id":6}}},
          {"id":7,"info":{"props":{"node.name":"cardioid_capture","media.class":"Stream/Input/Audio"}}},
          {"id":8,"type":"PipeWire:Interface:Link","info":{}}
        ]"#;
        let g = parse_graph(dump);
        assert_eq!(
            g.mics,
            vec![("alsa_input.mic".to_string(), "Mic".to_string())]
        );
        assert_eq!(g.capture_id, Some(7));
    }

    /// Needs the service running: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_changes_reach_the_filters_and_meters_read_a_mic() {
        let id = graph().capture_id.expect("service is running").to_string();
        let props = || {
            let out = Command::new("pw-cli")
                .args(["e", &id, "Props"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).replace(',', ".")
        };
        let saved = crate::chain::Settings::load().ports()["comp:cr"];
        set_ports(&BTreeMap::from([("comp:cr".to_string(), 7.25)]));
        assert!(props().contains("7.25"));
        set_ports(&BTreeMap::from([("comp:cr".to_string(), saved)]));

        let meters: Vec<Meter> = graph()
            .mics
            .iter()
            .map(|(name, _)| Meter::start(name).unwrap())
            .collect();
        std::thread::sleep(std::time::Duration::from_millis(1500));
        assert!(
            meters.iter().any(|m| m.take() > 0.0),
            "no mic produced signal"
        );
    }

    /// A stopped recording must have a finished WAV header, or players read 0 samples.
    #[test]
    #[ignore]
    fn stopped_recording_is_a_playable_wav() {
        let (name, _) = graph().mics.into_iter().next().expect("a mic is connected");
        let path = std::env::temp_dir().join("cardioid-test.wav");
        let child = record(&name, &path).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1200));
        stop_recording(child);
        let wav = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let data = wav
            .windows(4)
            .position(|w| w == b"data")
            .expect("data chunk")
            + 4;
        let declared = u32::from_le_bytes(wav[data..data + 4].try_into().unwrap());
        assert!(declared > 48000, "only {declared} bytes declared");
    }

    #[test]
    fn broken_dump_is_an_empty_graph() {
        assert!(parse_graph(b"not json").mics.is_empty());
    }
}
