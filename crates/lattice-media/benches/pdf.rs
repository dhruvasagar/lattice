//! PF.1: the cost of opening a PDF and of drawing one of its pages.
//!
//! Recorded in `docs/dev/operations/benchmarks.md`. The page is text-dense
//! (60 lines of Helvetica, which hayro supplies from its embedded standard
//! fonts) because text is what most pages are; PF.0's corpus run is where the
//! figure-heavy worst case was measured.
//!
//! `render_*_1x` / `render_*_2x` are the two passes of pdf-mode §3: the soft
//! page that appears at once and the sharp one that replaces it.

use criterion::{Criterion, criterion_group, criterion_main};
use lattice_media::PixelFormat;
use lattice_media::pdf::PdfDocument;

/// A `pages`-page US-letter PDF of text, with computed xref offsets.
fn text_pdf(pages: usize) -> Vec<u8> {
    let mut content = String::from("BT /F1 10 Tf 12 TL 72 740 Td\n");
    for i in 0..60 {
        content.push_str(&format!(
            "(Line {i}: the quick brown fox jumps over the lazy dog, again and again.) '\n"
        ));
    }
    content.push_str("ET");

    let mut objects: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        format!(
            "<< /Type /Pages /Kids [{}] /Count {pages} >>",
            (0..pages)
                .map(|i| format!("{} 0 R", 5 + i))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len() + 1
        ),
    ];
    for _ in 0..pages {
        objects.push(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 3 0 R >> >> /Contents 4 0 R >>"
                .into(),
        );
    }
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

fn bench_pdf(c: &mut Criterion) {
    let small = text_pdf(1);
    let large = text_pdf(500);
    let mut g = c.benchmark_group("pdf");

    g.bench_function("open_500_pages", |b| {
        b.iter(|| PdfDocument::from_bytes(large.clone()).expect("open"));
    });

    let doc = PdfDocument::from_bytes(small).expect("open");
    g.bench_function("render_text_page_1x_1600px", |b| {
        b.iter(|| {
            doc.render_page(0, (1600, 4000), PixelFormat::BgraPremultiplied8)
                .expect("render")
        });
    });
    g.bench_function("render_text_page_2x_3200px", |b| {
        b.iter(|| {
            doc.render_page(0, (3200, 8000), PixelFormat::BgraPremultiplied8)
                .expect("render")
        });
    });
    g.finish();
}

criterion_group!(benches, bench_pdf);
criterion_main!(benches);
