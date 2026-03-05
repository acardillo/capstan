# Architecture

Control-thread builds the patch; audio-thread runs it. Communication is lock-free SPSC (Commands control→audio, Events audio→control). No allocation on the audio path after startup.

## Core types and threads

| Type              | Thread          | Role                                                                                                                                                                    |
| ----------------- | --------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **AudioGraph**    | Control         | Mutable **Directed Acyclic Graph** with nodes + adjacency list (edges).                                                                                                 |
| **CompiledGraph** | Control + Audio | Immutable: nodes in **topological order**, one **AudioBuffer** scratch per node. Each node reads and writes to its own buffer. The last buffer is copied to the output. |
| **Engine**        | Audio           | Each callback: drain **Command**s, apply (e.g. SwapGraph, Quit), then run `current_graph.process(output)` or silence.                                                   |
| **AudioBuffer**   | Audio           | Fixed-size f32 array per node. Allocated at compile time; reused every callback.                                                                                        |
| **RingBuffer**    | Control + Audio | Lock-free **Single Producer, Single Consumer** buffer; fixed capacity;                                                                                                  |

## Data flow

```
| CONTROL THREAD   |                           | AUDIO THREAD             |
| ================ |                           | ======================== |
|                  |                           |                          |
|   AudioGraph     |                           |                          |
|     |            |                           |                          |
|     | compile    |                           |                          |
|     |            |                           |                          |
|     v            |                           |                          |
|  CompiledGraph   | ---- SwapGraph(new) ----> |  Engine                  |
|                  |                           |    |                     |
|                  |                           |    | CPAL callback:      |
|                  |                           |    |  1. drain commands  |
|                  |                           |    |  2. graph.process   |
|                  |                           |    |                     |
|                  |                           |    v                     |
|                  |                           |  Output to device        |
```

## Commands and events

**Command** (control → audio): `NoOp`, `SetGain(f32)`, `Quit`, `Resume`, `SwapGraph(CompiledGraph)`. Engine drains at the top of each callback; SwapGraph replaces the current graph and sends the previous one back as **Event::GraphSwapped** so the control thread can drop it (no leak).

**Event** (audio → control): `NoOp`, `GraphSwapped(CompiledGraph)`, `StreamStopped`, **StreamStarted(u32)**. StreamStarted is sent once when `run_audio` has the output config; the `u32` is the **actual output sample rate**. Use it for WAV resampling and graph construction so playback and processing match the device. Poll the event channel (e.g. in the main loop); no blocking.

## Sample sources and InputNode

**InputNode** takes `Arc<dyn SampleSource + Send + Sync>` and, each block, calls `read_block(output)` to fill its output. Two implementations:

- **InputSampleBuffer** — Lock-free SPSC ring. **Producer**: input stream callback (e.g. mic) via `write_block(data, channels)`. **Consumer**: graph’s InputNode in the output callback. On overflow, oldest samples are dropped (read index advanced). Use for live input. `run_audio(..., Some(buffer))` wires the **default** input device to that buffer; for a specific device use **device::open_input_stream** and keep the returned Stream alive.
- **FilePlaybackBuffer** — Whole file in memory (mono f32 at output rate). Single atomic read position; **pull-based**: the output callback reads directly. No feeder thread, so no producer/consumer rate mismatch or overflow crackle. Build with **file_feeder::load_wav_at_rate(path, output_sample_rate)** so the file is resampled to the same rate as StreamStarted.

**SampleSource** trait: `read_block(&self, out: &mut [f32]) -> usize`. Fills `out` (zeroes unused suffix on underrun). Both buffer types implement it so InputNode is agnostic.

## File feeder (WAV)

- **load_wav_at_rate(path, target_sample_rate)** — Load WAV → mono f32; resample to target if needed (linear interpolation; one-pole lowpass when downsampling). Returns `Vec<f32>`. Use with `FilePlaybackBuffer::new(Arc::new(samples))`. Prefer this for playback.
- **resample_to_rate(mono, file_rate, target_rate)** — Resample a mono buffer (e.g. for custom pipelines).
- **start_file_feeder(path, buffer, target_rate)** — Optional: background thread pushes into an **InputSampleBuffer** at realtime. Use only if you need push-based file input; pull-based FilePlaybackBuffer is simpler and avoids timing issues.

Use **StreamStarted(sample_rate)** as `target_sample_rate` so the file matches the output device.

## Meter taps

**MeterBuffer**: fixed number of slots; each holds one f32 (peak). **Audio thread** writes via `write_peak(slot, value)`; **control thread** reads via `read_peaks()` (e.g. each UI frame). Lock-free (atomics).

**compile_with_meter(frame_count, Some((tap_indices, meter_buffer)))** — `tap_indices`: scratch-buffer indices in **topo order** (same order as nodes in CompiledGraph). After each `process()` call, **CompiledGraph** computes the peak (max abs) of each tapped scratch and writes it to the corresponding MeterBuffer slot. So the **graph** does the metering, not the Engine. Requirement: `tap_indices.len() == meter_buffer.len()` and each index `< node_count`. Typical: one tap per track gain output, one for master.

## Device

- **input_device_list(host)** → `Vec<InputDeviceInfo>` (index, name). Use index with open_input_stream.
- **open_input_stream(host, device_index, buffer)** — Opens that input device, F32, low-latency config; callback writes first channel into the given **InputSampleBuffer**. Returns **cpal::Stream**; caller must keep it alive.

CPAL and stream lifecycle stay inside the crate; the app chooses device index and holds the Stream.

## run_audio (lib)

Blocks until shutdown. Does: default output device, low-latency config, send **StreamStarted(sample_rate)**, build Engine, optionally open **default input** and feed one **InputSampleBuffer**, build output stream, run callback (drain commands, run graph, interleave mono→stereo). F32 only. The app supplies command receiver, event sender, shutdown receiver, and optionally one input buffer for the default mic.

## Summary

- **Graph** = DAG on control; **CompiledGraph** = topo-ordered execution plan + scratch buffers + optional meter taps; **Engine** = run it on the audio thread.
- **Command/Event** = lock-free SPSC; drain on audio, poll on control.
- **SampleSource** = thing the graph reads from; **InputSampleBuffer** (live) and **FilePlaybackBuffer** (file, pull-based) implement it.
- **MeterBuffer** + **tap_indices** in compile = per-tap peaks written by **CompiledGraph::process**, read by control for meters.
- **StreamStarted(rate)** = use this rate for files and graph so everything is sample-locked.
