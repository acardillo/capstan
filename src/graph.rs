//! Graph types: node identity, AudioGraph (control-thread), and CompiledGraph.

use std::collections::VecDeque;

use crate::audio_buffer::AudioBuffer;
use crate::nodes::{GainProcessor, Mixer, SineGenerator};
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
}

impl Processor for GraphNode {
    fn process(&mut self, inputs: &[&[f32]], output: &mut [f32]) {
        match self {
            GraphNode::Sine(s) => s.process(inputs, output),
            GraphNode::Gain(g) => g.process(inputs, output),
            GraphNode::Mixer(m) => m.process(inputs, output),
        }
    }
}

/// Audio graph: adjacency list + node storage. Lives only on the control thread.
/// Nodes are stored in a Vec; NodeId is the index. Edges go from node A to node B (A feeds B).
pub struct AudioGraph {
    /// nodes[id.as_usize()] is the node for that id.
    nodes: Vec<GraphNode>,
    /// adjacency[id.as_usize()] is the list of node ids that this node's output feeds into.
    adjacency: Vec<Vec<NodeId>>,
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
    /// Returns `Err(())` if the graph contains a cycle.
    pub fn topological_sort(&self) -> Result<Vec<NodeId>, ()> {
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
            return Err(()); // cycle: some nodes never got in_degree 0
        }
        Ok(order)
    }

    /// Builds a CompiledGraph: topo-sorted nodes, one scratch buffer per node, and input indices per node.
    pub fn compile(&self, frame_count: usize) -> Result<CompiledGraph, ()> {
        let order = self.topological_sort()?;
        let n = order.len();
        let nodes: Vec<GraphNode> = order
            .iter()
            .map(|&id| self.nodes[id.as_usize()].clone())
            .collect();
        let scratch_buffers: Vec<AudioBuffer> = (0..n).map(|_| AudioBuffer::new(frame_count)).collect();
        let input_buf_indices: Vec<Vec<usize>> = (0..n)
            .map(|i| {
                (0..n)
                    .filter(|&j| self.adjacency[order[j].as_usize()].contains(&order[i]))
                    .collect()
            })
            .collect();
        Ok(CompiledGraph {
            nodes,
            scratch_buffers,
            input_buf_indices,
        })
    }
}

/// Immutable execution plan: nodes in topo order, one scratch buffer per node, and per-node input indices.
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledGraph {
    nodes: Vec<GraphNode>,
    scratch_buffers: Vec<AudioBuffer>,
    /// input_buf_indices[i] = buffer indices (0..i) that are inputs to node i.
    input_buf_indices: Vec<Vec<usize>>,
}

impl CompiledGraph {
    /// Runs the graph: each node reads from its input buffers and writes to its scratch; last node's buffer is copied to output.
    pub fn process(&mut self, output: &mut [f32]) {
        let n = self.nodes.len();
        if n == 0 {
            return;
        }
        for i in 0..n {
            let (head, tail) = self.scratch_buffers.split_at_mut(i);
            let out_buf = &mut tail[0];
            let input_slices: Vec<&[f32]> = self.input_buf_indices[i]
                .iter()
                .map(|&j| head[j].as_slice())
                .collect();
            self.nodes[i].process(&input_slices, out_buf.as_mut_slice());
        }
        let last = self.scratch_buffers.len() - 1;
        let len = self.scratch_buffers[last].len().min(output.len());
        output[..len].copy_from_slice(self.scratch_buffers[last].as_slice());
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
        assert!(max_abs > 0.0 && max_abs <= 0.26, "sine then gain 0.25 => amplitude ~0.25");
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
        assert!(max_abs > 0.0 && max_abs <= 1.1, "two sines mixed at 0.5 each => sum amplitude <= 1");
    }
}
