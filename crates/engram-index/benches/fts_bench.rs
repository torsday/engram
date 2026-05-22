//! BM25 search latency benchmark.
//!
//! Per `docs/design/10-performance-budgets.md`: BM25 query p95 < 10ms for 10K notes.
//!
//! Run with:
//!   cargo bench -p engram-index

use criterion::{criterion_group, criterion_main, Criterion};
use engram_index::fts::search_fts;
use engram_index::sqlite::Migrator;
use rusqlite::{params, Connection};

fn setup_10k_notes() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    // WAL mode + cache for better benchmark accuracy.
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA cache_size = -4000;")
        .unwrap();
    Migrator::new(&conn).apply_all().unwrap();

    // Insert 10K synthetic notes.
    let tx = conn.unchecked_transaction().unwrap();
    for i in 0..10_000usize {
        tx.execute(
            "INSERT INTO notes (id, path, title, note_type, content) VALUES (?1, ?2, ?3, 'evergreen', ?4)",
            params![
                format!("note-{i:05}"),
                format!("note-{i:05}.md"),
                format!("Note {:05}", i),
                format!(
                    "This is the body of note {}. It discusses concepts such as \
                     knowledge management, spaced repetition, atomic habits, and \
                     evergreen note-taking. Iteration {}.",
                    i, i
                ),
            ],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    conn
}

fn bench_bm25_single_term(c: &mut Criterion) {
    let conn = setup_10k_notes();

    c.bench_function("fts bm25 single term (10K notes)", |b| {
        b.iter(|| {
            let hits = search_fts(&conn, "spaced", 20).unwrap();
            criterion::black_box(hits);
        });
    });
}

fn bench_bm25_phrase(c: &mut Criterion) {
    let conn = setup_10k_notes();

    c.bench_function("fts bm25 phrase search (10K notes)", |b| {
        b.iter(|| {
            let hits = search_fts(&conn, "\"atomic habits\"", 20).unwrap();
            criterion::black_box(hits);
        });
    });
}

fn bench_bm25_prefix(c: &mut Criterion) {
    let conn = setup_10k_notes();

    c.bench_function("fts bm25 prefix search (10K notes)", |b| {
        b.iter(|| {
            let hits = search_fts(&conn, "know*", 20).unwrap();
            criterion::black_box(hits);
        });
    });
}

criterion_group!(
    benches,
    bench_bm25_single_term,
    bench_bm25_phrase,
    bench_bm25_prefix,
);
criterion_main!(benches);
