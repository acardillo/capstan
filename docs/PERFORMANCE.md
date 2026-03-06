# Performance and Limits

This document describes practical limits and real-time constraints so you can size graphs and buffers correctly and avoid glitches.

## Audio thread: no allocation, no locks

The **audio thread** (the output callback) must never allocate or block. All of the following are forbidden there:

- **No heap allocation** — no `Vec::push`, `Box::new`, `String`, or other allocation. The callback uses only pre-allocated buffers (e.g. `CompiledGraph` scratch buffers, the engine's mono buffer).
- **No locks** — no `Mutex`, `RwLock`, or blocking channels. The audio thread only uses lock-free structures: `try_recv` / `try_send` on the command/event ring buffers, and atomics in meter/record/input buffers.
- **No I/O** — no file or network access. File playback is done by reading from an already-loaded in-memory buffer.

All graph building, compilation, file loading, and UI work happen on the **control thread**. The audio thread only drains commands and runs the current `CompiledGraph`. If you add custom nodes or use the library in a custom stream, keep the same rules in the callback.

## Callback size (frame count)

- The callback is invoked once per **buffer** of samples. At 48 kHz, 128 frames ≈ 2.7 ms, 256 ≈ 5.3 ms.
- The library requests a low-latency buffer size (128 frames) when building the stream; the host/driver may impose a larger minimum.
- **Compile with the same frame count** you use at runtime: `graph.compile(frame_count)`. The compiled graph allocates one scratch buffer of `frame_count` f32 samples per node. Typical values are 128–4096; use the actual callback frame size when possible to avoid wasted memory and to keep phase/timing correct.
- If the host gives a larger callback than your compiled graph's buffer length, the graph's `process()` only fills up to its buffer size and zero-fills the rest; avoid large mismatches.

## Graph size

- **Compilation** is O(nodes + edges): topological sort plus one allocation per node (scratch buffer) and per-node input index lists. Compilation runs on the control thread, so it can take a few milliseconds for large graphs without affecting audio.
- There is no hard limit on node count. For interactive use (dozens to low hundreds of nodes), compilation cost is negligible. If you compile very frequently (e.g. many times per second) or build graphs with thousands of nodes, consider profiling; the main cost is allocation and topo sort.
- **Runtime (audio thread)** is O(nodes × frame_count) per callback: one `process()` call per node, each over `frame_count` samples. Keep the product (nodes × frames) small enough that the callback finishes well within the buffer's time budget (e.g. under 2 ms for a 128-frame @ 48 kHz buffer).

## Ring buffer capacities

- **Command / event channels:** Size so that bursts of commands (e.g. many `SetGain` or a few `SwapGraph`) don't fill the buffer. A capacity of 64–256 is usually enough; the audio thread drains every callback.
- **Input sample buffer (device → graph):** Must absorb timing jitter between input and output callbacks. Capacity in samples should be at least a few times the larger of input and output block sizes (e.g. 2048–4096 for 128–512 frame callbacks).
- **Record buffer:** Default capacity is ~5.5 minutes at 48 kHz. When full, oldest samples are dropped.

## Summary

| Concern                | Recommendation                                                                           |
| ---------------------- | ---------------------------------------------------------------------------------------- |
| Audio thread           | No alloc, no locks, no I/O. Only lock-free and pre-allocated data.                       |
| Callback / frame count | Match compile `frame_count` to actual callback size (e.g. 128–4096).                     |
| Graph size             | Fine for dozens–hundreds of nodes; profile if you compile very often or use 1000+ nodes. |
| Command/event capacity | 64–256 typically sufficient.                                                             |
| Input ring capacity    | 2048–4096 samples for typical block sizes.                                               |
