# Architecture

Capstan is a composable DSP library that uses two threads:

1. **Control Thread** - Responsible for modifying signal processing chains.
2. **Audio Thread** - Responsible for running the signal processing chains and computing output samples.

Threads communicate with each other via two lock-free buffers:

1. **Command Buffer** - Commands from the control thread to the audio thread.
2. **Event Buffer** - Events from the audio thread to the control thread.

_Commands_ are used to modify the audio graph and to quit the audio thread.
_Events_ are used to notify the control thread of events such as the audio thread starting or stopping.

## Core Types

| Type              | Thread  | Role                                                                                                                                                                    |
| ----------------- | ------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **AudioGraph**    | Control | Mutable **Directed Acyclic Graph** with nodes + adjacency list (edges).                                                                                                 |
| **CompiledGraph** | Both    | Immutable: nodes in **topological order**, one **AudioBuffer** scratch per node. Each node reads and writes to its own buffer. The last buffer is copied to the output. |
| **Engine**        | Audio   | Each callback: drain **Command**s, apply (e.g. SwapGraph, Quit), then run `current_graph.process(output)` or silence.                                                   |
| **AudioBuffer**   | Audio   | Fixed-size f32 array per node. Allocated at compile time; reused every callback.                                                                                        |
| **RingBuffer**    | Both    | Lock-free **Single Producer, Single Consumer** buffer; fixed capacity;                                                                                                  |

## Audio Control Flow

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

## Commands

_Commands_ are used to modify the audio graph and to quit the audio thread. They are sent from the control thread and received by the audio thread. At the start of each audio callback, the audio thread drains the command buffer and applies the commands.

`NoOp`, `SetGain(level)`, `Quit`, `Resume`, `SwapGraph(CompiledGraph)`.

## Events

_Events_ are used to notify the control thread of events such as the audio thread starting or stopping. They are sent from the audio thread and received by the control thread. The application should poll the event buffer in the main loop and handle the events accordingly.

`NoOp`, `GraphSwapped(CompiledGraph)`, `StreamStopped`, `StreamStarted(sampleRate)`.

## Input Types

Inputs are just nodes in the Audio Graph that generate samples. There are three types of inputs:

| Type       | Graph node                                                  | Where samples come from                                                                                              |
| ---------- | ----------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| **Sine**   | `GraphNode::Sine(SineGenerator::new(freq, sample_rate))`    | Produces a basic tone at a given frequency. Computed deterministically so no buffer is needed.                       |
| **Device** | `GraphNode::Input(InputNode::new(Arc<InputSampleBuffer>))`  | Live audio from an input device. An input stream callback writes to a SPSC buffer and the audio graph reads from it. |
| **File**   | `GraphNode::Input(InputNode::new(Arc<FilePlaybackBuffer>))` | Reads WAV files from memory. The audio graph reads via an atomic position each block.                                |

### SampleSource and Buffer Types

Device and File inputs use the **SampleSource** trait via **InputNode**. Sample sources provide a `read_block` method that fills the output buffer. Each type implements this trait differently:

- **InputSampleBuffer** — A lock-free SPSC buffer. **Producer**: the input stream callback from CPAL. **Consumer**: the graph's InputNode in the output callback. On overflow, oldest samples are dropped.

- **FilePlaybackBuffer** — Stores the whole file in memory as mono samples at the output sample rate. The audio graph reads from it directly. Using one thread ensures no rate mismatch or overflow. Memory is loaded via the _File Feeder_.

**File Feeder** - Loads WAV files and resamples them for use with file-playback inputs.

- **load_wav_at_rate(path, target_sample_rate)** — Load WAV as mono f32, resample to target rate. Use with `FilePlaybackBuffer` for file tracks.
- **resample_to_rate(mono, file_rate, target_rate)** — Resample a mono buffer.

The _StreamStarted(sample_rate)_ event is used to set the target rate so the file matches the output device.

## Meter Taps

Meter taps are used to report the peak level of nodes in the audio graph to the control thread.

**MeterBuffer**: A fixed SPSC buffer holding one f32 (peak) per tap. Typically one tap is used per gain node in the audio graph and one for the master output.

- **MeterBuffer::new(num_taps)** — Creates a meter buffer with the given number of taps.
- **MeterBuffer::write_peak(slot, value)** — Writes the peak level to the given slot. Called from the audio thread after processing a block.
- **MeterBuffer::read_peaks()** — Reads all current peak levels. Called from the control thread to get the peak levels for UI.

**compile_with_meter(frame_count, Some((tap_indices, meter_buffer)))** — After each `process()` call, **CompiledGraph** computes the peak of each tapped scratch buffer and writes it to the corresponding MeterBuffer slot.

## Devices

The CPAL and stream lifecycle stay inside the crate. The application is responsible for choosing the device and keeping the Stream alive for as long as input should be captured.

- **input_device_list(host)** → `Vec<InputDeviceInfo>` (index, name). Use index with open_input_stream.

- **open_input_stream(host, device_index, buffer)** — Opens that input device, F32, low-latency config; callback writes first channel into the given **InputSampleBuffer**.
