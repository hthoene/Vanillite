//! Blocking use of a record store: submit a batch and wait for its results on the calling
//! thread. For tools, tests and import; the game never blocks on storage.

use std::io;
use std::sync::Arc;

use vanillite_base::ChunkPos;
use vanillite_chunk::ProtoChunk;
use vanillite_chunk::slots::{
    ChunkCodec, ChunkStore, Completion, CompletionTarget, DimensionId, Priority, RecordKey,
    RecordMeta, StoreOp, StoreResult, Token,
};

/// Runs a batch and returns one result per op, in op order.
pub fn run(store: &dyn ChunkStore, ops: Vec<StoreOp>) -> Vec<StoreResult> {
    let (to, inbox) = CompletionTarget::channel();
    let count = ops.len();
    store.submit(Token(0), ops, Priority::Urgent, to);
    let mut results: Vec<Option<StoreResult>> = (0..count).map(|_| None).collect();
    let mut missing = count;
    while missing > 0 {
        match inbox.recv() {
            Ok(Completion::Op { index, result, .. }) => {
                if let Some(slot) = results.get_mut(index)
                    && slot.replace(result).is_none()
                {
                    missing -= 1;
                }
            }
            Ok(Completion::Commit { .. }) => {}
            Err(_) => break,
        }
    }
    results
        .into_iter()
        .map(|r| {
            r.unwrap_or_else(|| StoreResult::Failed(io::Error::other("store dropped the batch")))
        })
        .collect()
}

/// Reads records; `None` for missing ones.
pub fn get(store: &dyn ChunkStore, keys: &[RecordKey]) -> Vec<io::Result<Option<Arc<[u8]>>>> {
    run(store, keys.iter().map(|&k| StoreOp::Get(k)).collect())
        .into_iter()
        .map(|result| match result {
            StoreResult::Record(_, bytes) => Ok(Some(bytes)),
            StoreResult::Missing => Ok(None),
            StoreResult::Failed(e) => Err(e),
            other => Err(io::Error::other(format!("unexpected {other:?}"))),
        })
        .collect()
}

/// Writes records.
pub fn put(store: &dyn ChunkStore, records: Vec<(RecordKey, Arc<[u8]>)>) -> Vec<io::Result<()>> {
    let ops = records
        .into_iter()
        .map(|(key, bytes)| StoreOp::Put {
            key,
            meta: RecordMeta::default(),
            bytes,
        })
        .collect();
    run(store, ops).into_iter().map(done).collect()
}

fn done(result: StoreResult) -> io::Result<()> {
    match result {
        StoreResult::Done => Ok(()),
        StoreResult::Failed(e) => Err(e),
        other => Err(io::Error::other(format!("unexpected {other:?}"))),
    }
}

/// Loads chunks: `Ok(None)` for chunks that were never saved.
pub fn load(
    store: &dyn ChunkStore,
    codec: &dyn ChunkCodec,
    dimension: DimensionId,
    positions: &[ChunkPos],
) -> Vec<io::Result<Option<ProtoChunk>>> {
    let ops = positions
        .iter()
        .map(|&pos| StoreOp::Get(RecordKey::chunk(dimension, pos)))
        .collect();
    run(store, ops)
        .into_iter()
        .zip(positions)
        .map(|(result, &pos)| match result {
            StoreResult::Record(meta, bytes) => codec.decode(pos, &meta, &bytes).map(Some),
            StoreResult::Missing => Ok(None),
            StoreResult::Failed(e) => Err(e),
            other => Err(io::Error::other(format!("unexpected {other:?}"))),
        })
        .collect()
}

/// Saves chunks.
pub fn save(
    store: &dyn ChunkStore,
    codec: &dyn ChunkCodec,
    dimension: DimensionId,
    chunks: &[&ProtoChunk],
) -> Vec<io::Result<()>> {
    let mut ops = Vec::with_capacity(chunks.len());
    let mut errors = Vec::new();
    for (index, chunk) in chunks.iter().enumerate() {
        match codec.encode(chunk) {
            Ok((meta, bytes)) => ops.push(StoreOp::Put {
                key: RecordKey::chunk(dimension, chunk.pos),
                meta,
                bytes,
            }),
            Err(e) => errors.push((index, e)),
        }
    }
    let mut results: Vec<io::Result<()>> = run(store, ops).into_iter().map(done).collect();
    for (index, e) in errors {
        results.insert(index, Err(e));
    }
    results
}

/// Commits everything submitted so far.
pub fn commit(store: &dyn ChunkStore) -> io::Result<()> {
    let (to, inbox) = CompletionTarget::channel();
    store.commit(0, to);
    loop {
        match inbox.recv() {
            Ok(Completion::Commit { result, .. }) => return result,
            Ok(Completion::Op { .. }) => {}
            Err(_) => return Err(io::Error::other("store dropped the commit")),
        }
    }
}
