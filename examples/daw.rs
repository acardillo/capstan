//! Example: CLI-based DAW (MVP) — tracks, input devices, per-track and master gain.
//! Sticky header at top lists tracks and draws live level meters (ASCII).
//!
//! Run with: `cargo run --example daw`
//!
//! Structure:
//! - Types: CLI, track/source, OpenInputs, Session, StatusKind
//! - Graph: build_session_graph, send_graph
//! - Paths: recording_path, expand_tilde
//! - UI: draw_header, draw_history, meter helpers
//! - Commands: parse_track_no, handle_command
//! - Recording: stop_recording_and_save
//! - Main: event loop

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use capstan::command::{command_channel, Command};
use capstan::device::{input_device_list, open_input_stream, DeviceError};
use capstan::event::event_channel;
use capstan::file_feeder::load_wav_at_rate;
use capstan::graph::{AudioGraph, CompiledGraph, GraphNode};
use capstan::input_buffer::{FilePlaybackBuffer, InputSampleBuffer, SampleSource};
use capstan::meter::MeterBuffer;
use capstan::nodes::{
    Echo, GainProcessor, InputNode, Mixer, Overdrive, RecordNode, SineGenerator, Tremolo,
};
use capstan::record::{write_wav, RecordBuffer};
use capstan::run_audio;
use clap::Parser;
use crossterm::cursor::{MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

const DEFAULT_FRAME_COUNT: usize = 512;
const METER_WIDTH: usize = 24;
const HEADER_REDRAW_MS: u64 = 80;
const METER_DB_MIN: f32 = -60.0;
const HISTORY_LINES: usize = 6;
const COMMAND_HISTORY_CAP: usize = 50;
const SUCCESS_PREFIX: &str = "  ✓ ";
const WARNING_PREFIX: &str = "\u{200B}  ";
const ERROR_PREFIX: &str = "  ✗ ";

const HELP_MSG: &str = "track create | track delete <no> | input <tn> ... | gain [tn] <lvl> | echo <tn> <ms>|none | tremolo <tn> <rate> <depth>|none | overdrive <tn> <0-5>|none | record | quit";

// -----------------------------------------------------------------------------
// Types
// -----------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(name = "daw")]
#[command(about = "Capstan DAW (MVP) — tracks, input devices, gain.")]
struct Cli {
    #[arg(long, default_value = "1024")]
    channel_capacity: usize,
}

/// Source for a track: none, device input, sine tone, or file playback.
enum TrackSource {
    None,
    Device(usize),
    Sine {
        freq_hz: f32,
    },
    File {
        path: PathBuf,
        buffer: Arc<dyn SampleSource + Send + Sync>,
    },
}

struct Track {
    source: TrackSource,
    gain: f32,
    delay_ms: Option<f32>,
    tremolo: Option<(f32, f32)>,
    overdrive: Option<f32>,
}

/// All mutable session state that commands and the graph use.
struct Session {
    tracks: Vec<Track>,
    master_gain: f32,
    open_inputs: OpenInputs,
    meter_buffer: Option<Arc<MeterBuffer>>,
    output_sample_rate: u32,
    recording: bool,
    record_buffer: Option<Arc<RecordBuffer>>,
    record_output_path: Option<PathBuf>,
}

#[derive(Clone, Copy)]
enum StatusKind {
    Success,
    Warning,
    Error,
    Neutral,
}

/// Result of handling one command: what to show and whether to quit.
struct CommandOutcome {
    status_kind: StatusKind,
    status_msg: String,
    quit: bool,
}

/// Open input streams: keep streams alive and map device index -> buffer.
struct OpenInputs {
    _streams: Vec<cpal::Stream>,
    device_to_buffer: HashMap<usize, Arc<dyn SampleSource + Send + Sync>>,
}

impl OpenInputs {
    fn new() -> Self {
        OpenInputs {
            _streams: Vec::new(),
            device_to_buffer: HashMap::new(),
        }
    }

    fn ensure_device(
        &mut self,
        host: &cpal::Host,
        device_index: usize,
    ) -> Result<Arc<dyn SampleSource + Send + Sync>, DeviceError> {
        if let Some(buf) = self.device_to_buffer.get(&device_index) {
            return Ok(Arc::clone(buf));
        }
        let buffer: Arc<InputSampleBuffer> = Arc::new(InputSampleBuffer::new(2048));
        let stream = open_input_stream(host, device_index, Arc::clone(&buffer))?;
        self._streams.push(stream);
        let buffer_dyn: Arc<dyn SampleSource + Send + Sync> = buffer;
        self.device_to_buffer
            .insert(device_index, Arc::clone(&buffer_dyn));
        Ok(buffer_dyn)
    }

    fn get_buffer(&self, device_index: usize) -> Option<Arc<dyn SampleSource + Send + Sync>> {
        self.device_to_buffer.get(&device_index).cloned()
    }
}

// -----------------------------------------------------------------------------
// Graph
// -----------------------------------------------------------------------------

fn build_session_graph(
    session: &Session,
    silent_buffer: &Arc<dyn SampleSource + Send + Sync>,
    record_buffer: Option<Arc<RecordBuffer>>,
) -> Option<CompiledGraph> {
    use std::iter::once;
    let tracks = &session.tracks;
    let open_inputs = &session.open_inputs;
    let master_gain = session.master_gain;
    let meter_buffer = &session.meter_buffer;
    let sample_rate = session.output_sample_rate;

    let mut g = AudioGraph::new();
    if tracks.is_empty() {
        let inp = g.add_node(GraphNode::Input(InputNode::new(Arc::clone(silent_buffer))));
        let out = g.add_node(GraphNode::Gain(GainProcessor::new(master_gain)));
        g.add_edge(inp, out);
        let g = if let Some(rb) = record_buffer {
            let rec = g.add_node(GraphNode::Record(RecordNode::new(rb)));
            g.add_edge(capstan::graph::NodeId::new(1), rec);
            g
        } else {
            g
        };
        return match &meter_buffer {
            Some(mb) if mb.len() == 1 => g
                .compile_with_meter(DEFAULT_FRAME_COUNT, Some((vec![1], Arc::clone(mb))))
                .ok(),
            _ => g.compile(DEFAULT_FRAME_COUNT).ok(),
        };
    }

    const MAX_DELAY_MS: f32 = 2000.0;
    let mut gain_node_ids = Vec::with_capacity(tracks.len());
    for track in tracks {
        let source_node = match &track.source {
            TrackSource::None => {
                g.add_node(GraphNode::Input(InputNode::new(Arc::clone(silent_buffer))))
            }
            TrackSource::Device(d) => {
                let buf = open_inputs
                    .get_buffer(*d)
                    .unwrap_or_else(|| Arc::clone(silent_buffer));
                g.add_node(GraphNode::Input(InputNode::new(buf)))
            }
            TrackSource::Sine { freq_hz } => {
                g.add_node(GraphNode::Sine(SineGenerator::new(*freq_hz, sample_rate)))
            }
            TrackSource::File { buffer, .. } => {
                g.add_node(GraphNode::Input(InputNode::new(Arc::clone(buffer))))
            }
        };
        let mut head = source_node;
        if let Some(ms) = track.delay_ms {
            let mut echo = Echo::new(MAX_DELAY_MS, sample_rate);
            echo.set_delay_ms(ms);
            let echo_node = g.add_node(GraphNode::Echo(echo));
            g.add_edge(head, echo_node);
            head = echo_node;
        }
        if let Some((rate_hz, depth)) = track.tremolo {
            let mut tremolo = Tremolo::new(rate_hz, sample_rate);
            tremolo.depth = depth.clamp(0.0, 1.0);
            let trem_node = g.add_node(GraphNode::Tremolo(tremolo));
            g.add_edge(head, trem_node);
            head = trem_node;
        }
        if let Some(drive) = track.overdrive {
            let ovd_node = g.add_node(GraphNode::Overdrive(Overdrive::new(drive)));
            g.add_edge(head, ovd_node);
            head = ovd_node;
        }
        let gain = g.add_node(GraphNode::Gain(GainProcessor::new(track.gain)));
        g.add_edge(head, gain);
        gain_node_ids.push(gain);
    }

    let num_echo = tracks.iter().filter(|t| t.delay_ms.is_some()).count();
    let num_tremolo = tracks.iter().filter(|t| t.tremolo.is_some()).count();
    let num_overdrive = tracks.iter().filter(|t| t.overdrive.is_some()).count();
    let n = tracks.len();
    let gains: Vec<f32> = tracks.iter().map(|t| t.gain).collect();
    let mix = g.add_node(GraphNode::Mixer(Mixer::new(gains)));
    for &gid in &gain_node_ids {
        g.add_edge(gid, mix);
    }
    let master = g.add_node(GraphNode::Gain(GainProcessor::new(master_gain)));
    g.add_edge(mix, master);

    let g = if let Some(rb) = record_buffer {
        let master_id =
            capstan::graph::NodeId::new(n + num_echo + num_tremolo + num_overdrive + n + 1);
        let rec = g.add_node(GraphNode::Record(RecordNode::new(rb)));
        g.add_edge(master_id, rec);
        g
    } else {
        g
    };

    // Node order: sources 0..n, echo, tremolo, overdrive, gains, mix, master [, record].
    match &meter_buffer {
        Some(mb) if mb.len() == n + 1 => {
            let base = n + num_echo + num_tremolo + num_overdrive;
            let tap_indices: Vec<usize> =
                (0..n).map(|i| base + i).chain(once(base + n + 1)).collect();
            g.compile_with_meter(DEFAULT_FRAME_COUNT, Some((tap_indices, Arc::clone(mb))))
                .ok()
        }
        _ => g.compile(DEFAULT_FRAME_COUNT).ok(),
    }
}

// -----------------------------------------------------------------------------
// Paths
// -----------------------------------------------------------------------------

fn recording_path() -> PathBuf {
    let desktop = expand_tilde("~/Desktop");
    let filename = format!(
        "Recording_{}.wav",
        chrono::Local::now().format("%Y-%m-%d_%H-%M-%S")
    );
    desktop.join(filename)
}

fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" || path.starts_with("~/") {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".to_string());
        let home = PathBuf::from(home);
        if path == "~" {
            home
        } else {
            home.join(path.trim_start_matches('~').trim_start_matches('/'))
        }
    } else {
        PathBuf::from(path)
    }
}

// -----------------------------------------------------------------------------
// Display helpers
// -----------------------------------------------------------------------------

/// Parses 1-based track number; returns error message if out of range.
fn parse_track_no(s: &str, num_tracks: usize) -> Result<usize, String> {
    let n: usize = s.parse().map_err(|_| "Invalid track number.".to_string())?;
    if (1..=num_tracks.max(1)).contains(&n) {
        Ok(n)
    } else {
        Err(format!("Track number must be 1–{}.", num_tracks.max(1)))
    }
}

fn source_display(source: &TrackSource) -> String {
    match source {
        TrackSource::None => "-".to_string(),
        TrackSource::Device(d) => format!("dev {}", d),
        TrackSource::Sine { freq_hz } => format!("{} Hz", freq_hz),
        TrackSource::File { path, .. } => path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string(),
    }
}

fn send_graph(cmd_tx: &capstan::command::CommandSender, compiled: CompiledGraph) {
    let _ = cmd_tx.try_send(Command::SwapGraph(compiled));
}

// -----------------------------------------------------------------------------
// UI
// -----------------------------------------------------------------------------

fn peak_to_db(peak: f32) -> f32 {
    if peak <= 1e-10 {
        METER_DB_MIN
    } else {
        (20.0 * peak.log10()).max(METER_DB_MIN)
    }
}

/// Draw an ASCII level bar scaled in dB (more sensitive to low levels) and append the dB value.
fn ascii_meter_with_db(peak: f32) -> String {
    let db = peak_to_db(peak);
    let normalized = (db - METER_DB_MIN) / (-METER_DB_MIN);
    let fill = (normalized * (METER_WIDTH as f32)).round() as usize;
    let fill = fill.min(METER_WIDTH);
    let bar: String = (0..METER_WIDTH)
        .map(|i| if i < fill { '#' } else { '-' })
        .collect();
    format!("[{}] {:>6.1} dB", bar, db)
}

/// Prompt row is directly under the tracks: table header + track rows + master row.
fn prompt_row(tracks: &[Track]) -> u16 {
    (2 + tracks.len()).min(200) as u16
}

fn draw_header(
    stdout: &mut impl Write,
    tracks: &[Track],
    peaks: &[f32],
    master_gain: f32,
    prompt_row: u16,
) -> std::io::Result<()> {
    let mut line = 0u16;
    execute!(stdout, MoveTo(0, 0), Clear(ClearType::CurrentLine))?;
    writeln!(
        stdout,
        " track | source     | gain  | echo    | tremolo  | overdrive | level"
    )?;
    line += 1;
    for (i, track) in tracks.iter().enumerate() {
        let src = source_display(&track.source);
        let peak = peaks.get(i).copied().unwrap_or(0.0);
        let echo_display = track
            .delay_ms
            .map(|ms| format!("{:.0} ms", ms))
            .unwrap_or_else(|| "-".to_string());
        let trem_display = track
            .tremolo
            .map(|(r, d)| format!("{:.1}/{:.2}", r, d))
            .unwrap_or_else(|| "-".to_string());
        let ovd_display = track
            .overdrive
            .map(|d| format!("{:.1}", d))
            .unwrap_or_else(|| "-".to_string());
        execute!(stdout, MoveTo(0, line), Clear(ClearType::CurrentLine))?;
        writeln!(
            stdout,
            "   {}   | {:>10} | {:.2}  | {:>7} | {:>8} | {:>9} | {}",
            i + 1,
            src,
            track.gain,
            echo_display,
            trem_display,
            ovd_display,
            ascii_meter_with_db(peak)
        )?;
        line += 1;
    }
    // Master row: always show. peaks has one extra slot for master (last element).
    let master_peak = peaks.last().copied().unwrap_or(0.0);
    execute!(stdout, MoveTo(0, line), Clear(ClearType::CurrentLine))?;
    writeln!(
        stdout,
        " master| {:>10} | {:.2}  | {:>7} | {:>8} | {:>9} | {}",
        "(mix)",
        master_gain,
        "-",
        "-",
        "-",
        ascii_meter_with_db(master_peak)
    )?;
    line += 1;
    for y in line..prompt_row {
        execute!(stdout, MoveTo(0, y), Clear(ClearType::CurrentLine))?;
    }
    Ok(())
}

fn draw_history(
    stdout: &mut impl Write,
    history: &[String],
    start_row: u16,
    max_lines: usize,
) -> std::io::Result<()> {
    let take = history.len().saturating_sub(max_lines);
    let lines: Vec<_> = history[take..].iter().rev().collect();
    for (i, s) in lines.iter().enumerate() {
        let row = start_row + i as u16;
        let color = if s.starts_with(SUCCESS_PREFIX) {
            Color::Green
        } else if s.starts_with(WARNING_PREFIX) {
            Color::Yellow
        } else if s.starts_with(ERROR_PREFIX) {
            Color::Red
        } else {
            Color::DarkGrey
        };
        execute!(
            stdout,
            MoveTo(0, row),
            Clear(ClearType::CurrentLine),
            SetForegroundColor(color)
        )?;
        let truncate = 200;
        // Strip zero-width space from warning lines so they display as "  msg" (U+200B is 3 bytes in UTF-8)
        let visible: String = if s.starts_with(WARNING_PREFIX) {
            s.chars().skip(1).collect()
        } else {
            (*s).clone()
        };
        let display = if visible.len() > truncate {
            format!("{}...", &visible[..truncate])
        } else {
            visible
        };
        writeln!(stdout, "{}", display)?;
        execute!(stdout, ResetColor)?;
    }
    for i in lines.len()..max_lines {
        execute!(
            stdout,
            MoveTo(0, start_row + i as u16),
            Clear(ClearType::CurrentLine)
        )?;
    }
    Ok(())
}

// -----------------------------------------------------------------------------
// Command handling
// -----------------------------------------------------------------------------

fn handle_command(
    session: &mut Session,
    parts: &[&str],
    host: &cpal::Host,
    cmd_tx: &capstan::command::CommandSender,
    silent_buffer: &Arc<dyn SampleSource + Send + Sync>,
) -> CommandOutcome {
    let mut session_changed = false;
    let mut status_kind = StatusKind::Neutral;
    let mut status_msg = String::new();
    let mut quit = false;
    let n = session.tracks.len();

    match parts {
        ["record"] => {
            if session.recording {
                status_kind = StatusKind::Warning;
                status_msg = "Already recording. Press Enter to stop.".to_string();
            } else {
                let path = recording_path();
                session.record_buffer = Some(Arc::new(RecordBuffer::new()));
                session.record_buffer.as_ref().unwrap().set_armed(true);
                if let Some(compiled) =
                    build_session_graph(session, silent_buffer, session.record_buffer.clone())
                {
                    send_graph(cmd_tx, compiled);
                    session.record_output_path = Some(path);
                    session.recording = true;
                    status_kind = StatusKind::Success;
                    status_msg = "Recording... Press Enter to stop.".to_string();
                } else {
                    session.record_buffer.as_ref().unwrap().set_armed(false);
                    session.record_buffer = None;
                    status_kind = StatusKind::Error;
                    status_msg = "Failed to compile graph for recording.".to_string();
                }
            }
        }
        ["quit" | "q"] => {
            quit = true;
        }
        ["help" | "h" | "?"] => {
            status_msg = HELP_MSG.to_string();
        }
        ["track", "create"] => {
            session.tracks.push(Track {
                source: TrackSource::None,
                gain: 0.7,
                delay_ms: None,
                tremolo: None,
                overdrive: None,
            });
            session_changed = true;
            status_kind = StatusKind::Success;
            status_msg = format!("Created track {}.", session.tracks.len());
        }
        ["track", "delete", no] => match parse_track_no(no, n) {
            Ok(tn) => {
                session.tracks.remove(tn - 1);
                session_changed = true;
                status_kind = StatusKind::Success;
                status_msg = format!("Deleted track {}.", tn);
            }
            Err(e) => {
                status_kind = StatusKind::Warning;
                status_msg = e;
            }
        },
        ["input", "--list" | "-l"] => match input_device_list(host) {
            Ok(devices) => {
                status_kind = StatusKind::Success;
                status_msg = if devices.is_empty() {
                    "(no input devices)".to_string()
                } else {
                    let s: String = devices
                        .iter()
                        .map(|d| format!("{}: {}", d.index, d.name))
                        .collect::<Vec<_>>()
                        .join("  |  ");
                    if s.len() > 120 {
                        format!("{}...", &s[..117])
                    } else {
                        s
                    }
                };
            }
            Err(e) => {
                status_kind = StatusKind::Error;
                status_msg = format!("List devices: {}", e);
            }
        },
        ["input", track_no, "--device", dev] => {
            if let (Ok(tn), Ok(d)) = (parse_track_no(track_no, n), dev.parse::<usize>()) {
                match session.open_inputs.ensure_device(host, d) {
                    Ok(_) => {
                        session.tracks[tn - 1].source = TrackSource::Device(d);
                        session_changed = true;
                        status_kind = StatusKind::Success;
                        status_msg = format!("Track {} → device {}.", tn, d);
                    }
                    Err(e) => {
                        status_kind = StatusKind::Error;
                        status_msg = format!("Failed to open device {}: {}", d, e);
                    }
                }
            } else {
                status_kind = StatusKind::Warning;
                status_msg = "Usage: input <track_no> --device <index>".to_string();
            }
        }
        ["input", track_no, "--sine", freq] => {
            if let (Ok(tn), Ok(f)) = (parse_track_no(track_no, n), freq.parse::<f32>()) {
                if (0.0..=20_000.0).contains(&f) {
                    session.tracks[tn - 1].source = TrackSource::Sine { freq_hz: f };
                    session_changed = true;
                    status_kind = StatusKind::Success;
                    status_msg = format!("Track {} → sine {} Hz.", tn, f);
                } else {
                    status_kind = StatusKind::Warning;
                    status_msg = "Frequency must be 0–20000 Hz.".to_string();
                }
            } else {
                status_kind = StatusKind::Warning;
                status_msg = parse_track_no(track_no, n)
                    .err()
                    .unwrap_or_else(|| "Usage: input <track_no> --sine <freq_hz>".to_string());
            }
        }
        _ if parts.len() >= 4 && parts[0] == "input" && parts[2] == "--file" => {
            let path_str = parts[3..].join(" ");
            if let Ok(tn) = parse_track_no(parts[1], n) {
                let path = PathBuf::from(&path_str);
                match load_wav_at_rate(&path, session.output_sample_rate) {
                    Ok(samples) => {
                        let buffer: Arc<dyn SampleSource + Send + Sync> =
                            Arc::new(FilePlaybackBuffer::new(Arc::new(samples)));
                        session.tracks[tn - 1].source = TrackSource::File { path, buffer };
                        session_changed = true;
                        status_kind = StatusKind::Success;
                        status_msg = format!("Track {} → file {}.", tn, path_str);
                    }
                    Err(e) => {
                        status_kind = StatusKind::Error;
                        status_msg = format!("File load: {}", e);
                    }
                }
            } else {
                status_kind = StatusKind::Warning;
                status_msg = parse_track_no(parts[1], n)
                    .err()
                    .unwrap_or_else(|| "Usage: input <track_no> --file <path>".to_string());
            }
        }
        ["gain", level] => {
            if let Ok(g) = level.parse::<f32>() {
                session.master_gain = g.clamp(0.0, 2.0);
                session_changed = true;
                status_kind = StatusKind::Success;
                status_msg = format!("Master gain set to {}.", session.master_gain);
            } else {
                status_msg = "Usage: gain <level>  or  gain <track_no> <level>".to_string();
            }
        }
        ["gain", track_no, level] => {
            if let (Ok(tn), Ok(g)) = (parse_track_no(track_no, n), level.parse::<f32>()) {
                session.tracks[tn - 1].gain = g.clamp(0.0, 2.0);
                session_changed = true;
                status_kind = StatusKind::Success;
                status_msg = format!("Track {} gain set to {}.", tn, session.tracks[tn - 1].gain);
            } else {
                status_kind = StatusKind::Warning;
                status_msg = parse_track_no(track_no, n)
                    .err()
                    .unwrap_or_else(|| "Usage: gain <track_no> <level>".to_string());
            }
        }
        ["echo", track_no, "none"] => match parse_track_no(track_no, n) {
            Ok(tn) => {
                session.tracks[tn - 1].delay_ms = None;
                session_changed = true;
                status_kind = StatusKind::Success;
                status_msg = format!("Track {} echo removed.", tn);
            }
            Err(e) => {
                status_kind = StatusKind::Warning;
                status_msg = e;
            }
        },
        ["echo", track_no, ms_str] => {
            if let Ok(tn) = parse_track_no(track_no, n) {
                if let Ok(ms) = ms_str.parse::<f32>() {
                    if (0.0..=2000.0).contains(&ms) {
                        session.tracks[tn - 1].delay_ms = Some(ms);
                        session_changed = true;
                        status_kind = StatusKind::Success;
                        status_msg = format!("Track {} echo set to {:.0} ms.", tn, ms);
                    } else {
                        status_kind = StatusKind::Warning;
                        status_msg = "Echo must be 0–2000 ms.".to_string();
                    }
                } else {
                    status_msg = "Usage: echo <track_no> <ms> | echo <track_no> none".to_string();
                }
            } else {
                status_kind = StatusKind::Warning;
                status_msg = parse_track_no(track_no, n).err().unwrap_or_default();
            }
        }
        ["tremolo", track_no, "none"] => match parse_track_no(track_no, n) {
            Ok(tn) => {
                session.tracks[tn - 1].tremolo = None;
                session_changed = true;
                status_kind = StatusKind::Success;
                status_msg = format!("Track {} tremolo removed.", tn);
            }
            Err(e) => {
                status_kind = StatusKind::Warning;
                status_msg = e;
            }
        },
        ["tremolo", track_no, rate_str, depth_str] => {
            if let (Ok(tn), Ok(rate), Ok(depth)) = (
                parse_track_no(track_no, n),
                rate_str.parse::<f32>(),
                depth_str.parse::<f32>(),
            ) {
                if (0.1..=20.0).contains(&rate) && (0.0..=1.0).contains(&depth) {
                    session.tracks[tn - 1].tremolo = Some((rate, depth));
                    session_changed = true;
                    status_kind = StatusKind::Success;
                    status_msg = format!("Track {} tremolo {:.1} Hz depth {:.2}.", tn, rate, depth);
                } else {
                    status_kind = StatusKind::Warning;
                    status_msg = "Tremolo rate 0.1–20 Hz, depth 0–1.".to_string();
                }
            } else {
                status_kind = StatusKind::Warning;
                status_msg = parse_track_no(track_no, n).err().unwrap_or_else(|| {
                    "Usage: tremolo <track_no> <rate_hz> <depth> | tremolo <track_no> none"
                        .to_string()
                });
            }
        }
        ["overdrive", track_no, "none"] => match parse_track_no(track_no, n) {
            Ok(tn) => {
                session.tracks[tn - 1].overdrive = None;
                session_changed = true;
                status_kind = StatusKind::Success;
                status_msg = format!("Track {} overdrive removed.", tn);
            }
            Err(e) => {
                status_kind = StatusKind::Warning;
                status_msg = e;
            }
        },
        ["overdrive", track_no, amount_str] => {
            if let (Ok(tn), Ok(amount)) = (parse_track_no(track_no, n), amount_str.parse::<f32>()) {
                if (0.0..=5.0).contains(&amount) {
                    session.tracks[tn - 1].overdrive = Some(amount);
                    session_changed = true;
                    status_kind = StatusKind::Success;
                    status_msg = format!("Track {} overdrive {:.1}.", tn, amount);
                } else {
                    status_kind = StatusKind::Warning;
                    status_msg = "Overdrive must be 0–5.".to_string();
                }
            } else {
                status_kind = StatusKind::Warning;
                status_msg = parse_track_no(track_no, n).err().unwrap_or_else(|| {
                    "Usage: overdrive <track_no> <0-5> | overdrive <track_no> none".to_string()
                });
            }
        }
        _ => {
            status_kind = StatusKind::Warning;
            status_msg = "Unknown command. Type 'help' for commands.".to_string();
        }
    }

    if session_changed {
        session.meter_buffer = Some(Arc::new(MeterBuffer::new(session.tracks.len() + 1)));
        if let Some(compiled) = build_session_graph(session, silent_buffer, None) {
            send_graph(cmd_tx, compiled);
        } else {
            status_kind = StatusKind::Error;
            status_msg = "Failed to compile graph.".to_string();
        }
    }

    CommandOutcome {
        status_kind,
        status_msg,
        quit,
    }
}

// -----------------------------------------------------------------------------
// Recording
// -----------------------------------------------------------------------------

fn stop_recording_and_save(
    session: &mut Session,
    cmd_tx: &capstan::command::CommandSender,
    silent_buffer: &Arc<dyn SampleSource + Send + Sync>,
    history: &mut Vec<String>,
) -> std::io::Result<()> {
    if let (Some(rb), Some(path)) = (
        session.record_buffer.take(),
        session.record_output_path.take(),
    ) {
        rb.set_armed(false);
        thread::sleep(Duration::from_millis(150));
        let samples = rb.drain();
        let _ = path.parent().map(std::fs::create_dir_all);
        match write_wav(&path, &samples, session.output_sample_rate) {
            Ok(()) => {
                history.push("> record".to_string());
                history.push(format!("{}Saved to {}", SUCCESS_PREFIX, path.display()));
            }
            Err(e) => {
                history.push("> record".to_string());
                history.push(format!("{}Write WAV: {}", ERROR_PREFIX, e));
            }
        }
    }
    session.recording = false;
    if let Some(compiled) = build_session_graph(session, silent_buffer, None) {
        send_graph(cmd_tx, compiled);
    }
    Ok(())
}

// -----------------------------------------------------------------------------
// Main
// -----------------------------------------------------------------------------

fn main() -> std::io::Result<()> {
    let cli = Cli::parse();

    let (cmd_tx, cmd_rx) = command_channel(cli.channel_capacity);
    let (evt_tx, evt_rx) = event_channel(cli.channel_capacity);
    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel();
    let (audio_result_tx, audio_result_rx) = std::sync::mpsc::channel();
    let host = capstan::cpal::default_host();

    let audio_handle = thread::spawn(move || {
        let result = run_audio(cmd_rx, evt_tx, shutdown_rx, None);
        let _ = audio_result_tx.send(result);
    });

    let silent_buffer: Arc<dyn SampleSource + Send + Sync> = Arc::new(InputSampleBuffer::new(2048));
    let mut session = Session {
        tracks: Vec::new(),
        master_gain: 0.8,
        open_inputs: OpenInputs::new(),
        meter_buffer: Some(Arc::new(MeterBuffer::new(1))),
        output_sample_rate: capstan::default_output_sample_rate().unwrap_or(48_000),
        recording: false,
        record_buffer: None,
        record_output_path: None,
    };

    let mut input_line = String::new();
    let mut cursor_pos: usize = 0;
    let mut history: Vec<String> = Vec::new();
    let mut command_history: Vec<String> = Vec::new();
    let mut history_index: Option<usize> = None;

    enable_raw_mode().map_err(std::io::Error::other)?;
    let mut stdout = io::stdout();
    execute!(stdout, Clear(ClearType::All), MoveTo(0, 0)).map_err(std::io::Error::other)?;
    stdout.flush()?;

    if let Some(compiled) = build_session_graph(&session, &silent_buffer, None) {
        send_graph(&cmd_tx, compiled);
    }

    loop {
        if let Ok(Err(e)) = audio_result_rx.try_recv() {
            disable_raw_mode().map_err(std::io::Error::other)?;
            eprintln!("Audio error: {}", e);
            let _ = audio_handle.join();
            return Err(std::io::Error::other(e.to_string()));
        }

        let pr = prompt_row(&session.tracks);
        let peaks = session
            .meter_buffer
            .as_ref()
            .map(|m| m.read_peaks())
            .unwrap_or_default();
        draw_header(
            &mut stdout,
            &session.tracks,
            &peaks,
            session.master_gain,
            pr,
        )?;
        execute!(stdout, MoveTo(0, pr), Clear(ClearType::CurrentLine))?;
        write!(stdout, "> {}", input_line)?;
        draw_history(&mut stdout, &history, pr + 1, HISTORY_LINES)?;
        let cursor_col = (2 + cursor_pos.min(input_line.len())).min(u16::MAX as usize) as u16;
        execute!(stdout, MoveTo(cursor_col, pr), Show)?;
        stdout.flush()?;

        if event::poll(Duration::from_millis(HEADER_REDRAW_MS)).map_err(std::io::Error::other)? {
            if let Ok(Event::Key(ke)) = event::read() {
                if ke.kind != KeyEventKind::Press {
                    continue;
                }
                match ke.code {
                    KeyCode::Enter => {
                        if session.recording {
                            stop_recording_and_save(
                                &mut session,
                                &cmd_tx,
                                &silent_buffer,
                                &mut history,
                            )?;
                        } else {
                            let line = input_line.trim().to_string();
                            input_line.clear();
                            cursor_pos = 0;
                            if !line.is_empty() {
                                let parts: Vec<&str> = line.split_ascii_whitespace().collect();
                                let outcome = handle_command(
                                    &mut session,
                                    &parts,
                                    &host,
                                    &cmd_tx,
                                    &silent_buffer,
                                );
                                history.push(format!("> {}", line));
                                let result_line = match outcome.status_kind {
                                    StatusKind::Success => {
                                        format!("{}{}", SUCCESS_PREFIX, outcome.status_msg)
                                    }
                                    StatusKind::Warning => {
                                        format!("{}{}", WARNING_PREFIX, outcome.status_msg)
                                    }
                                    StatusKind::Error => {
                                        format!("{}{}", ERROR_PREFIX, outcome.status_msg)
                                    }
                                    StatusKind::Neutral => format!("  {}", outcome.status_msg),
                                };
                                history.push(result_line);
                                command_history.push(line);
                                if command_history.len() > COMMAND_HISTORY_CAP {
                                    command_history.remove(0);
                                }
                                history_index = None;

                                if outcome.quit {
                                    let _ = cmd_tx.try_send(Command::Quit);
                                    let _ = shutdown_tx.send(());
                                    disable_raw_mode().map_err(std::io::Error::other)?;
                                    let _ = audio_handle.join();
                                    if let Ok(Err(e)) = audio_result_rx.recv() {
                                        eprintln!("Audio error: {}", e);
                                    }
                                    execute!(stdout, Clear(ClearType::All), MoveTo(0, 0))
                                        .map_err(std::io::Error::other)?;
                                    stdout.flush()?;
                                    return Ok(());
                                }
                            }
                        }
                    }
                    KeyCode::Left => {
                        cursor_pos = cursor_pos.saturating_sub(1);
                    }
                    KeyCode::Right => {
                        cursor_pos = (cursor_pos + 1).min(input_line.len());
                    }
                    KeyCode::Up => {
                        if !command_history.is_empty() {
                            if let Some(i) = history_index {
                                if i + 1 < command_history.len() {
                                    history_index = Some(i + 1);
                                    input_line = command_history
                                        [command_history.len() - 1 - (i + 1)]
                                        .clone();
                                    cursor_pos = input_line.len();
                                }
                            } else {
                                history_index = Some(0);
                                input_line = command_history.last().cloned().unwrap_or_default();
                                cursor_pos = input_line.len();
                            }
                        }
                    }
                    KeyCode::Down => {
                        if let Some(0) = history_index {
                            history_index = None;
                            input_line = String::new();
                            cursor_pos = 0;
                        } else if let Some(i) = history_index {
                            history_index = Some(i - 1);
                            input_line =
                                command_history[command_history.len() - 1 - (i - 1)].clone();
                            cursor_pos = input_line.len();
                        }
                    }
                    KeyCode::Char(c) => {
                        history_index = None;
                        let pos = cursor_pos.min(input_line.len());
                        input_line.insert(pos, c);
                        cursor_pos = pos + 1;
                    }
                    KeyCode::Backspace => {
                        history_index = None;
                        if cursor_pos > 0 && cursor_pos <= input_line.len() {
                            cursor_pos -= 1;
                            input_line.remove(cursor_pos);
                        }
                    }
                    _ => {}
                }
            }
        }

        while let Some(evt) = evt_rx.try_recv() {
            if let capstan::event::Event::StreamStarted(sr) = evt {
                session.output_sample_rate = sr;
                history.push(format!("{}Output sample rate: {} Hz", SUCCESS_PREFIX, sr));
            }
        }
    }
}
