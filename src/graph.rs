//! Graph types: node identity, AudioGraph (control-thread), and CompiledGraph.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::audio_buffer::AudioBuffer;
use crate::meter::MeterBuffer;
use crate::nodes::{
    BiquadFilter, DelayLine, GainProcessor, InputNode, Mixer, RecordNode, SineGenerator,
};
use crate::processor::Processor;

/// Identifies a node in the audio graph. Newtype so we don't confuse node indices with other integers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(usize);

impl NodeId {
    /// Creates a node id from a raw index. Caller ensures the index is valid for the graph.
    pub fn new(id: usize) -> Self {
        NodeId(id)
    }

    /// Returns the raw index. Use when indexing into node storage or for debugging.
    pub fn as_usize(self) -> usize {
        self.0
    }
}

/// A single node in the graph: one of the supported processor types.
#[derive(Clone, Debug, PartialEq)]
pub enum GraphNode {
    Sine(SineGenerator),
    Gain(GainProcessor),
    Mixer(Mixer),
    Input(InputNode),
    Delay(DelayLine),
    Biquad(BiquadFilter),
    Record(RecordNode),
}

impl Processor for GraphNode {
    fn process(&mut self, inputs: &[&[f32]], output: &mut [f32]) {
        match self {
            GraphNode::Sine(s) => s.process(inputs, output),
            GraphNode::Gain(g) => g.process(inputs, output),
            GraphNode::Mixer(m) => m.process(inputs, output),
            GraphNode::Input(n) => n.process(inputs, output),
            GraphNode::Delay(d) => d.process(inputs, output),
            GraphNode::Biquad(b) => b.process(inputs, output),
            GraphNode::Record(r) => r.process(inputs, output),
        }
    }
}

/// Errors from graph operations (e.g. cycle detected, invalid meter config).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphError {
    /// The graph contains a cycle; topological sort is impossible.
    Cycle,
    /// Meter tap indices or buffer length is invalid.
    InvalidMeterTaps,
}

impl std::fmt::Display for GraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GraphError::Cycle => write!(f, "graph contains a cycle"),
            GraphError::InvalidMeterTaps => write!(f, "invalid meter tap configuration"),
        }
    }
}

impl std::error::Error for GraphError {}

/// Audio graph: adjacency list + node storage. Lives only on the control thread.
/// Nodes are stored in a Vec; NodeId is the index. Edges go from node A to node B (A feeds B).
pub struct AudioGraph {
    /// nodes[id.as_usize()] is the node for that id.
    nodes: Vec<GraphNode>,
    /// adjacency[id.as_usize()] is the list of node ids that this node's output feeds into.
    adjacency: Vec<Vec<NodeId>>,
}

impl Default for AudioGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioGraph {
    /// Creates an empty graph.
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            adjacency: Vec::new(),
        }
    }

    /// Adds a node and returns its id. The node is not connected to anything yet.
    pub fn add_node(&mut self, node: GraphNode) -> NodeId {
        self.nodes.push(node);
        self.adjacency.push(Vec::new());
        NodeId::new(self.nodes.len() - 1)
    }

    /// Adds an edge from `from` to `to` (output of `from` feeds into `to`). Panics if either id is out of range.
    pub fn add_edge(&mut self, from: NodeId, to: NodeId) {
        self.adjacency[from.as_usize()].push(to);
    }

    /// Returns the number of nodes.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Returns the successors of the given node (nodes this node's output feeds into).
    pub fn successors(&self, id: NodeId) -> &[NodeId] {
        &self.adjacency[id.as_usize()]
    }

    /// Returns nodes in topological order (Kahn's algorithm). Nodes with no incoming edges first.
    /// Returns `Err(GraphError::Cycle)` if the graph contains a cycle.
    pub fn topological_sort(&self) -> Result<Vec<NodeId>, GraphError> {
        let n = self.nodes.len();
        if n == 0 {
            return Ok(Vec::new());
        }
        // in_degree[i] = number of edges pointing to node i
        let mut in_degree: Vec<usize> = vec![0; n];
        for succ_list in &self.adjacency {
            for &succ in succ_list {
                let i = succ.as_usize();
                if i < n {
                    in_degree[i] += 1;
                }
            }
        }
        let mut queue: VecDeque<NodeId> = (0..n)
            .filter(|&i| in_degree[i] == 0)
            .map(NodeId::new)
            .collect();
        let mut order = Vec::with_capacity(n);
        while let Some(id) = queue.pop_front() {
            order.push(id);
            for &succ in self.successors(id) {
                let i = succ.as_usize();
                if i < n {
                    in_degree[i] -= 1;
                    if in_degree[i] == 0 {
                        queue.push_back(succ);
                    }
                }
            }
        }
        if order.len() != n {
            return Err(GraphError::Cycle); // cycle: some nodes never got in_degree 0
        }
        Ok(order)
    }

    /// Builds a CompiledGraph: topo-sorted nodes, one scratch buffer per node, and input indices per node.
    ///
    /// # Example
    ///
    /// Build a simple chain (sine → gain), compile it, and process a few blocks:
    ///
    /// ```
    /// use capstan::graph::{AudioGraph, GraphNode};
    /// use capstan::nodes::{GainProcessor, SineGenerator};
    ///
    /// let mut g = AudioGraph::new();
    /// let sine = g.add_node(GraphNode::Sine(SineGenerator::new(440.0, 48_000)));
    /// let gain = g.add_node(GraphNode::Gain(GainProcessor::new(0.5)));
    /// g.add_edge(sine, gain);
    ///
    /// let mut compiled = g.compile(64).unwrap();
    /// let mut output = vec![0.0f32; 64];
    /// compiled.process(&mut output);
    /// let peak = output.iter().map(|s| s.abs()).fold(0.0f32, |a, b| a.max(b));
    /// assert!(peak > 0.0 && peak <= 0.51);
    /// ```
    pub fn compile(&self, frame_count: usize) -> Result<CompiledGraph, GraphError> {
        self.compile_with_meter(frame_count, None)
    }

    /// Like [`compile`](Self::compile), but optionally wires meter taps: after each process call,
    /// the peak level of each specified scratch buffer (by index in topo order) is written to the
    /// shared [`MeterBuffer`]. Use for live level meters in a UI. `tap_indices` must have the same
    /// length as `meter_buffer.len()`, and each index must be in range `0..node_count`.
    pub fn compile_with_meter(
        &self,
        frame_count: usize,
        meter: Option<(Vec<usize>, Arc<MeterBuffer>)>,
    ) -> Result<CompiledGraph, GraphError> {
        let order = self.topological_sort()?;
        let n = order.len();
        if let Some((ref tap_indices, ref buf)) = meter {
            if tap_indices.len() != buf.len() {
                return Err(GraphError::InvalidMeterTaps);
            }
            for &idx in tap_indices {
                if idx >= n {
                    return Err(GraphError::InvalidMeterTaps);
                }
            }
        }
        let nodes: Vec<GraphNode> = order
            .iter()
            .map(|&id| self.nodes[id.as_usize()].clone())
            .collect();
        let scratch_buffers: Vec<AudioBuffer> =
            (0..n).map(|_| AudioBuffer::new(frame_count)).collect();
        let input_buf_indices: Vec<Vec<usize>> = (0..n)
            .map(|i| {
                (0..n)
                    .filter(|&j| self.adjacency[order[j].as_usize()].contains(&order[i]))
                    .collect()
            })
            .collect();
        let (tap_indices, meter_buffer) = meter
            .map(|(taps, buf)| (Some(taps), Some(buf)))
            .unwrap_or((None, None));
        Ok(CompiledGraph {
            nodes,
            scratch_buffers,
            input_buf_indices,
            tap_indices,
            meter_buffer,
        })
    }
}

/// Immutable execution plan: nodes in topo order, one scratch buffer per node, and per-node input indices.
/// Optionally holds meter taps: scratch buffer indices whose peak level is written to [`MeterBuffer`] each callback.
#[derive(Clone)]
pub struct CompiledGraph {
    nodes: Vec<GraphNode>,
    scratch_buffers: Vec<AudioBuffer>,
    /// input_buf_indices[i] = buffer indices (0..i) that are inputs to node i.
    input_buf_indices: Vec<Vec<usize>>,
    tap_indices: Option<Vec<usize>>,
    meter_buffer: Option<Arc<MeterBuffer>>,
}

impl std::fmt::Debug for CompiledGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledGraph")
            .field("node_count", &self.nodes.len())
            .field("tap_indices", &self.tap_indices)
            .finish_non_exhaustive()
    }
}

impl PartialEq for CompiledGraph {
    fn eq(&self, other: &Self) -> bool {
        self.nodes.len() == other.nodes.len()
            && self.tap_indices == other.tap_indices
            && match (&self.meter_buffer, &other.meter_buffer) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
    }
}

impl CompiledGraph {
    /// Runs the graph: each node reads from its input buffers and writes to its scratch; last node's buffer is copied to output.
    /// Only processes `output.len()` frames per call so generator phase and timing stay in sync with the device.
    pub fn process(&mut self, output: &mut [f32]) {
        let node_count = self.nodes.len();
        if node_count == 0 {
            return;
        }
        let out_len = output.len().min(self.scratch_buffers[0].len());
        if out_len == 0 {
            return;
        }
        for i in 0..node_count {
            let (head, tail) = self.scratch_buffers.split_at_mut(i);
            let out_buf = &mut tail[0];
            let input_slices: Vec<&[f32]> = self.input_buf_indices[i]
                .iter()
                .map(|&j| &head[j].as_slice()[..out_len])
                .collect();
            self.nodes[i].process(&input_slices, &mut out_buf.as_mut_slice()[..out_len]);
        }
        output[..out_len]
            .copy_from_slice(&self.scratch_buffers[node_count - 1].as_slice()[..out_len]);
        if output.len() > out_len {
            output[out_len..].fill(0.0);
        }

        if let (Some(ref tap_indices), Some(ref meter_buffer)) =
            (&self.tap_indices, &self.meter_buffer)
        {
            for (slot, &scratch_idx) in tap_indices.iter().enumerate() {
                if scratch_idx < self.scratch_buffers.len() {
                    let buf = &self.scratch_buffers[scratch_idx];
                    let slice = &buf.as_slice()[..out_len];
                    let peak = slice.iter().map(|&s| s.abs()).fold(0.0f32, |a, b| a.max(b));
                    meter_buffer.write_peak(slot, peak);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AudioGraph, GraphNode, NodeId};
    use crate::nodes::{GainProcessor, SineGenerator};

    #[test]
    fn test_node_id_roundtrip() {
        for n in 0..10 {
            assert_eq!(NodeId::new(n).as_usize(), n);
        }
    }

    #[test]
    fn test_node_id_equality() {
        assert_eq!(NodeId::new(0), NodeId::new(0));
        assert_ne!(NodeId::new(0), NodeId::new(1));
    }

    #[test]
    fn test_audio_graph_new_is_empty() {
        assert_eq!(AudioGraph::new().node_count(), 0);
    }

    #[test]
    fn test_audio_graph_add_node_returns_id_and_increases_count() {
        let mut g = AudioGraph::new();
        let sine = g.add_node(GraphNode::Sine(SineGenerator::new(440.0, 48_000)));
        let gain = g.add_node(GraphNode::Gain(GainProcessor::new(0.5)));
        assert_eq!(g.node_count(), 2);
        assert_eq!(sine, NodeId::new(0));
        assert_eq!(gain, NodeId::new(1));
    }

    #[test]
    fn test_audio_graph_add_edge_and_successors() {
        let mut g = AudioGraph::new();
        g.add_node(GraphNode::Sine(SineGenerator::new(440.0, 48_000)));
        g.add_node(GraphNode::Gain(GainProcessor::new(0.5)));
        g.add_edge(NodeId::new(0), NodeId::new(1));
        assert_eq!(g.successors(NodeId::new(0)), &[NodeId::new(1)]);
        assert_eq!(g.successors(NodeId::new(1)), &[] as &[NodeId]);
    }

    #[test]
    fn test_topological_sort_linear_chain() {
        let mut g = AudioGraph::new();
        g.add_node(GraphNode::Sine(SineGenerator::new(440.0, 48_000)));
        g.add_node(GraphNode::Gain(GainProcessor::new(0.5)));
        g.add_edge(NodeId::new(0), NodeId::new(1));
        let order = g.topological_sort().unwrap();
        assert_eq!(order, vec![NodeId::new(0), NodeId::new(1)]);
    }

    #[test]
    fn test_topological_sort_cycle_returns_err() {
        let mut g = AudioGraph::new();
        g.add_node(GraphNode::Sine(SineGenerator::new(440.0, 48_000)));
        g.add_node(GraphNode::Gain(GainProcessor::new(0.5)));
        g.add_edge(NodeId::new(0), NodeId::new(1));
        g.add_edge(NodeId::new(1), NodeId::new(0));
        assert!(g.topological_sort().is_err());
    }

    #[test]
    fn test_compiled_graph_process() {
        let mut g = AudioGraph::new();
        g.add_node(GraphNode::Sine(SineGenerator::new(440.0, 48_000)));
        g.add_node(GraphNode::Gain(GainProcessor::new(0.25)));
        g.add_edge(NodeId::new(0), NodeId::new(1));
        let mut compiled = g.compile(64).unwrap();
        let mut output = vec![0.0f32; 64];
        compiled.process(&mut output);
        let max_abs = output.iter().map(|s| s.abs()).fold(0.0f32, |a, b| a.max(b));
        assert!(
            max_abs > 0.0 && max_abs <= 0.26,
            "sine then gain 0.25 => amplitude ~0.25"
        );
    }

    #[test]
    fn test_compiled_graph_with_mixer() {
        use crate::nodes::Mixer;
        let mut g = AudioGraph::new();
        let s0 = g.add_node(GraphNode::Sine(SineGenerator::new(440.0, 48_000)));
        let s1 = g.add_node(GraphNode::Sine(SineGenerator::new(660.0, 48_000)));
        let mix = g.add_node(GraphNode::Mixer(Mixer::new(vec![0.5, 0.5])));
        g.add_edge(s0, mix);
        g.add_edge(s1, mix);
        let mut compiled = g.compile(64).unwrap();
        let mut output = vec![0.0f32; 64];
        compiled.process(&mut output);
        let max_abs = output.iter().map(|s| s.abs()).fold(0.0f32, |a, b| a.max(b));
        assert!(
            max_abs > 0.0 && max_abs <= 1.1,
            "two sines mixed at 0.5 each => sum amplitude <= 1"
        );
    }

    #[test]
    fn test_compiled_graph_with_input() {
        use crate::input_buffer::{InputSampleBuffer, SampleSource};
        use crate::nodes::InputNode;
        use std::sync::Arc;
        let buf = Arc::new(InputSampleBuffer::new(256));
        let buf_dyn: Arc<dyn SampleSource + Send + Sync> = Arc::clone(&buf) as _;
        let mut g = AudioGraph::new();
        let inp = g.add_node(GraphNode::Input(InputNode::new(buf_dyn)));
        let gain = g.add_node(GraphNode::Gain(GainProcessor::new(0.5)));
        g.add_edge(inp, gain);
        let mut compiled = g.compile(64).unwrap();
        let mut output = vec![1.0f32; 64];
        compiled.process(&mut output);
        assert!(
            output.iter().all(|&s| s == 0.0),
            "input underrun => silence"
        );
        buf.write_block(&[0.5f32; 64], 1);
        compiled.process(&mut output);
        assert!(
            output.iter().all(|&s| (s - 0.25).abs() < 1e-5),
            "input 0.5 * gain 0.5 => 0.25"
        );
    }

    #[test]
    fn test_compiled_graph_with_meter_taps() {
        use crate::meter::MeterBuffer;
        use std::sync::Arc;
        let mut g = AudioGraph::new();
        g.add_node(GraphNode::Sine(SineGenerator::new(440.0, 48_000)));
        g.add_node(GraphNode::Gain(GainProcessor::new(0.1)));
        g.add_edge(NodeId::new(0), NodeId::new(1));
        let meter = Arc::new(MeterBuffer::new(1));
        let tap_indices = vec![1];
        let mut compiled = g
            .compile_with_meter(64, Some((tap_indices, Arc::clone(&meter))))
            .unwrap();
        let mut output = vec![0.0f32; 64];
        compiled.process(&mut output);
        let peaks = meter.read_peaks();
        assert_eq!(peaks.len(), 1);
        assert!(
            peaks[0] > 0.0 && peaks[0] <= 0.11,
            "tap 1 = gain output, peak ~0.1"
        );
    }

    #[test]
    fn test_compiled_graph_with_record_node() {
        use crate::nodes::RecordNode;
        use crate::record::RecordBuffer;
        use std::sync::Arc;
        let record_buf = Arc::new(RecordBuffer::new());
        let mut g = AudioGraph::new();
        g.add_node(GraphNode::Sine(SineGenerator::new(440.0, 48_000)));
        g.add_node(GraphNode::Record(RecordNode::new(Arc::clone(&record_buf))));
        g.add_edge(NodeId::new(0), NodeId::new(1));
        let mut compiled = g.compile(64).unwrap();
        let mut output = vec![0.0f32; 64];
        record_buf.set_armed(true);
        compiled.process(&mut output);
        compiled.process(&mut output);
        record_buf.set_armed(false);
        let drained = record_buf.drain();
        assert_eq!(drained.len(), 128, "two blocks of 64");
        let max_abs = drained
            .iter()
            .map(|s| s.abs())
            .fold(0.0f32, |a, b| a.max(b));
        assert!(max_abs > 0.0 && max_abs <= 1.0, "recorded sine-like levels");
    }
}
