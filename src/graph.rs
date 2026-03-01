//! Graph types: node identity, AudioGraph (control-thread), and (later) CompiledGraph.

use crate::nodes::{GainProcessor, SineGenerator};
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
pub enum GraphNode {
    Sine(SineGenerator),
    Gain(GainProcessor),
}

impl Processor for GraphNode {
    fn process(&mut self, output: &mut [f32]) {
        match self {
            GraphNode::Sine(s) => s.process(output),
            GraphNode::Gain(g) => g.process(output),
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
}
