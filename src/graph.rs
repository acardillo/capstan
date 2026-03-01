//! Graph types: node identity and (later) AudioGraph, CompiledGraph.

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

#[cfg(test)]
mod tests {
    use super::NodeId;

    #[test]
    /// Test that the node id can be created and converted back to a usize.
    fn test_node_id_roundtrip() {
        for n in 0..10 {
            assert_eq!(NodeId::new(n).as_usize(), n);
        }
    }

    #[test]
    /// Test that the node id is equal to itself and not equal to another node id.
    fn test_node_id_equality() {
        assert_eq!(NodeId::new(0), NodeId::new(0));
        assert_ne!(NodeId::new(0), NodeId::new(1));
    }
}
