# Design decisions

Short rationale for the main architectural choices. See [ARCHITECTURE.md](ARCHITECTURE.md) for how the system is built; this document explains why.

---

## Two threads (control vs audio)

**Decision:** All graph editing, file loading, and UI run on the **control thread**. The **audio thread** only drains commands and runs the compiled graph in the output callback.

**Why:** The audio callback has hard real-time constraints: it must produce a block of samples within a few milliseconds (e.g. ~2.7 ms at 128 frames @ 48 kHz). If the callback blocks on I/O, allocation, or a lock held by another thread, the driver can underrun and produce glitches or dropouts. Building graphs, loading WAVs, and handling user input are unpredictable in time and can block. Keeping them off the audio thread ensures the callback only does bounded, deterministic work: drain commands (lock-free), then run the current graph (fixed topology and buffers). The control thread is free to do slow or blocking work; it communicates with the audio thread only via messages.

**Tradeoff:** Graph changes take effect on the next callback (one block of latency). That’s acceptable for interactive use; we avoid any design that would let the callback wait on the control thread.

---

## Lock-free messaging

**Decision:** Command and event channels are **lock-free SPSC ring buffers** (atomics, no mutexes). The audio thread only uses `try_recv` / `try_send`; it never blocks waiting for the control thread.

**Why:** If the audio thread called a blocking `recv()` on a mutex-based channel, and the control thread was slow or preempted, the callback could block and miss its deadline. With a lock-free ring buffer and non-blocking try operations, the worst case is: the audio thread doesn’t see a command this callback and applies it next time, or the control thread gets “buffer full” when sending. We never stall the audio thread. Same for events: the control thread polls with `try_recv`; if the buffer is full, the audio thread drops the send (or we size the buffer so that’s rare). Real-time systems avoid locks in the hot path; lock-free SPSC is a standard way to pass data between a real-time thread and the rest of the app.

**Tradeoff:** We don’t provide “wait until the audio thread has applied this command” semantics; the API is fire-and-forget. Callers that need confirmation use events (e.g. `GraphSwapped`) and poll.

---

## Compiled graph (immutable execution plan)

**Decision:** The control thread builds a mutable **AudioGraph** (nodes, edges). When the user is done editing, we **compile** it into a **CompiledGraph**: topologically sorted node list, one preallocated scratch buffer per node, no allocation in the audio path. The audio thread only ever sees a `CompiledGraph`; swapping is replacing the current one with a new one (and receiving the old one back via an event for disposal).

**Why:** If the audio thread walked a shared, mutable graph, we’d need synchronization (locks or tricky lock-free structures) and the callback could still see an inconsistent topology mid-edit. By compiling on the control thread, we produce an immutable snapshot: a fixed order of nodes and fixed buffers. The audio thread’s `process()` is a simple loop over that list; no allocation, no shared mutable state, no locks. Graph edits happen on the control thread; the only cross-thread operation is “swap in this new compiled graph,” which is a pointer swap and safe. The cost of compilation (topo sort, buffer allocation) is paid once per edit, not every callback.

**Tradeoff:** Compilation has a cost (clone nodes, allocate scratch buffers). We assume edits are infrequent relative to callback rate. For very large or very frequently changing graphs, you’d need to measure; the design favors simplicity and real-time safety over zero-cost edits.

---

## Pull-based file playback

**Decision:** File playback uses a **pull-based** model: the entire WAV is loaded into memory (mono, resampled to output rate). The audio callback reads from that buffer via an atomic read position. No separate “feeder” thread.

**Why:** We initially used a **push-based** design: a background thread decoded/resampled the file and wrote chunks into a ring buffer that the audio callback read from. In practice, scheduling jitter meant the feeder and the callback often got out of sync—buffer underruns (silence or repeats) or overruns (drops), which showed up as crackle and drift. Fixing that required ever more tuning (buffer sizes, sleep timing). Switching to **pull-based**: load the whole file once on the control thread, then the audio thread only reads the next block each callback. No second thread, no rate matching, no producer/consumer coordination. The callback just advances an atomic index; worst case we reuse the same block if something is slow, but we never block.

**Tradeoff:** Memory holds the full decoded file. For long or many simultaneous files that can be significant; for typical clips and a small number of tracks it’s acceptable. We avoid the complexity and fragility of a feeder thread in the real-time path.
