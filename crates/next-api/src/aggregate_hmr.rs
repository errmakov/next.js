//! Aggregated HMR: one [`VersionState`] covering every chunk under a target's
//! root, so the dev server can subscribe once instead of per chunk.
//!
//! [`VersionState`]: turbopack_core::version::VersionState

use std::sync::Arc;

use anyhow::Result;
use rustc_hash::FxHashMap;
use turbo_rcstr::RcStr;
use turbo_tasks::{FxIndexMap, ReadRef, ResolvedVc, TraitRef, TryJoinIterExt, Vc};
use turbo_tasks_fs::FileSystemPath;
use turbo_tasks_hash::{Xxh3Hash64Hasher, encode_base64};
use turbopack_core::version::{
    NotFoundVersion, PartialUpdate, Update, Version, VersionState, VersionedContent,
};

use crate::versioned_content_map::VersionedContentMap;

/// One chunk's contribution to an [`AggregateHmrVersion`]: its output path and
/// the versioned content backing it.
pub struct HmrChunkWithContent {
    pub path: RcStr,
    pub content: ResolvedVc<Box<dyn VersionedContent>>,
}

/// Whether an emitted chunk participates in HMR. Source map (`.map`) files do
/// not: their content fully rewrites on any source change, which would force
/// per-chunk diffs to escalate to `Total`.
pub fn is_hmr_eligible_chunk(name: &str) -> bool {
    !name.ends_with(".map")
}

/// Per-chunk versions keyed by path. `id()` hashes sorted entries so it's
/// stable across `FxIndexMap` iteration order. Mirrors `EcmascriptDevChunkListVersion`.
#[turbo_tasks::value(serialization = "skip", shared)]
pub struct AggregateHmrVersion {
    #[turbo_tasks(trace_ignore)]
    pub versions: FxIndexMap<RcStr, TraitRef<Box<dyn Version>>>,
}

#[turbo_tasks::value_impl]
impl Version for AggregateHmrVersion {
    #[turbo_tasks::function]
    async fn id(&self) -> Result<Vc<RcStr>> {
        let mut entries = self
            .versions
            .iter()
            .map(|(path, version)| {
                let path = path.clone();
                let version = TraitRef::cell(version.clone());
                async move {
                    let id = version.id().owned().await?;
                    Ok::<_, anyhow::Error>((path, id))
                }
            })
            .try_join()
            .await?;
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        let mut hasher = Xxh3Hash64Hasher::new();
        hasher.write_value(entries.len());
        for (path, id) in entries {
            hasher.write_value(path.as_str());
            hasher.write_value(id.as_str());
        }
        Ok(Vc::cell(encode_base64(hasher.finish()).into()))
    }
}

impl AggregateHmrVersion {
    /// Snapshots every HMR-eligible chunk under `root` in `map` into a new
    /// [`Version`]. Returns a [`NotFoundVersion`] when no chunks exist yet
    /// (e.g. before any endpoints have been written).
    pub async fn from_map(
        map: Vc<VersionedContentMap>,
        root: &FileSystemPath,
    ) -> Result<Vc<Box<dyn Version>>> {
        let chunks = map.hmr_chunks_in_path(root).await?;
        if chunks.is_empty() {
            return Ok(Vc::upcast(NotFoundVersion::new()));
        }
        Ok(Vc::upcast(Self::from_chunks(&chunks).await?))
    }

    /// Snapshots each [`HmrChunkWithContent`]'s [`Version`] into a new
    /// [`AggregateHmrVersion`].
    pub async fn from_chunks(chunks: &[HmrChunkWithContent]) -> Result<Vc<Self>> {
        let versions = chunks
            .iter()
            .map(|HmrChunkWithContent { path, content }| {
                let path = path.clone();
                let content = *content;
                async move {
                    let version = content.version().into_trait_ref().await?;
                    Ok::<_, anyhow::Error>((path, version))
                }
            })
            .try_join()
            .await?
            .into_iter()
            .collect();
        Ok(Self { versions }.cell())
    }
}

/// Unions one chunk's `EcmascriptMergedUpdate` into the combined `{entries, chunks}`.
/// Both maps are keyed by globally-unique ids, so plain insertion is safe.
pub fn merge_ecmascript_merged_update(
    combined_entries: &mut FxHashMap<String, serde_json::Value>,
    combined_chunks: &mut FxHashMap<String, serde_json::Value>,
    instruction: &serde_json::Value,
) {
    let Some(obj) = instruction.as_object() else {
        return;
    };
    if let Some(entries) = obj.get("entries").and_then(|v| v.as_object()) {
        for (k, v) in entries {
            combined_entries.insert(k.clone(), v.clone());
        }
    }
    if let Some(chunks) = obj.get("chunks").and_then(|v| v.as_object()) {
        for (k, v) in chunks {
            combined_chunks.insert(k.clone(), v.clone());
        }
    }
}

/// Builds an `Update::Partial` whose instruction is a combined
/// `EcmascriptMergedUpdate` covering `entries` and `chunks`. Empty maps are
/// omitted so an empty `entries`/`chunks` field never appears in the payload.
///
/// Passing empty maps produces an instruction with only `type:
/// "EcmascriptMergedUpdate"`, used to advance `VersionState` to `to` without
/// the JS consumer applying anything: it sees a `partial` event with nothing
/// to apply and short-circuits.
pub fn merged_partial_update(
    to: TraitRef<Box<dyn Version>>,
    entries: FxHashMap<String, serde_json::Value>,
    chunks: FxHashMap<String, serde_json::Value>,
) -> Update {
    let mut instruction = serde_json::Map::new();
    instruction.insert(
        "type".to_string(),
        serde_json::Value::String("EcmascriptMergedUpdate".to_string()),
    );
    if !entries.is_empty() {
        instruction.insert(
            "entries".to_string(),
            serde_json::Value::Object(entries.into_iter().collect()),
        );
    }
    if !chunks.is_empty() {
        instruction.insert(
            "chunks".to_string(),
            serde_json::Value::Object(chunks.into_iter().collect()),
        );
    }
    Update::Partial(PartialUpdate {
        to,
        instruction: Arc::new(serde_json::Value::Object(instruction)),
    })
}

/// Per-chunk [`Update`]s computed against an `AggregateHmrVersion` snapshot.
/// `has_new_chunks` is true when the current snapshot contains chunks absent
/// from `from` (e.g. a new endpoint was written); callers decide whether that
/// affects the batch shape.
pub struct DiffResult {
    pub chunk_updates: Vec<(RcStr, ReadRef<Update>)>,
    pub has_new_chunks: bool,
}

/// Diffs each chunk against `from`'s [`AggregateHmrVersion`] snapshot, if any.
/// When `from` doesn't downcast to an aggregate version (e.g. the seed
/// transition), the returned `chunk_updates` is empty.
pub async fn diff_chunks_against(
    chunks: &[HmrChunkWithContent],
    from: Vc<VersionState>,
) -> Result<DiffResult> {
    if chunks.is_empty() {
        return Ok(DiffResult {
            chunk_updates: Vec::new(),
            has_new_chunks: false,
        });
    }
    let from_resolved = from.get().to_resolved().await?;
    let Some(from_aggregate) = ResolvedVc::try_downcast_type::<AggregateHmrVersion>(from_resolved)
    else {
        return Ok(DiffResult {
            chunk_updates: Vec::new(),
            has_new_chunks: false,
        });
    };
    let from_aggregate = from_aggregate.await?;

    let mut has_new_chunks = false;
    let chunk_updates = chunks
        .iter()
        .filter_map(|HmrChunkWithContent { path, content }| {
            let Some(prev) = from_aggregate.versions.get(path).cloned() else {
                has_new_chunks = true;
                return None;
            };
            Some((path.clone(), *content, TraitRef::cell(prev)))
        })
        .map(|(path, content, prev)| async move {
            let update = content.update(prev).await?;
            Ok::<_, anyhow::Error>((path, update))
        })
        .try_join()
        .await?;
    Ok(DiffResult {
        chunk_updates,
        has_new_chunks,
    })
}
