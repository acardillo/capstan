//! Benchmarks for graph compilation and ring buffer throughput.
//!
//! Run with: `cargo bench`

use capstan::graph::{AudioGraph, GraphNode};
use capstan::nodes::{GainProcessor, SineGenerator};
use capstan::ring_buffer::RingBuffer;
use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};

const FRAME_COUNT: usize = 128;
const SAMPLE_RATE: u32 = 48_000;

/// Builds a linear chain: sine -> gain -> gain -> ... -> gain (1 + n nodes).
fn linear_chain(node_count: usize) -> AudioGraph {
    let mut g = AudioGraph::new();
    if node_count == 0 {
        return g;
    }
    let mut prev = g.add_node(GraphNode::Sine(SineGenerator::new(440.0, SAMPLE_RATE)));
    for _ in 1..node_count {
        let gain = g.add_node(GraphNode::Gain(GainProcessor::new(0.5)));
        g.add_edge(prev, gain);
        prev = gain;
    }
    g
}

/// Benchmarks graph compilation for linear chains of varying size.
fn bench_compile(c: &mut Criterion) {
    let mut group = c.benchmark_group("compile");
    for size in [10, 50, 100, 200] {
        let graph = linear_chain(size);
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(
            criterion::BenchmarkId::new("linear_chain", size),
            &graph,
            |b, g| {
                b.iter(|| g.compile(black_box(FRAME_COUNT)).unwrap());
            },
        );
    }
    group.finish();
}

/// Benchmarks ring buffer send/recv throughput (pairs per iteration).
fn bench_ring_buffer(c: &mut Criterion) {
    let mut group = c.benchmark_group("ring_buffer");
    for capacity in [64, 256, 1024] {
        group.throughput(Throughput::Elements(capacity as u64));
        group.bench_with_input(
            criterion::BenchmarkId::new("try_send_try_recv", capacity),
            &capacity,
            |b, &cap| {
                let ring: RingBuffer<u64> = RingBuffer::new(cap);
                b.iter(|| {
                    for i in 0..cap {
                        let _ = ring.try_send(black_box(i as u64));
                    }
                    for _ in 0..cap {
                        let _ = ring.try_recv();
                    }
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_compile, bench_ring_buffer);
criterion_main!(benches);
