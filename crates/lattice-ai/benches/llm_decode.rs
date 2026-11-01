//! LL.4 -- stream decode throughput.
//!
//! The request task decodes every network chunk before the sink sees it, so the
//! decoder's per-byte cost bounds how fast a reply can reach a buffer. The
//! recorded fixtures are replayed many times over and fed in network-sized
//! (1 KiB) chunks, the shape `reqwest` delivers.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use lattice_ai::llm::{AnthropicProvider, OpenAiProvider, Provider};
use std::hint::black_box;

const ANTHROPIC: &str = include_str!("../tests/fixtures/llm/anthropic_text_thinking.sse");
const OPENAI: &str = include_str!("../tests/fixtures/llm/openai_text_reasoning.sse");
const CHUNK: usize = 1024;
const REPEATS: usize = 200;

fn decode(p: &dyn Provider, bytes: &[u8]) -> usize {
    let mut d = p.decoder();
    let mut n = 0;
    for chunk in bytes.chunks(CHUNK) {
        n += d.push(chunk).len();
    }
    n + d.finish().len()
}

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("llm_decode");
    for (name, fixture, provider) in [
        (
            "anthropic",
            ANTHROPIC,
            Box::new(AnthropicProvider::default()) as Box<dyn Provider>,
        ),
        (
            "openai",
            OPENAI,
            Box::new(OpenAiProvider::new("http://localhost/v1")),
        ),
    ] {
        let bytes = fixture.repeat(REPEATS).into_bytes();
        group.throughput(Throughput::Bytes(bytes.len() as u64));
        group.bench_function(name, |b| {
            b.iter(|| black_box(decode(provider.as_ref(), &bytes)))
        });
    }
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
