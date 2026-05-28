//! D.0a bench: `virtual_row_layout_p99_us` (build the matrix
//! from N rows) and `display_slice_iter_p99_us` (interleave
//! N virtual rows with M document rows at viewport=80 across
//! representative scroll positions).
//!
//! Per `docs/dev/architecture/virtual-rows.md` and
//! `diff-system.md` §7, the design target is sub-frame at
//! 60Hz. CI-gate enforcement deferred to the first
//! production consumer slice (D.3 inline diff overlay).

use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use lattice_cells::{
	AnchorPosition, Cell, CellChunk, CellMatrix, CellRow, MatrixVersion, VirtualRow,
	VirtualRowMatrix, VirtualRowVersion,
};

fn build_matrix(lines: u32) -> CellMatrix {
	let rows: Vec<CellRow> = (0..lines)
		.map(|i| {
			let cells: Vec<Cell> = (0..80).map(|c| Cell::with_codepoint(b'a' as u32 + (c % 26))).collect();
			CellRow::new(cells, i, Vec::new())
		})
		.collect();
	let chunk = Arc::new(CellChunk::new(0, rows, MatrixVersion::ZERO));
	CellMatrix::whole_doc(chunk, lines)
}

fn build_virtual_rows(count: u32, source_line_count: u32) -> Vec<VirtualRow> {
	let stride = (source_line_count / count).max(1);
	(0..count)
		.map(|i| VirtualRow {
			anchor_line: (i * stride).min(source_line_count),
			position: if i % 2 == 0 {
				AnchorPosition::Above
			} else {
				AnchorPosition::Below
			},
			cells: Arc::from([] as [Cell; 0]),
			height: 1,
		})
		.collect()
}

fn bench_layout(c: &mut Criterion) {
	let mut group = c.benchmark_group("virtual_row_layout");
	for &n in &[100u32, 1_000, 10_000] {
		let rows = build_virtual_rows(n, 5_000);
		group.bench_with_input(
			BenchmarkId::new("build", n),
			&rows,
			|b, rows| {
				b.iter(|| {
					VirtualRowMatrix::build(
						black_box(rows.clone()),
						black_box(5_000),
						VirtualRowVersion(1),
					)
				});
			},
		);
	}
	group.finish();
}

fn bench_display_slice(c: &mut Criterion) {
	let mut group = c.benchmark_group("display_slice_iter");
	let matrix = build_matrix(5_000);

	for &n in &[0u32, 100, 1_000] {
		let v = if n == 0 {
			VirtualRowMatrix::empty()
		} else {
			VirtualRowMatrix::build(build_virtual_rows(n, 5_000), 5_000, VirtualRowVersion(1))
		};

		// Top-of-document viewport.
		group.bench_with_input(
			BenchmarkId::new("scroll_0_height_80", n),
			&(matrix.clone(), v.clone()),
			|b, (m, v)| {
				b.iter(|| {
					let ds = m.display_slice(black_box(0), black_box(80), black_box(v));
					let count = ds.iter().count();
					assert!(count <= 80);
					count
				});
			},
		);

		// Mid-document viewport (forces interleaver to skip).
		group.bench_with_input(
			BenchmarkId::new("scroll_2500_height_80", n),
			&(matrix.clone(), v.clone()),
			|b, (m, v)| {
				b.iter(|| {
					let ds = m.display_slice(black_box(2_500), black_box(80), black_box(v));
					ds.iter().count()
				});
			},
		);
	}
	group.finish();
}

criterion_group!(benches, bench_layout, bench_display_slice);
criterion_main!(benches);
