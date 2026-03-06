//! Example: CLI-based DAW (MVP) — tracks, input devices, per-track and master gain.
//! Sticky header at top lists tracks and draws live level meters (ASCII).
//!
//! Run with: `cargo run --example daw`

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
use capstan::nodes::{GainProcessor, InputNode, Mixer, SineGenerator};
use capstan::run_audio;
use clap::Parser;
use crossterm::cursor::{MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType};

const DEFAULT_FRAME_COUNT: usize = 512;
const METER_WIDTH: usize = 24;
const HEADER_REDRAW_MS: u64 = 80;
/// dB range for the bar: bar is empty at DB_MIN, full at 0 dB. Makes the meter more sensitive to low levels.
const METER_DB_MIN: f32 = -60.0;

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

fn build_session_graph(
    tracks: &[Track],
    open_inputs: &OpenInputs,
    silent_buffer: &Arc<dyn SampleSource + Send + Sync>,
    master_gain: f32,
    meter_buffer: Option<Arc<MeterBuffer>>,
    sample_rate: u32,
) -> Option<CompiledGraph> {
    use std::iter::once;
    let mut g = AudioGraph::new();
    if tracks.is_empty() {
        let inp = g.add_node(GraphNode::Input(InputNode::new(Arc::clone(silent_buffer))));
        let out = g.add_node(GraphNode::Gain(GainProcessor::new(master_gain)));
        g.add_edge(inp, out);
        return match &meter_buffer {
            Some(mb) if mb.len() == 1 => g
                .compile_with_meter(DEFAULT_FRAME_COUNT, Some((vec![1], Arc::clone(mb))))
                .ok(),
            _ => g.compile(DEFAULT_FRAME_COUNT).ok(),
        };
    }

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
        let gain = g.add_node(GraphNode::Gain(GainProcessor::new(track.gain)));
        g.add_edge(source_node, gain);
        gain_node_ids.push(gain);
    }

    let gains: Vec<f32> = tracks.iter().map(|t| t.gain).collect();
    let mix = g.add_node(GraphNode::Mixer(Mixer::new(gains)));
    for &gid in &gain_node_ids {
        g.add_edge(gid, mix);
    }
    let master = g.add_node(GraphNode::Gain(GainProcessor::new(master_gain)));
    g.add_edge(mix, master);

    let n = tracks.len();
    // Node order: sources 0..n, gains n..2n, mixer 2n, master 2n+1. Tap track gains + master.
    match &meter_buffer {
        Some(mb) if mb.len() == n + 1 => {
            let tap_indices: Vec<usize> = (0..n).map(|i| n + i).chain(once(2 * n + 1)).collect();
            g.compile_with_meter(DEFAULT_FRAME_COUNT, Some((tap_indices, Arc::clone(mb))))
                .ok()
        }
        _ => g.compile(DEFAULT_FRAME_COUNT).ok(),
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

/// Linear peak (0..=1 or higher) to dB. Uses a floor for near-silence.
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
    writeln!(stdout, " track | source     | gain  | level")?;
    line += 1;
    for (i, track) in tracks.iter().enumerate() {
        let src = source_display(&track.source);
        let peak = peaks.get(i).copied().unwrap_or(0.0);
        execute!(stdout, MoveTo(0, line), Clear(ClearType::CurrentLine))?;
        writeln!(
            stdout,
            "   {}   | {:>10} | {:.2}  | {}",
            i + 1,
            src,
            track.gain,
            ascii_meter_with_db(peak)
        )?;
        line += 1;
    }
    // Master row: always show. peaks has one extra slot for master (last element).
    let master_peak = peaks.last().copied().unwrap_or(0.0);
    execute!(stdout, MoveTo(0, line), Clear(ClearType::CurrentLine))?;
    writeln!(
        stdout,
        " master| {:>10} | {:.2}  | {}",
        "(mix)",
        master_gain,
        ascii_meter_with_db(master_peak)
    )?;
    line += 1;
    for y in line..prompt_row {
        execute!(stdout, MoveTo(0, y), Clear(ClearType::CurrentLine))?;
    }
    Ok(())
}

const SUCCESS_PREFIX: &str = "  ✓ ";
/// Zero-width space + "  " so warnings display as "  msg" (no symbol) in yellow.
const WARNING_PREFIX: &str = "\u{200B}  ";
const ERROR_PREFIX: &str = "  ✗ ";

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

#[derive(Clone, Copy)]
enum StatusKind {
    Success,
    Warning,
    Error,
    Neutral,
}

/// Last 3 commands (each command + result = 2 lines), shown newest-first.
const HISTORY_LINES: usize = 6;

const COMMAND_HISTORY_CAP: usize = 50;

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

    let mut output_sample_rate = capstan::default_output_sample_rate().unwrap_or(48_000);

    let mut tracks: Vec<Track> = Vec::new();
    let mut master_gain: f32 = 0.8;
    let mut open_inputs = OpenInputs::new();
    let silent_buffer: Arc<dyn SampleSource + Send + Sync> = Arc::new(InputSampleBuffer::new(2048));
    let mut meter_buffer = Some(Arc::new(MeterBuffer::new(tracks.len() + 1)));
    let mut input_line = String::new();
    let mut cursor_pos: usize = 0; // index into input_line (0..=len)
    let mut status_msg: String;
    let mut history: Vec<String> = Vec::new();
    let mut command_history: Vec<String> = Vec::new();
    let mut history_index: Option<usize> = None;

    enable_raw_mode().map_err(std::io::Error::other)?;
    let mut stdout = io::stdout();

    execute!(stdout, Clear(ClearType::All), MoveTo(0, 0)).map_err(std::io::Error::other)?;
    stdout.flush()?;

    // Initial graph with master meter (1 slot when no tracks).
    if let Some(compiled) = build_session_graph(
        &tracks,
        &open_inputs,
        &silent_buffer,
        master_gain,
        meter_buffer.clone(),
        output_sample_rate,
    ) {
        send_graph(&cmd_tx, compiled);
    }

    loop {
        if let Ok(Err(e)) = audio_result_rx.try_recv() {
            disable_raw_mode().map_err(std::io::Error::other)?;
            eprintln!("Audio error: {}", e);
            let _ = audio_handle.join();
            return Err(std::io::Error::other(e.to_string()));
        }

        let pr = prompt_row(&tracks);
        let peaks = meter_buffer
            .as_ref()
            .map(|m| m.read_peaks())
            .unwrap_or_default();
        draw_header(&mut stdout, &tracks, &peaks, master_gain, pr)?;
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
                        let line = input_line.trim().to_string();
                        input_line.clear();
                        cursor_pos = 0;
                        if !line.is_empty() {
                            let parts: Vec<&str> = line.split_ascii_whitespace().collect();
                            let mut session_changed = false;
                            let mut status_kind = StatusKind::Neutral;

                            match parts.as_slice() {
                                ["quit" | "q"] => {
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
                                ["help" | "h" | "?"] => {
                                    status_msg = "track create | track delete <no> | input --list | input <tn> --device <n> | input <tn> --sine <hz> | input <tn> --file <path> | gain [tn] <lvl> | quit".to_string();
                                }

                                ["track", "create"] => {
                                    tracks.push(Track {
                                        source: TrackSource::None,
                                        gain: 0.7,
                                    });
                                    session_changed = true;
                                    status_kind = StatusKind::Success;
                                    status_msg = format!("Created track {}.", tracks.len());
                                }
                                ["track", "delete", no] => {
                                    if let Ok(n) = no.parse::<usize>() {
                                        if n >= 1 && n <= tracks.len() {
                                            tracks.remove(n - 1);
                                            session_changed = true;
                                            status_kind = StatusKind::Success;
                                            status_msg = format!("Deleted track {}.", n);
                                        } else {
                                            status_kind = StatusKind::Warning;
                                            status_msg = format!(
                                                "Track number must be 1–{}.",
                                                tracks.len().max(1)
                                            );
                                        }
                                    } else {
                                        status_msg = "Usage: track delete <track_no>".to_string();
                                    }
                                }

                                ["input", "--list" | "-l"] => match input_device_list(&host) {
                                    Ok(devices) => {
                                        status_kind = StatusKind::Success;
                                        if devices.is_empty() {
                                            status_msg = "(no input devices)".to_string();
                                        } else {
                                            status_msg = devices
                                                .iter()
                                                .map(|d| format!("{}: {}", d.index, d.name))
                                                .collect::<Vec<_>>()
                                                .join("  |  ");
                                            if status_msg.len() > 120 {
                                                status_msg = format!("{}...", &status_msg[..117]);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        status_kind = StatusKind::Error;
                                        status_msg = format!("List devices: {}", e);
                                    }
                                },
                                ["input", track_no, "--device", dev] => {
                                    if let (Ok(tn), Ok(d)) =
                                        (track_no.parse::<usize>(), dev.parse::<usize>())
                                    {
                                        if tn >= 1 && tn <= tracks.len() {
                                            match open_inputs.ensure_device(&host, d) {
                                                Ok(_) => {
                                                    tracks[tn - 1].source = TrackSource::Device(d);
                                                    session_changed = true;
                                                    status_kind = StatusKind::Success;
                                                    status_msg =
                                                        format!("Track {} → device {}.", tn, d);
                                                }
                                                Err(e) => {
                                                    status_kind = StatusKind::Error;
                                                    status_msg = format!(
                                                        "Failed to open device {}: {}",
                                                        d, e
                                                    );
                                                }
                                            }
                                        } else {
                                            status_kind = StatusKind::Warning;
                                            status_msg = format!(
                                                "Track number must be 1–{}.",
                                                tracks.len().max(1)
                                            );
                                        }
                                    } else {
                                        status_msg =
                                            "Usage: input <track_no> --device <index>".to_string();
                                    }
                                }
                                ["input", track_no, "--sine", freq] => {
                                    if let (Ok(tn), Ok(f)) =
                                        (track_no.parse::<usize>(), freq.parse::<f32>())
                                    {
                                        if tn >= 1 && tn <= tracks.len() && f > 0.0 && f <= 20_000.0
                                        {
                                            tracks[tn - 1].source =
                                                TrackSource::Sine { freq_hz: f };
                                            session_changed = true;
                                            status_kind = StatusKind::Success;
                                            status_msg = format!("Track {} → sine {} Hz.", tn, f);
                                        } else if tn < 1 || tn > tracks.len() {
                                            status_kind = StatusKind::Warning;
                                            status_msg = format!(
                                                "Track number must be 1–{}.",
                                                tracks.len().max(1)
                                            );
                                        } else {
                                            status_kind = StatusKind::Warning;
                                            status_msg =
                                                "Frequency must be 0–20000 Hz.".to_string();
                                        }
                                    } else {
                                        status_msg =
                                            "Usage: input <track_no> --sine <freq_hz>".to_string();
                                    }
                                }
                                _ if parts.len() >= 4
                                    && parts[0] == "input"
                                    && parts[2] == "--file" =>
                                {
                                    let path_str = parts[3..].join(" ");
                                    if let Ok(tn) = parts[1].parse::<usize>() {
                                        if tn >= 1 && tn <= tracks.len() {
                                            let path = PathBuf::from(&path_str);
                                            match load_wav_at_rate(&path, output_sample_rate) {
                                                Ok(samples) => {
                                                    let buffer: Arc<
                                                        dyn SampleSource + Send + Sync,
                                                    > = Arc::new(FilePlaybackBuffer::new(
                                                        Arc::new(samples),
                                                    ));
                                                    tracks[tn - 1].source =
                                                        TrackSource::File { path, buffer };
                                                    session_changed = true;
                                                    status_kind = StatusKind::Success;
                                                    status_msg = format!(
                                                        "Track {} → file {}.",
                                                        tn, path_str
                                                    );
                                                }
                                                Err(e) => {
                                                    status_kind = StatusKind::Error;
                                                    status_msg = format!("File load: {}", e);
                                                }
                                            }
                                        } else {
                                            status_kind = StatusKind::Warning;
                                            status_msg = format!(
                                                "Track number must be 1–{}.",
                                                tracks.len().max(1)
                                            );
                                        }
                                    } else {
                                        status_msg =
                                            "Usage: input <track_no> --file <path>".to_string();
                                    }
                                }

                                ["gain", level] => {
                                    if let Ok(g) = level.parse::<f32>() {
                                        master_gain = g.clamp(0.0, 2.0);
                                        session_changed = true;
                                        status_kind = StatusKind::Success;
                                        status_msg = format!("Master gain set to {}.", master_gain);
                                    } else {
                                        status_msg =
                                            "Usage: gain <level>  or  gain <track_no> <level>"
                                                .to_string();
                                    }
                                }
                                ["gain", track_no, level] => {
                                    if let (Ok(tn), Ok(g)) =
                                        (track_no.parse::<usize>(), level.parse::<f32>())
                                    {
                                        if tn >= 1 && tn <= tracks.len() {
                                            tracks[tn - 1].gain = g.clamp(0.0, 2.0);
                                            session_changed = true;
                                            status_kind = StatusKind::Success;
                                            status_msg = format!(
                                                "Track {} gain set to {}.",
                                                tn,
                                                tracks[tn - 1].gain
                                            );
                                        } else {
                                            status_kind = StatusKind::Warning;
                                            status_msg = format!(
                                                "Track number must be 1–{}.",
                                                tracks.len().max(1)
                                            );
                                        }
                                    } else {
                                        status_msg = "Usage: gain <track_no> <level>".to_string();
                                    }
                                }

                                _ => {
                                    status_kind = StatusKind::Warning;
                                    status_msg =
                                        "Unknown command. Type 'help' for commands.".to_string();
                                }
                            }

                            if session_changed {
                                // One slot per track + one for master (always shown).
                                meter_buffer = Some(Arc::new(MeterBuffer::new(tracks.len() + 1)));
                                if let Some(compiled) = build_session_graph(
                                    &tracks,
                                    &open_inputs,
                                    &silent_buffer,
                                    master_gain,
                                    meter_buffer.clone(),
                                    output_sample_rate,
                                ) {
                                    send_graph(&cmd_tx, compiled);
                                } else {
                                    status_kind = StatusKind::Error;
                                    status_msg = "Failed to compile graph.".to_string();
                                }
                            }

                            history.push(format!("> {}", line));
                            let result_line = match status_kind {
                                StatusKind::Success => format!("{}{}", SUCCESS_PREFIX, status_msg),
                                StatusKind::Warning => format!("{}{}", WARNING_PREFIX, status_msg),
                                StatusKind::Error => format!("{}{}", ERROR_PREFIX, status_msg),
                                StatusKind::Neutral => format!("  {}", status_msg),
                            };
                            history.push(result_line);
                            command_history.push(line);
                            if command_history.len() > COMMAND_HISTORY_CAP {
                                command_history.remove(0);
                            }
                            history_index = None;
                        }
                    }
                    KeyCode::Left => {
                        cursor_pos = cursor_pos.saturating_sub(1);
                    }
                    KeyCode::Right => {
                        cursor_pos = (cursor_pos + 1).min(input_line.len());
                    }
                    KeyCode::Up => {
                        if command_history.is_empty() {
                            // no-op
                        } else if let Some(i) = history_index {
                            if i + 1 < command_history.len() {
                                history_index = Some(i + 1);
                                input_line =
                                    command_history[command_history.len() - 1 - (i + 1)].clone();
                                cursor_pos = input_line.len();
                            }
                        } else {
                            history_index = Some(0);
                            input_line = command_history.last().cloned().unwrap_or_default();
                            cursor_pos = input_line.len();
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
                output_sample_rate = sr;
                history.push(format!("{}Output sample rate: {} Hz", SUCCESS_PREFIX, sr));
            }
        }
    }
}
