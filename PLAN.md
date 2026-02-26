# Rust Real-Time Audio Engine — Project Plan

## Goal

Build a real-time audio processing engine in Rust. The engine manages a graph of audio nodes (sources, processors, sinks), processes audio buffers on a dedicated audio thread, and allows the control thread to modify the graph safely without ever blocking audio output.

The project demonstrates: lock-free concurrency, Rust's ownership model under real-time constraints, DSP fundamentals, and optional WASM portability.

---

## Architecture Overview

```
┌─────────────────────────────────┐
│  Control Thread                 │
│  CLI / config / graph mutations │
└────────────────┬────────────────┘
                 │
         Lock-Free Channel
         (SPSC ring buffer)
         Commands ──►
         ◄── Events
                 │
┌────────────────▼────────────────┐
│  Audio Thread (cpal callback)   │
│  Compiled graph execution       │
│  No allocation. No locks.       │
└─────────────────────────────────┘
```

**Core types:**
- `AudioBuffer` — fixed f32 sample array, allocated once, reused forever
- `Processor` trait — interface every audio node implements
- `AudioGraph` — node/edge structure, lives on control thread
- `CompiledGraph` — sorted execution plan sent to audio thread
- `Command` / `Event` — message types crossing the thread boundary

---

## Phase 1 — Make a Sound

Goal: get audio output working and validate core types.

- [ ] Set up Rust project, add `cpal` dependency
- [ ] Open an output stream with `cpal`, output silence
- [ ] Implement `AudioBuffer` — fixed size, no post-construction allocation
- [ ] Write `AudioBuffer` unit tests
- [ ] Define the `Processor` trait
- [ ] Implement `SineGenerator` node
- [ ] Implement `GainProcessor` node
- [ ] Hardcode a chain: `SineGenerator → GainProcessor → cpal output`
- [ ] Hear a tone

---

## Phase 2 — Lock-Free Channel

Goal: build the communication layer between control and audio threads.

- [ ] Implement SPSC ring buffer using atomics (`AtomicUsize`, `MaybeUninit`)
- [ ] Document every `unsafe` block with invariant explanation
- [ ] Define `Command` enum (no heap allocation)
- [ ] Define `Event` enum (no heap allocation)
- [ ] Wrap ring buffer into `CommandSender` / `CommandReceiver` types
- [ ] Wrap ring buffer into `EventSender` / `EventReceiver` types
- [ ] Enforce thread ownership in types — audio thread handle must be `!Send`
- [ ] Drain commands at the top of each audio callback
- [ ] Write tests for ring buffer correctness

---

## Phase 3 — Audio Graph

Goal: replace the hardcoded node chain with a real graph.

- [ ] Define `NodeId` newtype
- [ ] Build `AudioGraph` — adjacency list + node storage, control thread only
- [ ] Implement topological sort (Kahn's algorithm)
- [ ] Define `CompiledGraph` — sorted node list + pre-allocated scratch buffers
- [ ] Implement atomic graph swap via `Arc<CompiledGraph>`
- [ ] Return old graph to control thread via Event for deallocation
- [ ] Execute compiled graph in audio callback

---

## Phase 4 — DSP Nodes & CLI

Goal: build useful nodes and make the engine interactive.

- [ ] Implement `DelayLine` node — circular buffer, millisecond delay time
- [ ] Write `DelayLine` impulse test
- [ ] Implement `BiquadFilter` node — lowpass and highpass (Audio EQ Cookbook)
- [ ] Implement `Mixer` node — N inputs, per-input gain
- [ ] Build CLI with `clap` — construct graph from args or config
- [ ] Support runtime parameter changes via stdin commands

---

## Phase 5 — WASM (Optional)

Goal: compile the engine to WebAssembly and run it in a browser.

- [ ] Feature-flag `cpal` and native I/O behind `native` feature
- [ ] Confirm core crate compiles to `wasm32-unknown-unknown`
- [ ] Write JavaScript `AudioWorkletProcessor` wrapper
- [ ] Expose `process_block(output_ptr, frames)` to JS
- [ ] Build minimal HTML demo page — start button, gain slider

---

## Change Log

_Update this section when the plan changes due to new decisions or scope shifts._

| Date | Change |
|------|--------|
| —    | Initial plan created |
