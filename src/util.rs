//! Crate-private helpers that don't belong to a PDF subsystem.

use std::thread;

/// Split `input` across a small `std::thread::scope` pool and collect results
/// in input order. Sequential when there is nothing to fan out.
pub(crate) fn parallel_map<T, R, F>(input: &[T], f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(usize, &T) -> R + Sync + Send,
{
    let len = input.len();
    if len == 0 {
        return Vec::new();
    }
    let workers = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .max(1)
        .min(len);
    if workers == 1 {
        return input
            .iter()
            .enumerate()
            .map(|(i, item)| f(i, item))
            .collect();
    }
    let chunk = (len + workers - 1) / workers;
    let mut out: Vec<Option<R>> = (0..len).map(|_| None).collect();
    thread::scope(|s| {
        let f = &f;
        for (chunk_i, (in_chunk, out_chunk)) in
            input.chunks(chunk).zip(out.chunks_mut(chunk)).enumerate()
        {
            s.spawn(move || {
                let base = chunk_i * chunk;
                for (offset, (slot, item)) in out_chunk.iter_mut().zip(in_chunk).enumerate() {
                    *slot = Some(f(base + offset, item));
                }
            });
        }
    });
    out.into_iter().map(Option::unwrap).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_map_empty_is_empty() {
        let input: [u8; 0] = [];
        let out: Vec<u8> = parallel_map(&input, |_, &x| x);
        assert!(out.is_empty());
    }

    #[test]
    fn parallel_map_preserves_order_and_index() {
        let input: Vec<u32> = (0..8).collect();
        let out = parallel_map(&input, |i, &x| (i, x * 2));
        assert_eq!(
            out,
            vec![
                (0, 0),
                (1, 2),
                (2, 4),
                (3, 6),
                (4, 8),
                (5, 10),
                (6, 12),
                (7, 14)
            ]
        );
    }

    #[test]
    fn parallel_map_single_item_stays_sequential() {
        assert_eq!(parallel_map(&[7u8], |i, &x| i + x as usize), vec![7]);
    }
}
