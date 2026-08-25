//! Cache-hit benchmark — SL-2.PERF.01.
//!
//! The renderer's tile/display-list cache must make a hit cheap enough that
//! scrolling never waits on a re-parse. This measures get-hit throughput on a
//! warm, budgeted LRU of a realistic viewer size (~32 pages × 16 tiles).

#![allow(missing_docs)]

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use selis_geom::Matrix;
use selis_pdf_engine::{CacheKey, LruCache};

/// Fill a cache with `pages × tiles` entries and measure `get` hit rate.
fn bench_cache_hit(c: &mut Criterion) {
    let pages = 32u32;
    let tiles_per_page = 16u32;
    let tile_bytes = 256 * 256 * 4u64; // a 256×256 RGBA tile

    let mut cache = LruCache::new(tile_bytes.saturating_mul(1024));
    for page in 0..pages {
        for _tile in 0..tiles_per_page {
            cache.insert(
                CacheKey::new(page, Matrix::IDENTITY, 1, 1),
                vec![0u8; 256 * 256 * 4],
                tile_bytes,
            );
        }
    }

    c.bench_function("lru_cache_hit", |b| {
        b.iter(|| {
            for page in 0..pages {
                let key = CacheKey::new(page, Matrix::IDENTITY, 1, 1);
                black_box(cache.get(&key));
            }
        });
    });

    c.bench_function("lru_cache_insert_evict", |b| {
        b.iter(|| {
            let mut small = LruCache::new(tile_bytes.saturating_mul(8));
            for i in 0..32u32 {
                small.insert(
                    CacheKey::new(i, Matrix::IDENTITY, 1, 1),
                    vec![0u8; 256 * 256 * 4],
                    tile_bytes,
                );
            }
        });
    });
}

criterion_group!(benches, bench_cache_hit);
criterion_main!(benches);
