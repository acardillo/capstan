# Architecture: Graph, Engine, and Buffer

This doc explains how the main pieces fit together. See `PLAN.md` for the high-level control-thread / audio-thread diagram and the phased task list.

---

## Where each type lives

| Type | Thread | Role |
|------|--------|------|
| **AudioGraph** | Control | Editable patch: which nodes exist and how they're wired (adjacency list). You add/remove nodes and edges here. |
| **CompiledGraph** | Built on control, then used on audio | Immutable execution plan: nodes in topological order plus pre-allocated scratch buffers. Sent to the audio thread via a Command. |
| **Engine** | Audio | Runs each callback: drain commands, then execute the current CompiledGraph (or a fixed chain until that exists). |
| **AudioBuffer** | Audio (allocated when building CompiledGraph) | Fixed-size f32 scratch arrays. Allocated once, reused every callback to pass data between nodes. |

---

## How they connect

```
Control thread:
  AudioGraph (nodes + edges)
       │
       ▼  compile: topological sort + allocate buffers
  CompiledGraph (ordered nodes + scratch AudioBuffers)
       │
       │  Command: "Swap in this CompiledGraph"
       ▼
  ──────── Lock-free channel (Commands / Events) ────────
       │
       ▼
Audio thread (each callback):
  Engine
    ├── drain commands (e.g. "swap graph", SetGain, Quit)
    ├── if quit → fill output with silence
    └── else run CompiledGraph:
          for each node in order:
            use AudioBuffers as scratch; last node writes to callback output
```

---

## Summary

- **Graph** = the editable patch on the control thread (what the patch is).
- **CompiledGraph** = the runnable form of that patch (how to execute it; built on control, used on audio).
- **Engine** = the runner on the audio thread (drains commands, then runs the current graph or chain).
- **Buffer** = the reusable f32 arrays used when executing the graph (and the final output slice from cpal).
