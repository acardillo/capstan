# Design Decisions

This document explains the design decisions/rationale behind the architecture.

## Two threads (control vs audio)

**Decision:** All graph editing, file loading, and UI run on the **control thread**. The **audio thread** only drains commands and runs the compiled graph in the output callback.

**Why:** The audio callback has hard real-time constraints: it must produce a block of samples within a few milliseconds (~2.7 ms @ 48 kHz). If the audio callback blocks the driver can underrun and produce glitches or dropouts. Building graphs, loading WAVs, and handling user input are unpredictable in time and can block. The audio thread only does bounded, deterministic work: draining commands and running the current graph. The control thread is free to do slow or blocking work; it communicates with the audio thread only via messages.

**Tradeoff:** Graph changes take effect on the next callback (one block of latency). That’s acceptable for most interactive applications.

## Lock-free messaging

**Decision:** Command channels are **lock-free SPSC ring buffers**. The audio thread only uses `try_recv` and `try_send` to communicate with the control thread, never blocking.

**Why:** If the audio thread called a blocking `recv()` (e.g. a mutex channel) instead, and the control thread was slow, the callback could block and stall the audio thread. With a lock-free ring buffer and non-blocking try operations, the worst cases are commands being applied one cycle late or the buffer being full (this is unlikely given the buffer size). We _never_ stall the audio thread.

**Tradeoff:** We don’t provide any blocking capabilities on the audio thread. Logic cannot wait until a command is applied. Events must be used instead to achieve the desired semantics.

## Compiled graph (immutable execution plan)

**Decision:** The control thread builds a mutable, directed acyclic graph (**AudioGraph**). When the user is done editing, we _compile_ it into an immutable, topologically sorted node list (**CompiledGraph**). Memory for compiled graphs is allocated on the control thread. The audio thread only interacts with the compiled graph, modifying the signal path by transferring ownership of the new graph to the audio thread and returning the old graph to the control thread for disposal.

**Why:** If the audio thread used a shared mutable graph, we would need synchronization via locks or a higher complexity lock-free structure. By compiling on the control thread, we produce an immutable snapshot containing a fixed order of nodes and pre-allocated buffers. The audio thread can then process audio in a simple loop over the compiled graph with no allocation, shared state, or locks. The only cross-thread operation needed is ownership transfer of compiled graphs over the command channel. The cost of compilation is paid once per edit on the control thread, not every callback.

**Tradeoff:** Compiling the graph can be costly for large and frequently changing graphs. This design works well for most applications where edits are infrequent relative to callback rate. Reconsider only if you need to change the graph at near-callback rates (e.g. many times per second) or if graphs grow very large.

## Pull-based file playback

**Decision:** File playback uses a **pull-based** model: the entire WAV is loaded into memory and is resampled to the output rate. The audio callback reads from that buffer via an atomic read position eliminating the need for a separate feeder thread.

**Why:** We initially used a **push-based** design with a feeder thread that decoded, resampled the file and wrote chunks into a ring buffer that the audio thread read from. Scheduling jitter meant the feeder and the callback often got out of sync manifesting as crackle and drift in the audio. This was fixed by switching to a simpler **pull-based** model. The whole file is loaded once on the control thread, then the audio thread only reads the next block each callback. No second thread, no rate matching, no producer/consumer coordination.

**Tradeoff:** Memory holds the full decoded file. For long or many simultaneous files that can add up. Future work could explore a more memory-efficient approach such as streaming the file or using a more efficient resampling algorithm. For now, this design is simple and avoids the fragility of a feeder thread in favor of the real-time path.
