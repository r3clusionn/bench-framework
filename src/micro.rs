//! The built-in CPU, memory and cache microbenchmarks.
//!
//! Each one is described by a name and a builder, so the large buffers of the memory benchmarks
//! are only allocated for the benchmarks a run actually selects.

use std::hint::black_box;

use crate::runner::Throughput;

pub struct Micro {
    pub name: String,
    pub description: &'static str,
    pub throughput: Throughput,
    pub build: Box<dyn Fn() -> Box<dyn FnMut() -> u64>>,
}

const KIB: usize = 1024;
const MIB: usize = 1024 * 1024;

fn size_name(bytes: usize) -> String {
    if bytes >= MIB {
        format!("{}MiB", bytes / MIB)
    } else {
        format!("{}KiB", bytes / KIB)
    }
}

/// Rounds of dependent work per call of the CPU benchmarks.
const ROUNDS: u64 = 1024;

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

#[repr(align(64))]
#[derive(Clone, Copy)]
struct Line {
    next: usize,
}

/// A single random cycle through `lines` cache lines (Sattolo's shuffle), so every load depends on
/// the previous one and the prefetcher cannot predict the next address.
fn chase_ring(lines: usize) -> Vec<Line> {
    let mut order: Vec<usize> = (0..lines).collect();
    let mut rng = 0x1234_5678_9ABC_DEF1u64;
    for i in (1..lines).rev() {
        let j = (xorshift(&mut rng) % i as u64) as usize;
        order.swap(i, j);
    }
    let mut ring = vec![Line { next: 0 }; lines];
    for w in 0..lines {
        ring[order[w]].next = order[(w + 1) % lines];
    }
    ring
}

pub fn all() -> Vec<Micro> {
    let mut v: Vec<Micro> = Vec::new();

    v.push(Micro {
        name: "cpu/int-latency".into(),
        description: "1024 dependent multiply, shift and xor steps; per op is the latency of one step",
        throughput: Throughput::Elements(ROUNDS),
        build: Box::new(|| {
            let (k, mut x) = (black_box(0x9E37_79B9_7F4A_7C15u64), black_box(1u64));
            Box::new(move || {
                for _ in 0..ROUNDS {
                    x = x.wrapping_mul(k) ^ (x >> 29);
                }
                x
            })
        }),
    });
    v.push(Micro {
        name: "cpu/int-throughput".into(),
        description: "the same step on 8 independent chains; per op shows how many run at once",
        throughput: Throughput::Elements(ROUNDS),
        build: Box::new(|| {
            let k = black_box(0x9E37_79B9_7F4A_7C15u64);
            let mut x = [1u64, 2, 3, 4, 5, 6, 7, 8].map(black_box);
            Box::new(move || {
                for _ in 0..ROUNDS / 8 {
                    for c in x.iter_mut() {
                        *c = c.wrapping_mul(k) ^ (*c >> 29);
                    }
                }
                x.iter().fold(0, |a, b| a ^ b)
            })
        }),
    });
    v.push(Micro {
        name: "cpu/fp-latency".into(),
        description: "1024 dependent f64 multiply-add steps",
        throughput: Throughput::Elements(ROUNDS),
        build: Box::new(|| {
            let (a, b, mut x) = (black_box(0.999_999f64), black_box(1e-9f64), black_box(1.0f64));
            Box::new(move || {
                for _ in 0..ROUNDS {
                    x = x * a + b;
                }
                x.to_bits()
            })
        }),
    });
    v.push(Micro {
        name: "cpu/fp-throughput".into(),
        description: "the same step on 8 independent chains",
        throughput: Throughput::Elements(ROUNDS),
        build: Box::new(|| {
            let (a, b) = (black_box(0.999_999f64), black_box(1e-9f64));
            let mut x = [1.0f64, 1.1, 1.2, 1.3, 1.4, 1.5, 1.6, 1.7].map(black_box);
            Box::new(move || {
                for _ in 0..ROUNDS / 8 {
                    for c in x.iter_mut() {
                        *c = *c * a + b;
                    }
                }
                x.iter().fold(0u64, |s, c| s ^ c.to_bits())
            })
        }),
    });

    for &size in &[32 * KIB, MIB, 32 * MIB, 256 * MIB] {
        v.push(Micro {
            name: format!("mem/read/{}", size_name(size)),
            description: "sequential sum of a buffer of u64",
            throughput: Throughput::Bytes(size as u64),
            build: Box::new(move || {
                let buf: Vec<u64> = (0..size / 8).map(|i| i as u64).collect();
                Box::new(move || {
                    let mut acc = [0u64; 4];
                    for c in buf.as_chunks::<4>().0 {
                        for i in 0..4 {
                            acc[i] = acc[i].wrapping_add(c[i]);
                        }
                    }
                    acc[0] ^ acc[1] ^ acc[2] ^ acc[3]
                })
            }),
        });
    }
    for &size in &[32 * KIB, MIB, 32 * MIB, 256 * MIB] {
        v.push(Micro {
            name: format!("mem/write/{}", size_name(size)),
            description: "sequential fill of a buffer",
            throughput: Throughput::Bytes(size as u64),
            build: Box::new(move || {
                let mut buf = vec![0u8; size];
                let mut v = 0u8;
                Box::new(move || {
                    v = v.wrapping_add(1);
                    buf.fill(black_box(v));
                    buf[size / 2] as u64
                })
            }),
        });
    }
    for &size in &[MIB, 256 * MIB] {
        v.push(Micro {
            name: format!("mem/copy/{}", size_name(size)),
            description: "copy between two buffers; the rate counts bytes copied, not read plus written",
            throughput: Throughput::Bytes(size as u64),
            build: Box::new(move || {
                let src: Vec<u8> = (0..size).map(|i| i as u8).collect();
                let mut dst = vec![0u8; size];
                Box::new(move || {
                    dst.copy_from_slice(black_box(&src));
                    dst[size / 3] as u64
                })
            }),
        });
    }

    for &size in &[4 * KIB, 16 * KIB, 32 * KIB, 64 * KIB, 256 * KIB, MIB, 4 * MIB, 16 * MIB, 64 * MIB, 256 * MIB] {
        v.push(Micro {
            name: format!("cache/chase/{}", size_name(size)),
            description: "dependent random loads through a working set; per op is the load latency",
            throughput: Throughput::Elements(1000),
            build: Box::new(move || {
                let ring = chase_ring(size / 64);
                let mut p = 0usize;
                Box::new(move || {
                    for _ in 0..1000 {
                        p = ring[p].next;
                    }
                    p as u64
                })
            }),
        });
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chase_ring_is_one_cycle_through_every_line() {
        for lines in [2usize, 7, 64, 1000] {
            let ring = chase_ring(lines);
            let mut seen = vec![false; lines];
            let mut p = 0;
            for _ in 0..lines {
                assert!(!seen[p], "revisited {p} before covering all {lines} lines");
                seen[p] = true;
                p = ring[p].next;
            }
            assert_eq!(p, 0, "the walk must come back to the start after {lines} steps");
            assert!(seen.iter().all(|&s| s));
        }
    }

    #[test]
    fn names_are_unique_and_grouped() {
        let all = all();
        let mut names: Vec<&str> = all.iter().map(|m| m.name.as_str()).collect();
        let n = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), n);
        assert!(all.iter().all(|m| m.name.split('/').count() >= 2));
    }
}
