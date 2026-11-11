//! Deterministic, opt-in IVF index for hybrid project memory.
//!
//! The exact scan is the default and the fallback. This module only *adds* a
//! bounded candidate lane: when a project explicitly selects `index = ivf`, a
//! derived k-means index is built from a bounded sample of stored embeddings.
//! Recall probes the nearest centroids, then scores every probed candidate
//! with the same exact cosine function used by the scan. Lexical candidates
//! and the existing deterministic RRF/task/scope/recency/status weights are
//! unchanged.
//!
//! Invariants:
//!
//! - The index is derived operational state. It is not an event, not a replay
//!   projection, and can be deleted and rebuilt at any time from
//!   `memory_embeddings`.
//! - No vector, centroid, content hash, or embedded text crosses an RPC,
//!   event, log, or UI boundary. Only aggregate counts and generation numbers
//!   are exposed.
//! - k-means is deterministic: input is sorted by memory id, initialization is
//!   seeded from a SHA-256 of project/model/dimensions/nlist plus that stable
//!   ordering, iterations are bounded, and every tie breaks by the lower
//!   centroid index (and then memory id for assignments).
//! - Every entry point validates dimensions, finiteness, and non-zero norm.
//!   Corrupt or stale state fails closed to the exact scan with an explicit
//!   reason instead of returning a partial result silently.
//! - No universal recall guarantee is claimed. The acceptance fixture measures
//!   recall@k against the exact scan; real recall depends on the embedding
//!   distribution, `nlist`, and `nprobe`.
//!
//! Centroid encoding (`versioned little-endian f32`):
//!
//! ```text
//! bytes 0..4   magic "VPIV"
//! byte  4      version (1)
//! bytes 5..9   dimensions, little-endian u32
//! bytes 9..13  centroid count, little-endian u32
//! bytes 13..   count * dimensions little-endian f32 values
//! ```
//!
//! The dimension and count fields, the exact byte length, finiteness of every
//! value, and non-zero L2 norm of every centroid are validated before use.

use anyhow::{Result, anyhow, bail};
use vp_protocol::types::{MemoryEmbeddingIndexStatus, MemoryEmbeddingPolicy, MemoryIndexMode};
use vp_store::Store;
use vp_store::store::{MemoryEmbeddingIndexCounts, MemoryEmbeddingIndexRow};

use crate::memory::{
    MAX_EMBEDDING_DIMENSIONS, MIN_EMBEDDING_DIMENSIONS, decode_embedding_vector, semantic_scores,
};
use crate::memory_embedding::{
    MAX_IVF_ASSIGNMENTS_PER_REBUILD, MAX_IVF_ASSIGNMENTS_PER_TICK, MAX_IVF_INDEX_BYTES,
    MAX_IVF_NLIST, MAX_IVF_SAMPLE_VECTORS, MAX_KMEANS_ITERATIONS,
};

pub const CENTROID_MAGIC: [u8; 4] = *b"VPIV";
pub const CENTROID_VERSION: u8 = 1;
pub const CENTROID_HEADER_BYTES: usize = 13;

/// One indexed vector supplied to a k-means build. The vector is already
/// decoded and validated by the caller; the memory id is the stable tie-break
/// and ordering key.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexVector {
    pub memory_id: String,
    pub values: Vec<f32>,
}

/// Deterministic k-means outcome. `assignments` is parallel to the *sorted*
/// sample; the caller persists it keyed by memory id.
#[derive(Debug, Clone, PartialEq)]
pub struct KMeansBuild {
    pub dimensions: u32,
    pub centroids: Vec<Vec<f32>>,
    pub assignments: Vec<(String, u32)>,
    pub iterations: u32,
}

/// What an IVF recall probe actually did. A fallback is not an error: the
/// engine performs the exact scan and keeps semantic recall active.
#[derive(Debug, Clone, Default)]
pub struct IvfProbe {
    pub scored: Vec<(vp_protocol::types::MemoryItem, f64)>,
    pub generation: Option<u64>,
    pub probed_clusters: u32,
    pub candidate_vectors: u32,
    /// Explicit fallback reason. `None` means the IVF lane produced the
    /// candidate set.
    pub fallback: Option<String>,
}

/// Outcome of one bounded maintenance pass.
#[derive(Debug, Clone, Default)]
pub struct IndexMaintenance {
    pub rebuilt_generation: Option<u64>,
    pub assigned_vectors: u32,
    pub repaired_assignments: u32,
    pub invalid_assignments: u32,
    pub generation: Option<u64>,
    pub vector_count: u32,
    pub cluster_count: u32,
}

/// Structural audit of assignment rows for one project/generation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IndexAudit {
    pub total: u32,
    pub invalid: u32,
    pub repaired: u32,
    pub assigned: u32,
}

/// Outcome of an explicit rebuild request.
#[derive(Debug, Clone, Default)]
pub struct IndexRebuildReport {
    pub rebuilt: bool,
    pub reason: Option<String>,
    pub generation: Option<u64>,
    pub vector_count: u32,
    pub assigned_vectors: u32,
    pub cluster_count: u32,
    pub nlist: u32,
}

/// Structural readiness of the stored index for the current policy and live
/// vector count. `reason` is machine-readable and safe to expose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexValidation {
    pub ready: bool,
    pub reason: Option<String>,
    pub generation: Option<u64>,
}

#[derive(Debug, Clone)]
struct PreparedVector {
    memory_id: String,
    values: Vec<f32>,
    norm: f64,
}

/// Encode centroids with the documented versioned header. Every centroid must
/// have exactly `dimensions` finite values and a non-zero norm.
pub fn encode_centroids(dimensions: u32, centroids: &[Vec<f32>]) -> Result<Vec<u8>> {
    if !(MIN_EMBEDDING_DIMENSIONS..=MAX_EMBEDDING_DIMENSIONS).contains(&dimensions) {
        bail!(
            "centroid dimensions must be between {MIN_EMBEDDING_DIMENSIONS} and {MAX_EMBEDDING_DIMENSIONS}"
        );
    }
    if centroids.is_empty() {
        bail!("an index needs at least one centroid");
    }
    if centroids.len() > MAX_IVF_NLIST as usize {
        bail!("centroid count exceeds the {MAX_IVF_NLIST} ceiling");
    }
    let count = u32::try_from(centroids.len()).map_err(|_| anyhow!("too many centroids"))?;
    let mut bytes = Vec::with_capacity(
        CENTROID_HEADER_BYTES + centroids.len().saturating_mul(dimensions as usize * 4),
    );
    bytes.extend_from_slice(&CENTROID_MAGIC);
    bytes.push(CENTROID_VERSION);
    bytes.extend_from_slice(&dimensions.to_le_bytes());
    bytes.extend_from_slice(&count.to_le_bytes());
    for centroid in centroids {
        if centroid.len() != dimensions as usize {
            bail!(
                "centroid has {} dimensions, expected {dimensions}",
                centroid.len()
            );
        }
        if centroid.iter().any(|value| !value.is_finite()) {
            bail!("centroid contains a non-finite value");
        }
        let norm = centroid
            .iter()
            .map(|value| f64::from(*value).powi(2))
            .sum::<f64>();
        if norm <= 0.0 || !norm.is_finite() {
            bail!("centroid has zero norm");
        }
        for value in centroid {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    Ok(bytes)
}

/// Cheap header/length/count validation without allocating the centroid
/// values. Used by status so a corrupt row is reported even when a full decode
/// would be wasteful.
pub fn validate_centroid_blob(bytes: &[u8], expected_dimensions: u32) -> Result<u32> {
    if bytes.len() < CENTROID_HEADER_BYTES {
        bail!("centroid blob is shorter than its header");
    }
    if bytes[..4] != CENTROID_MAGIC {
        bail!("centroid blob has an unknown magic");
    }
    if bytes[4] != CENTROID_VERSION {
        bail!("unsupported centroid version {}", bytes[4]);
    }
    let dimensions = u32::from_le_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]);
    if dimensions != expected_dimensions {
        bail!("centroid dimensions {dimensions} do not match policy {expected_dimensions}");
    }
    if !(MIN_EMBEDDING_DIMENSIONS..=MAX_EMBEDDING_DIMENSIONS).contains(&dimensions) {
        bail!("centroid dimensions are outside the supported range");
    }
    let count = u32::from_le_bytes([bytes[9], bytes[10], bytes[11], bytes[12]]);
    if count == 0 || count > MAX_IVF_NLIST {
        bail!("centroid count {count} is outside the supported range");
    }
    let expected_len = CENTROID_HEADER_BYTES
        .checked_add(
            usize::try_from(count)
                .map_err(|_| anyhow!("centroid count overflow"))?
                .checked_mul(usize::try_from(dimensions).unwrap_or(usize::MAX))
                .and_then(|values| values.checked_mul(4))
                .ok_or_else(|| anyhow!("centroid length overflow"))?,
        )
        .ok_or_else(|| anyhow!("centroid length overflow"))?;
    if bytes.len() != expected_len {
        bail!(
            "centroid blob has {} bytes, expected {expected_len}",
            bytes.len()
        );
    }
    Ok(count)
}

/// Decode and fully validate a centroid blob. This is the recall/build path;
/// status uses [`validate_centroid_blob`] instead.
pub fn decode_centroids(bytes: &[u8], expected_dimensions: u32) -> Result<Vec<Vec<f32>>> {
    let count = validate_centroid_blob(bytes, expected_dimensions)? as usize;
    let dimensions = expected_dimensions as usize;
    let mut centroids = Vec::with_capacity(count);
    let mut offset = CENTROID_HEADER_BYTES;
    for _ in 0..count {
        let end = offset + dimensions * 4;
        let mut values = Vec::with_capacity(dimensions);
        for chunk in bytes[offset..end].as_chunks::<4>().0 {
            let value = f32::from_le_bytes(*chunk);
            if !value.is_finite() {
                bail!("centroid contains a non-finite value");
            }
            values.push(value);
        }
        let norm = values
            .iter()
            .map(|value| f64::from(*value).powi(2))
            .sum::<f64>();
        if norm <= 0.0 || !norm.is_finite() {
            bail!("centroid has zero norm");
        }
        centroids.push(values);
        offset = end;
    }
    Ok(centroids)
}

/// Stable seed for k-means. Generation is intentionally excluded: an explicit
/// rebuild of unchanged data must produce byte-identical centroids.
pub fn index_seed(
    project_id: &str,
    model: &str,
    dimensions: u32,
    nlist: u32,
    sorted_memory_ids: &[String],
) -> u64 {
    let mut material = String::with_capacity(64 + sorted_memory_ids.len() * 40);
    material.push_str(project_id);
    material.push('\u{1f}');
    material.push_str(model);
    material.push('\u{1f}');
    material.push_str(&dimensions.to_string());
    material.push('\u{1f}');
    material.push_str(&nlist.to_string());
    material.push('\u{1f}');
    for memory_id in sorted_memory_ids {
        material.push_str(memory_id);
        material.push('\u{1f}');
    }
    let digest = crate::sha256_hex(material.as_bytes());
    let bytes = digest.as_bytes();
    let mut seed = 0u64;
    for chunk in bytes[..16].as_chunks::<2>().0 {
        let pair = std::str::from_utf8(chunk).unwrap_or("00");
        let value = u8::from_str_radix(pair, 16).unwrap_or(0);
        seed = (seed << 8) | u64::from(value);
    }
    seed
}

#[derive(Debug, Clone)]
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self {
            state: seed ^ 0x9e37_79b9_7f4a_7c15,
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    fn next_unit(&mut self) -> f64 {
        // 53 significant bits, exactly representable in f64.
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn prepare_vector(memory_id: &str, values: &[f32], dimensions: u32) -> Result<PreparedVector> {
    if values.len() != dimensions as usize {
        bail!(
            "vector {memory_id} has {} dimensions, expected {dimensions}",
            values.len()
        );
    }
    if values.iter().any(|value| !value.is_finite()) {
        bail!("vector {memory_id} contains a non-finite value");
    }
    let norm = values
        .iter()
        .map(|value| f64::from(*value).powi(2))
        .sum::<f64>();
    if norm <= 0.0 || !norm.is_finite() {
        bail!("vector {memory_id} has zero norm");
    }
    Ok(PreparedVector {
        memory_id: memory_id.to_string(),
        values: values.to_vec(),
        norm: norm.sqrt(),
    })
}

fn prepared_cosine(left: &[f32], left_norm: f64, right: &[f32], right_norm: f64) -> Option<f64> {
    if left.len() != right.len() || left_norm <= 0.0 || right_norm <= 0.0 {
        return None;
    }
    let mut dot = 0.0f64;
    for (left, right) in left.iter().zip(right) {
        dot += f64::from(*left) * f64::from(*right);
    }
    let similarity = dot / (left_norm * right_norm);
    if similarity.is_finite() {
        Some(similarity)
    } else {
        None
    }
}

fn centroid_norm(centroid: &[f32]) -> f64 {
    centroid
        .iter()
        .map(|value| f64::from(*value).powi(2))
        .sum::<f64>()
        .sqrt()
}

fn nearest_centroid(centroids: &[Vec<f32>], vector: &[f32]) -> Result<u32> {
    if vector.is_empty() || vector.iter().any(|value| !value.is_finite()) {
        bail!("assignment vector is empty or contains a non-finite value");
    }
    let norm = vector
        .iter()
        .map(|value| f64::from(*value).powi(2))
        .sum::<f64>()
        .sqrt();
    if norm <= 0.0 {
        bail!("assignment vector has zero norm");
    }
    let mut best = None::<(f64, u32)>;
    for (index, centroid) in centroids.iter().enumerate() {
        if centroid.len() != vector.len() {
            bail!("centroid dimensions do not match the assignment vector");
        }
        let centroid_norm = centroid_norm(centroid);
        let similarity =
            prepared_cosine(vector, norm, centroid, centroid_norm).ok_or_else(|| {
                anyhow!("centroid {index} cannot be scored (zero norm or non-finite values)")
            })?;
        let index = u32::try_from(index).map_err(|_| anyhow!("centroid index overflow"))?;
        match best {
            None => best = Some((similarity, index)),
            Some((best_similarity, _)) if similarity > best_similarity => {
                best = Some((similarity, index));
            }
            // Equal similarity always keeps the lower centroid index.
            Some(_) => {}
        }
    }
    best.map(|(_, index)| index)
        .ok_or_else(|| anyhow!("index has no centroids"))
}

/// Rank centroids by cosine similarity to the query. Ties break by the lower
/// centroid index, which makes recall stable across runs and platforms.
pub fn rank_centroids(centroids: &[Vec<f32>], query: &[f32], nprobe: u32) -> Result<Vec<u32>> {
    if centroids.is_empty() {
        bail!("index has no centroids");
    }
    if query.is_empty() || query.iter().any(|value| !value.is_finite()) {
        bail!("query vector is empty or contains a non-finite value");
    }
    let query_norm = query
        .iter()
        .map(|value| f64::from(*value).powi(2))
        .sum::<f64>()
        .sqrt();
    if query_norm <= 0.0 {
        bail!("query vector has zero norm");
    }
    let mut ranked = Vec::with_capacity(centroids.len());
    for (index, centroid) in centroids.iter().enumerate() {
        if centroid.len() != query.len() {
            bail!("centroid dimensions do not match the query vector");
        }
        let similarity = prepared_cosine(query, query_norm, centroid, centroid_norm(centroid))
            .ok_or_else(|| anyhow!("centroid {index} cannot be scored"))?;
        ranked.push((similarity, u32::try_from(index).unwrap_or(u32::MAX)));
    }
    ranked.sort_by(|left, right| {
        right
            .0
            .total_cmp(&left.0)
            .then_with(|| left.1.cmp(&right.1))
    });
    ranked.truncate(nprobe.max(1) as usize);
    Ok(ranked.into_iter().map(|(_, index)| index).collect())
}

/// Assign one vector to its nearest centroid with a stable tie-break.
pub fn assign_to_centroids(centroids: &[Vec<f32>], vector: &[f32]) -> Result<u32> {
    nearest_centroid(centroids, vector)
}

/// Build deterministic spherical k-means centroids from a bounded sample.
///
/// The sample is sorted by memory id before initialization so input order
/// cannot change the result. k-means++ uses a deterministic splitmix64 stream
/// seeded from stable project/model/dimension/nlist metadata. Empty clusters
/// retain their previous centroid; no random restart occurs. The objective is
/// cosine similarity, matching recall scoring.
pub fn build_kmeans(sample: &[IndexVector], nlist: u32, seed: u64) -> Result<KMeansBuild> {
    if sample.is_empty() {
        bail!("cannot build an index from an empty sample");
    }
    if !(1..=MAX_IVF_NLIST).contains(&nlist) {
        bail!("nlist must be between 1 and {MAX_IVF_NLIST}");
    }
    let dimensions = u32::try_from(sample[0].values.len())
        .map_err(|_| anyhow!("sample dimensions exceed u32"))?;
    if !(MIN_EMBEDDING_DIMENSIONS..=MAX_EMBEDDING_DIMENSIONS).contains(&dimensions) {
        bail!(
            "sample dimensions must be between {MIN_EMBEDDING_DIMENSIONS} and {MAX_EMBEDDING_DIMENSIONS}"
        );
    }
    let mut ordered = sample.to_vec();
    ordered.sort_by(|left, right| left.memory_id.cmp(&right.memory_id));
    for pair in ordered.windows(2) {
        if pair[0].memory_id == pair[1].memory_id {
            bail!(
                "sample contains a duplicate memory id: {}",
                pair[0].memory_id
            );
        }
    }
    let prepared = ordered
        .iter()
        .map(|vector| prepare_vector(&vector.memory_id, &vector.values, dimensions))
        .collect::<Result<Vec<_>>>()?;
    let count = prepared.len();
    let k = (nlist as usize).clamp(1, MAX_IVF_NLIST as usize).min(count);
    if k == 0 {
        bail!("cannot build an index without centroids");
    }

    let mut rng = SplitMix64::new(seed);
    let mut chosen = Vec::with_capacity(k);
    let first = (rng.next_u64() % count as u64) as usize;
    chosen.push(first);
    let mut nearest = prepared
        .iter()
        .map(|vector| {
            let similarity = prepared_cosine(
                &vector.values,
                vector.norm,
                &prepared[first].values,
                prepared[first].norm,
            )
            .unwrap_or(-1.0);
            (1.0 - similarity).max(0.0)
        })
        .collect::<Vec<_>>();
    while chosen.len() < k {
        let total = nearest
            .iter()
            .map(|distance| distance * distance)
            .sum::<f64>();
        let next = if total <= f64::EPSILON || !total.is_finite() {
            // No distance mass left (duplicate or identical vectors): pick the
            // lowest not-yet-chosen index so the build stays deterministic.
            (0..count).find(|index| !chosen.contains(index))
        } else {
            let target = rng.next_unit() * total;
            let mut cumulative = 0.0;
            let mut selected = None;
            for (index, distance) in nearest.iter().enumerate() {
                if chosen.contains(&index) {
                    continue;
                }
                cumulative += distance * distance;
                if cumulative >= target {
                    selected = Some(index);
                    break;
                }
            }
            selected.or_else(|| (0..count).rev().find(|index| !chosen.contains(index)))
        };
        let Some(next) = next else {
            break;
        };
        chosen.push(next);
        for (index, vector) in prepared.iter().enumerate() {
            let similarity = prepared_cosine(
                &vector.values,
                vector.norm,
                &prepared[next].values,
                prepared[next].norm,
            )
            .unwrap_or(-1.0);
            let distance = (1.0 - similarity).max(0.0);
            if distance < nearest[index] {
                nearest[index] = distance;
            }
        }
    }

    let mut centroids = chosen
        .iter()
        .map(|index| normalized(&prepared[*index].values, dimensions))
        .collect::<Result<Vec<_>>>()?;
    let mut assignments = vec![0u32; count];
    let mut iterations = 0u32;
    let mut converged = false;
    while iterations < MAX_KMEANS_ITERATIONS {
        iterations += 1;
        let mut changed = false;
        for (index, vector) in prepared.iter().enumerate() {
            let cluster = nearest_centroid(&centroids, &vector.values)?;
            if assignments[index] != cluster {
                assignments[index] = cluster;
                changed = true;
            }
        }
        let mut sums = vec![vec![0.0f64; dimensions as usize]; centroids.len()];
        let mut counts = vec![0usize; centroids.len()];
        for (index, vector) in prepared.iter().enumerate() {
            let cluster = assignments[index] as usize;
            counts[cluster] += 1;
            for (sum, value) in sums[cluster].iter_mut().zip(&vector.values) {
                *sum += f64::from(*value);
            }
        }
        for (cluster, centroid) in centroids.iter_mut().enumerate() {
            if counts[cluster] == 0 {
                continue;
            }
            let mean = sums[cluster]
                .iter()
                .map(|value| *value / counts[cluster] as f64)
                .collect::<Vec<_>>();
            let norm = mean.iter().map(|value| value * value).sum::<f64>().sqrt();
            if norm <= f64::EPSILON || !norm.is_finite() {
                // Retain the previous centroid rather than introducing a
                // zero vector that would break cosine scoring.
                continue;
            }
            *centroid = mean
                .iter()
                .map(|value| (value / norm) as f32)
                .collect::<Vec<_>>();
        }
        if !changed {
            converged = true;
            break;
        }
    }
    if !converged {
        // One final assignment pass so the returned map matches the returned
        // centroids as closely as the iteration bound allows.
        for (index, vector) in prepared.iter().enumerate() {
            assignments[index] = nearest_centroid(&centroids, &vector.values)?;
        }
    }
    Ok(KMeansBuild {
        dimensions,
        centroids,
        assignments: prepared
            .into_iter()
            .zip(assignments)
            .map(|(vector, cluster)| (vector.memory_id, cluster))
            .collect(),
        iterations,
    })
}

fn normalized(values: &[f32], dimensions: u32) -> Result<Vec<f32>> {
    if values.len() != dimensions as usize {
        bail!("vector dimensions do not match");
    }
    let norm = values
        .iter()
        .map(|value| f64::from(*value).powi(2))
        .sum::<f64>()
        .sqrt();
    if norm <= 0.0 || !norm.is_finite() {
        bail!("vector has zero norm");
    }
    Ok(values
        .iter()
        .map(|value| (f64::from(*value) / norm) as f32)
        .collect())
}

/// Validate a stored index against the current policy and live counts. The
/// order is intentional: a configured minimum that is not met is reported as
/// `below_min_vectors` even if an old blob is also corrupt, because that is
/// the actionable operator state.
pub fn validate_index(
    policy: &MemoryEmbeddingPolicy,
    row: Option<&MemoryEmbeddingIndexRow>,
    counts: MemoryEmbeddingIndexCounts,
) -> IndexValidation {
    let generation = row
        .map(|row| row.generation)
        .filter(|generation| *generation > 0);
    if !policy.enabled {
        return IndexValidation {
            ready: false,
            reason: Some("embedding_disabled".to_string()),
            generation,
        };
    }
    if !policy.is_configured() {
        return IndexValidation {
            ready: false,
            reason: Some("not_configured".to_string()),
            generation,
        };
    }
    if policy.index == MemoryIndexMode::Scan {
        return IndexValidation {
            ready: false,
            reason: Some("scan_mode".to_string()),
            generation,
        };
    }
    let Some(model) = policy
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
    else {
        return IndexValidation {
            ready: false,
            reason: Some("not_configured".to_string()),
            generation,
        };
    };
    // Metadata mismatch is more actionable than a count threshold, and a
    // stored generation from another model cannot be used even if the current
    // model happens to have few vectors. Report it first when a real
    // generation exists.
    if let Some(row) = row
        && row.generation > 0
    {
        if row.model != model {
            return IndexValidation {
                ready: false,
                reason: Some("model_mismatch".to_string()),
                generation,
            };
        }
        if row.dimensions != policy.dimensions {
            return IndexValidation {
                ready: false,
                reason: Some("dimensions_mismatch".to_string()),
                generation,
            };
        }
        if row.nlist != policy.ivf_nlist {
            return IndexValidation {
                ready: false,
                reason: Some("nlist_mismatch".to_string()),
                generation,
            };
        }
    }
    if counts.vector_count < policy.ivf_min_vectors {
        return IndexValidation {
            ready: false,
            reason: Some("below_min_vectors".to_string()),
            generation,
        };
    }
    let Some(row) = row else {
        return IndexValidation {
            ready: false,
            reason: Some("not_built".to_string()),
            generation: None,
        };
    };
    if row.generation == 0 {
        return IndexValidation {
            ready: false,
            reason: Some("not_built".to_string()),
            generation: None,
        };
    }
    let cluster_count = match validate_centroid_blob(&row.centroids, policy.dimensions) {
        Ok(count) => count,
        Err(error) => {
            return IndexValidation {
                ready: false,
                reason: Some(format!(
                    "index_corrupt: {}",
                    crate::memory_embedding::sanitize_embedding_error(&error.to_string())
                )),
                generation,
            };
        }
    };
    if cluster_count != row.cluster_count {
        return IndexValidation {
            ready: false,
            reason: Some("index_corrupt: centroid count does not match metadata".to_string()),
            generation,
        };
    }
    if counts.assigned_vectors == 0 {
        return IndexValidation {
            ready: false,
            reason: Some("no_assignments".to_string()),
            generation,
        };
    }
    IndexValidation {
        ready: true,
        reason: None,
        generation,
    }
}

/// Bounded pre-scoring candidate window for one IVF probe. Each probed
/// centroid gets a share of `max_candidates`; the total is hard-capped at the
/// documented 2,048 so a hand-edited `nprobe` cannot create an unbounded read.
pub fn probe_candidate_limit(policy: &MemoryEmbeddingPolicy) -> u32 {
    policy
        .max_candidates
        .clamp(1, 256)
        .saturating_mul(policy.ivf_nprobe)
        .clamp(1, crate::memory_embedding::MAX_IVF_CANDIDATES)
}

/// Probe the index for one query. Store errors and validation failures are
/// converted into a fallback reason so a recall never fails because derived
/// index state is unhealthy. The caller performs the exact scan.
pub fn probe_ivf(
    store: &Store,
    project_id: &str,
    policy: &MemoryEmbeddingPolicy,
    include_stale: bool,
    query_vector: &[f32],
) -> IvfProbe {
    let generation_hint = store
        .memory_embedding_index_row(project_id)
        .ok()
        .flatten()
        .map(|row| row.generation)
        .filter(|generation| *generation > 0);
    let Some(model) = policy
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
    else {
        return IvfProbe {
            fallback: Some("not_configured".to_string()),
            generation: generation_hint,
            ..IvfProbe::default()
        };
    };
    let dimensions = policy.dimensions;
    if query_vector.len() != dimensions as usize
        || query_vector.is_empty()
        || query_vector.iter().any(|value| !value.is_finite())
    {
        return IvfProbe {
            fallback: Some("query_invalid".to_string()),
            generation: generation_hint,
            ..IvfProbe::default()
        };
    }
    let row = match store.memory_embedding_index_row(project_id) {
        Ok(row) => row,
        Err(error) => {
            return IvfProbe {
                fallback: Some(format!(
                    "index_read_error: {}",
                    crate::memory_embedding::sanitize_embedding_error(&error.to_string())
                )),
                generation: generation_hint,
                ..IvfProbe::default()
            };
        }
    };
    let Some(row) = row else {
        let counts = match store.memory_embedding_index_counts(project_id, model, dimensions, 0, 0)
        {
            Ok(counts) => counts,
            Err(error) => {
                return IvfProbe {
                    fallback: Some(format!(
                        "index_read_error: {}",
                        crate::memory_embedding::sanitize_embedding_error(&error.to_string())
                    )),
                    generation: None,
                    ..IvfProbe::default()
                };
            }
        };
        return IvfProbe {
            fallback: Some(if counts.vector_count < policy.ivf_min_vectors {
                "below_min_vectors".to_string()
            } else {
                "not_built".to_string()
            }),
            generation: None,
            ..IvfProbe::default()
        };
    };
    let counts = match store.memory_embedding_index_counts(
        project_id,
        model,
        dimensions,
        row.generation,
        row.cluster_count,
    ) {
        Ok(counts) => counts,
        Err(error) => {
            return IvfProbe {
                fallback: Some(format!(
                    "index_read_error: {}",
                    crate::memory_embedding::sanitize_embedding_error(&error.to_string())
                )),
                generation: generation_hint,
                ..IvfProbe::default()
            };
        }
    };
    let validation = validate_index(policy, Some(&row), counts);
    if !validation.ready {
        return IvfProbe {
            fallback: validation.reason,
            generation: validation.generation,
            ..IvfProbe::default()
        };
    }
    let centroids = match decode_centroids(&row.centroids, dimensions) {
        Ok(centroids) => centroids,
        Err(error) => {
            return IvfProbe {
                fallback: Some(format!(
                    "index_corrupt: {}",
                    crate::memory_embedding::sanitize_embedding_error(&error.to_string())
                )),
                generation: validation.generation,
                ..IvfProbe::default()
            };
        }
    };
    let probed_clusters = match rank_centroids(&centroids, query_vector, policy.ivf_nprobe) {
        Ok(clusters) => clusters,
        Err(error) => {
            return IvfProbe {
                fallback: Some(format!("query_invalid: {error}")),
                generation: validation.generation,
                ..IvfProbe::default()
            };
        }
    };
    let candidate_limit = probe_candidate_limit(policy);
    let candidates = match store.memory_ivf_semantic_candidates(
        project_id,
        model,
        dimensions,
        row.generation,
        &probed_clusters,
        include_stale,
        candidate_limit,
    ) {
        Ok(candidates) => candidates,
        Err(error) => {
            return IvfProbe {
                fallback: Some(format!(
                    "index_read_error: {}",
                    crate::memory_embedding::sanitize_embedding_error(&error.to_string())
                )),
                generation: validation.generation,
                ..IvfProbe::default()
            };
        }
    };
    let candidate_vectors = u32::try_from(candidates.len()).unwrap_or(u32::MAX);
    let mut scored = semantic_scores(
        candidates,
        query_vector,
        dimensions,
        f64::from(policy.min_similarity),
    );
    scored.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.id.cmp(&right.0.id))
    });
    scored.truncate(policy.max_candidates.clamp(1, 256) as usize);
    IvfProbe {
        scored,
        generation: validation.generation,
        probed_clusters: u32::try_from(probed_clusters.len()).unwrap_or(u32::MAX),
        candidate_vectors,
        fallback: None,
    }
}

/// Pure rebuild decision shared by maintenance and its tests. A missing or
/// undecodable generation always rebuilds. Metadata changes rebuild. The growth
/// threshold rebuilds exactly when the live count first crosses
/// `ivf_rebuild_vectors`, and then again after each additional
/// `ivf_rebuild_vectors` of growth since the stored `vector_count`.
fn should_rebuild(
    policy: &MemoryEmbeddingPolicy,
    row: Option<&MemoryEmbeddingIndexRow>,
    decoded: bool,
    model: &str,
    dimensions: u32,
    vector_count: u32,
) -> bool {
    let Some(row) = row else {
        return true;
    };
    if !decoded {
        return true;
    }
    if row.model != model || row.dimensions != dimensions || row.nlist != policy.ivf_nlist {
        return true;
    }
    vector_count >= policy.ivf_rebuild_vectors
        && (row.vector_count < policy.ivf_rebuild_vectors
            || vector_count >= row.vector_count.saturating_add(policy.ivf_rebuild_vectors))
}

/// Bounded maintenance for one enabled IVF project: rebuild when metadata or
/// the growth threshold requires it, otherwise assign missing/stale vectors to
/// the current centroids. Concurrent embedding writes are never blocked; a
/// vector written after the sample is picked up by a later assignment pass.
pub fn maintain_index(
    store: &Store,
    project_id: &str,
    policy: &MemoryEmbeddingPolicy,
) -> Result<IndexMaintenance> {
    if !policy.uses_ivf_index() {
        return Ok(IndexMaintenance::default());
    }
    let Some(model) = policy
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
    else {
        return Ok(IndexMaintenance::default());
    };
    let dimensions = policy.dimensions;
    let row = store.memory_embedding_index_row(project_id)?;
    let generation = row.as_ref().map(|row| row.generation).unwrap_or(0);
    let counts = store.memory_embedding_index_counts(
        project_id,
        model,
        dimensions,
        generation,
        row.as_ref().map(|row| row.cluster_count).unwrap_or(0),
    )?;
    if counts.vector_count < policy.ivf_min_vectors {
        let same_metadata = row.as_ref().is_some_and(|row| {
            row.model == model && row.dimensions == dimensions && row.nlist == policy.ivf_nlist
        });
        if same_metadata && row.as_ref().is_some_and(|row| row.generation > 0) {
            // Below the configured minimum the index must not be used. Delete
            // the derived centroids/assignments so a later growth starts from
            // a clean build. A generation-0 diagnostic row is retained so the
            // fallback counter and last error remain observable.
            store.delete_memory_embedding_index(project_id)?;
        }
        // A metadata-mismatched row is kept until current-model vectors exist:
        // it is already unusable, and keeping its generation avoids resetting
        // the monotonic counter (or discarding fallback diagnostics) during a
        // model/dimension migration.
        return Ok(IndexMaintenance {
            vector_count: counts.vector_count,
            ..IndexMaintenance::default()
        });
    }
    let decoded = row
        .as_ref()
        .and_then(|row| decode_centroids(&row.centroids, dimensions).ok());
    let rebuild = should_rebuild(
        policy,
        row.as_ref(),
        decoded.is_some(),
        model,
        dimensions,
        counts.vector_count,
    );
    if rebuild {
        let result = rebuild_from_store(store, project_id, policy, MAX_IVF_ASSIGNMENTS_PER_TICK);
        if let Err(error) = &result {
            let sanitized = crate::memory_embedding::sanitize_embedding_error(&error.to_string());
            let _ = store.record_memory_embedding_index_error(
                project_id,
                model,
                dimensions,
                policy.ivf_nlist,
                &sanitized,
            );
        }
        return result;
    }
    let (Some(row), Some(centroids)) = (row, decoded) else {
        return rebuild_from_store(store, project_id, policy, MAX_IVF_ASSIGNMENTS_PER_TICK);
    };
    let audit = audit_index(store, project_id, policy)?;
    let assigned = assign_pending(
        store,
        project_id,
        model,
        dimensions,
        row.generation,
        &centroids,
        MAX_IVF_ASSIGNMENTS_PER_TICK,
    )?;
    let counts = store.memory_embedding_index_counts(
        project_id,
        model,
        dimensions,
        row.generation,
        row.cluster_count,
    )?;
    Ok(IndexMaintenance {
        rebuilt_generation: None,
        assigned_vectors: assigned,
        repaired_assignments: audit.repaired,
        invalid_assignments: audit.invalid.saturating_sub(audit.repaired),
        generation: Some(row.generation),
        vector_count: counts.vector_count,
        cluster_count: u32::try_from(centroids.len()).unwrap_or(u32::MAX),
    })
}

/// Explicit operator rebuild. Unlike maintenance this always creates a new
/// generation, and it also clears state for scan-mode or below-minimum
/// projects so the operator receives an honest, actionable result.
pub fn rebuild_index(
    store: &Store,
    project_id: &str,
    policy: &MemoryEmbeddingPolicy,
) -> Result<IndexRebuildReport> {
    if policy.index == MemoryIndexMode::Scan {
        let cleared = store.delete_memory_embedding_index(project_id)?;
        return Ok(IndexRebuildReport {
            reason: Some(if cleared > 0 {
                "index_mode_scan: removed derived index state".to_string()
            } else {
                "index_mode_scan".to_string()
            }),
            nlist: policy.ivf_nlist,
            ..IndexRebuildReport::default()
        });
    }
    if !policy.enabled {
        return Ok(IndexRebuildReport {
            reason: Some("embedding_disabled".to_string()),
            nlist: policy.ivf_nlist,
            ..IndexRebuildReport::default()
        });
    }
    let Some(model) = policy
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
    else {
        return Ok(IndexRebuildReport {
            reason: Some("not_configured".to_string()),
            nlist: policy.ivf_nlist,
            ..IndexRebuildReport::default()
        });
    };
    let dimensions = policy.dimensions;
    let counts = store.memory_embedding_index_counts(project_id, model, dimensions, 0, 0)?;
    if counts.vector_count < policy.ivf_min_vectors {
        store.delete_memory_embedding_index(project_id)?;
        return Ok(IndexRebuildReport {
            reason: Some("below_min_vectors".to_string()),
            vector_count: counts.vector_count,
            nlist: policy.ivf_nlist,
            ..IndexRebuildReport::default()
        });
    }
    match rebuild_from_store(store, project_id, policy, MAX_IVF_ASSIGNMENTS_PER_REBUILD) {
        Ok(maintenance) => Ok(IndexRebuildReport {
            rebuilt: true,
            reason: None,
            generation: maintenance.rebuilt_generation,
            vector_count: maintenance.vector_count,
            assigned_vectors: maintenance.assigned_vectors,
            cluster_count: maintenance.cluster_count,
            nlist: policy.ivf_nlist,
        }),
        Err(error) => {
            let sanitized = crate::memory_embedding::sanitize_embedding_error(&error.to_string());
            let _ = store.record_memory_embedding_index_error(
                project_id,
                model,
                dimensions,
                policy.ivf_nlist,
                &sanitized,
            );
            Err(error)
        }
    }
}

/// Audit and repair assignment rows for the current generation. Invalid rows
/// are deleted in one bounded statement, then normal assignment maintenance
/// refills coverage. Recall never performs this repair.
pub fn audit_index(
    store: &Store,
    project_id: &str,
    policy: &MemoryEmbeddingPolicy,
) -> Result<IndexAudit> {
    if !policy.uses_ivf_index() {
        return Ok(IndexAudit::default());
    }
    let Some(model) = policy
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
    else {
        return Ok(IndexAudit::default());
    };
    let Some(row) = store
        .memory_embedding_index_row(project_id)?
        .filter(|row| row.generation > 0)
    else {
        return Ok(IndexAudit::default());
    };
    let audit = store.memory_embedding_assignment_audit(
        project_id,
        model,
        policy.dimensions,
        row.generation,
        row.cluster_count,
    )?;
    let repaired = if audit.invalid() > 0 {
        store.delete_invalid_memory_embedding_assignments(
            project_id,
            model,
            policy.dimensions,
            row.generation,
            row.cluster_count,
        )?
    } else {
        0
    };
    Ok(IndexAudit {
        total: audit.total,
        invalid: audit.invalid(),
        repaired,
        assigned: audit.total.saturating_sub(repaired),
    })
}

/// Read-only status for the derived index. Never decodes or returns centroid
/// values; header validation is enough to report corruption.
pub fn index_status(
    store: &Store,
    project_id: &str,
    policy: Option<&MemoryEmbeddingPolicy>,
) -> Result<MemoryEmbeddingIndexStatus> {
    let Some(policy) = policy else {
        return Ok(MemoryEmbeddingIndexStatus {
            fallback_reason: Some("not_configured".to_string()),
            ..MemoryEmbeddingIndexStatus::default()
        });
    };
    let row = store.memory_embedding_index_row(project_id)?;
    let model = policy
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or_default();
    let generation = row.as_ref().map(|row| row.generation).unwrap_or(0);
    let counts = store.memory_embedding_index_counts(
        project_id,
        model,
        policy.dimensions,
        generation,
        row.as_ref().map(|row| row.cluster_count).unwrap_or(0),
    )?;
    let validation = validate_index(policy, row.as_ref(), counts);
    let assignment_audit = match row.as_ref().filter(|row| row.generation > 0) {
        Some(row) => store
            .memory_embedding_assignment_audit(
                project_id,
                model,
                policy.dimensions,
                row.generation,
                row.cluster_count,
            )
            .unwrap_or_default(),
        None => Default::default(),
    };
    let vector_count = counts.vector_count;
    let assigned_vectors = counts.assigned_vectors;
    Ok(MemoryEmbeddingIndexStatus {
        mode: policy.index.as_str().to_string(),
        ready: validation.ready,
        generation: validation.generation,
        model: row.as_ref().map(|row| row.model.clone()),
        dimensions: policy.dimensions,
        vector_count,
        assigned_vectors,
        invalid_assignments: assignment_audit.invalid(),
        cluster_count: row.as_ref().map(|row| row.cluster_count).unwrap_or(0),
        nlist: policy.ivf_nlist,
        nprobe: policy.ivf_nprobe,
        coverage: if vector_count == 0 {
            0.0
        } else {
            f64::from(assigned_vectors) / f64::from(vector_count)
        },
        min_vectors: policy.ivf_min_vectors,
        rebuild_vectors: policy.ivf_rebuild_vectors,
        bytes: row
            .as_ref()
            .map(|row| u64::try_from(row.centroids.len()).unwrap_or(u64::MAX))
            .unwrap_or(0),
        last_rebuilt_at: row
            .as_ref()
            .filter(|row| row.generation > 0)
            .map(|row| row.updated_at.clone()),
        last_error: row
            .as_ref()
            .and_then(|row| row.last_error.as_deref())
            .map(crate::memory_embedding::sanitize_embedding_error),
        fallback_count: row.as_ref().map(|row| row.fallback_count).unwrap_or(0),
        last_fallback_at: row.as_ref().and_then(|row| row.last_fallback_at.clone()),
        last_fallback_reason: row
            .as_ref()
            .and_then(|row| row.last_fallback_reason.clone()),
        fallback_reason: validation.reason,
    })
}

fn rebuild_from_store(
    store: &Store,
    project_id: &str,
    policy: &MemoryEmbeddingPolicy,
    assignment_cap: u32,
) -> Result<IndexMaintenance> {
    let model = policy
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .ok_or_else(|| anyhow!("embedding model is missing"))?;
    let dimensions = policy.dimensions;
    // Record the live project count, not the bounded sample size. The growth
    // threshold compares future live counts against this value; using the
    // 8,192 sample cap here would make a large project rebuild on every tick.
    let live_vector_count = store
        .memory_embedding_index_counts(project_id, model, dimensions, 0, 0)?
        .vector_count;
    let sample = store.memory_embedding_vectors_for_index(
        project_id,
        model,
        dimensions,
        MAX_IVF_SAMPLE_VECTORS,
        MAX_IVF_INDEX_BYTES,
    )?;
    if sample.is_empty() {
        bail!("at least one valid vector is required to build an index");
    }
    let vectors = sample
        .iter()
        .map(|candidate| {
            decode_embedding_vector(&candidate.vector, dimensions).map(|values| IndexVector {
                memory_id: candidate.memory.id.clone(),
                values,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    // The policy `nlist` is persisted even when the sample is smaller; the
    // actual cluster count is reported separately. Validation compares the
    // stored target to policy, so a small project must not look like an nlist
    // change on every rebuild.
    let target_clusters = policy
        .ivf_nlist
        .min(u32::try_from(vectors.len()).unwrap_or(u32::MAX));
    let sorted_ids = vectors
        .iter()
        .map(|vector| vector.memory_id.clone())
        .collect::<Vec<_>>();
    let seed = index_seed(project_id, model, dimensions, policy.ivf_nlist, &sorted_ids);
    let build = build_kmeans(&vectors, target_clusters, seed)?;
    let centroids = encode_centroids(dimensions, &build.centroids)?;
    let cluster_count = u32::try_from(build.centroids.len()).unwrap_or(u32::MAX);
    let generation = store.save_memory_embedding_index(
        project_id,
        model,
        dimensions,
        0,
        policy.ivf_nlist,
        cluster_count,
        live_vector_count,
        &centroids,
    )?;
    let assigned = assign_pending(
        store,
        project_id,
        model,
        dimensions,
        generation,
        &build.centroids,
        assignment_cap,
    )?;
    let counts = store.memory_embedding_index_counts(
        project_id,
        model,
        dimensions,
        generation,
        cluster_count,
    )?;
    Ok(IndexMaintenance {
        rebuilt_generation: Some(generation),
        assigned_vectors: assigned,
        repaired_assignments: 0,
        invalid_assignments: 0,
        generation: Some(generation),
        vector_count: counts.vector_count,
        cluster_count,
    })
}

fn assign_pending(
    store: &Store,
    project_id: &str,
    model: &str,
    dimensions: u32,
    generation: u64,
    centroids: &[Vec<f32>],
    cap: u32,
) -> Result<u32> {
    let mut remaining = cap;
    let mut assigned = 0u32;
    while remaining > 0 {
        let batch_limit = remaining.min(256);
        let candidates = store.memory_embedding_unassigned_vectors(
            project_id,
            model,
            dimensions,
            generation,
            batch_limit,
        )?;
        if candidates.is_empty() {
            break;
        }
        let mut assignments = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let values = decode_embedding_vector(&candidate.vector, dimensions)?;
            let cluster = assign_to_centroids(centroids, &values)?;
            assignments.push((candidate.memory.id, cluster));
        }
        let prepared = u32::try_from(assignments.len()).unwrap_or(u32::MAX);
        let written =
            store.save_memory_embedding_assignments(project_id, generation, &assignments)?;
        assigned = assigned.saturating_add(written);
        remaining = remaining.saturating_sub(prepared);
        if written == 0 {
            // Another build committed a newer generation; recall will use that
            // generation and this pass must stop instead of mixing assignments.
            break;
        }
        if written < prepared {
            break;
        }
    }
    Ok(assigned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vp_protocol::types::{MemoryEmbeddingIndexStatus, MemoryIndexMode};

    fn vector(values: &[f32]) -> IndexVector {
        IndexVector {
            memory_id: values
                .iter()
                .map(|value| format!("{value:+.3}"))
                .collect::<String>(),
            values: values.to_vec(),
        }
    }

    fn ids(vectors: &[IndexVector]) -> Vec<String> {
        let mut ids = vectors
            .iter()
            .map(|vector| vector.memory_id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        ids
    }

    fn policy_fixture() -> MemoryEmbeddingPolicy {
        MemoryEmbeddingPolicy {
            enabled: true,
            endpoint: Some("http://127.0.0.1:9/v1".to_string()),
            model: Some("embed-test".to_string()),
            dimensions: 8,
            ivf_nlist: 4,
            ivf_nprobe: 2,
            ivf_min_vectors: 4,
            ivf_rebuild_vectors: 16,
            index: MemoryIndexMode::Ivf,
            ..MemoryEmbeddingPolicy::default()
        }
    }

    #[test]
    fn centroid_encoding_round_trips_and_checks_every_header_field() {
        let centroids = vec![
            vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            vec![0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        ];
        let encoded = encode_centroids(8, &centroids).unwrap();
        assert_eq!(&encoded[..4], b"VPIV");
        assert_eq!(encoded[4], 1);
        assert_eq!(&encoded[5..9], &8u32.to_le_bytes());
        assert_eq!(&encoded[9..13], &2u32.to_le_bytes());
        assert_eq!(decode_centroids(&encoded, 8).unwrap(), centroids);
        assert_eq!(validate_centroid_blob(&encoded, 8).unwrap(), 2);

        let mut wrong_magic = encoded.clone();
        wrong_magic[0] = b'X';
        assert!(decode_centroids(&wrong_magic, 8).is_err());
        let mut wrong_version = encoded.clone();
        wrong_version[4] = 9;
        assert!(decode_centroids(&wrong_version, 8).is_err());
        assert!(decode_centroids(&encoded, 16).is_err());
        let mut truncated = encoded.clone();
        truncated.pop();
        assert!(decode_centroids(&truncated, 8).is_err());
        let mut empty = encoded.clone();
        empty.truncate(CENTROID_HEADER_BYTES);
        assert!(decode_centroids(&empty, 8).is_err());
    }

    #[test]
    fn centroid_encoding_rejects_zero_norm_and_non_finite_values() {
        let zero = vec![vec![0.0f32; 8]];
        assert!(encode_centroids(8, &zero).is_err());
        let infinite = vec![vec![f32::INFINITY, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]];
        assert!(encode_centroids(8, &infinite).is_err());
        let wrong_dimensions = vec![vec![1.0f32; 7]];
        assert!(encode_centroids(8, &wrong_dimensions).is_err());
        assert!(encode_centroids(8, &[]).is_err());
        let too_many = vec![vec![1.0f32; 8]; MAX_IVF_NLIST as usize + 1];
        assert!(encode_centroids(8, &too_many).is_err());
        assert!(encode_centroids(4, &[vec![1.0f32; 4]]).is_err());
    }

    #[test]
    fn corrupt_centroid_values_are_rejected_after_a_valid_header() {
        let mut encoded =
            encode_centroids(8, &[vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]]).unwrap();
        let offset = CENTROID_HEADER_BYTES;
        encoded[offset..offset + 4].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(validate_centroid_blob(&encoded, 8).is_ok());
        let error = decode_centroids(&encoded, 8).unwrap_err();
        assert!(error.to_string().contains("non-finite"));
    }

    #[test]
    fn kmeans_is_deterministic_and_order_independent() {
        let mut sample = Vec::new();
        for group in 0..4 {
            for index in 0..8 {
                let mut values = vec![0.0f32; 8];
                values[group] = 1.0;
                values[(group + 1) % 8] = 0.02 * index as f32;
                sample.push(IndexVector {
                    memory_id: format!("mem-{group}-{index:02}"),
                    values,
                });
            }
        }
        let seed = index_seed("prj", "embed", 8, 4, &ids(&sample));
        let first = build_kmeans(&sample, 4, seed).unwrap();
        let mut shuffled = sample.clone();
        shuffled.reverse();
        let second = build_kmeans(&shuffled, 4, seed).unwrap();
        assert_eq!(first.centroids, second.centroids);
        assert_eq!(first.assignments, second.assignments);
        assert_eq!(first.iterations, second.iterations);
        assert_eq!(first.centroids.len(), 4);
        for assignment in &first.assignments {
            assert!((assignment.1 as usize) < first.centroids.len());
        }
    }

    #[test]
    fn kmeans_assignments_are_stable_for_duplicate_vectors() {
        let sample = (0..12)
            .map(|index| IndexVector {
                memory_id: format!("mem-{index:02}"),
                values: vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            })
            .collect::<Vec<_>>();
        let build = build_kmeans(&sample, 4, 42).unwrap();
        // With identical vectors no distance mass exists after the first pick;
        // deterministic fallback selects the lowest remaining indices.
        assert_eq!(build.centroids.len(), 4);
        let distinct = build
            .assignments
            .iter()
            .map(|(_, cluster)| *cluster)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(distinct.len(), 1);
        let repeated = build_kmeans(&sample, 4, 42).unwrap();
        assert_eq!(build.assignments, repeated.assignments);
    }

    #[test]
    fn nearest_centroid_ties_break_by_lower_index() {
        let centroids = vec![
            vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            vec![0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        ];
        let vector = vec![1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        assert_eq!(assign_to_centroids(&centroids, &vector).unwrap(), 0);
        let ranked = rank_centroids(&centroids, &vector, 2).unwrap();
        assert_eq!(ranked, vec![0, 1]);
    }

    #[test]
    fn rank_centroids_validates_query_and_probe_bound() {
        let centroids = vec![
            vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            vec![0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            vec![0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        ];
        let query = vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        assert_eq!(rank_centroids(&centroids, &query, 1).unwrap(), vec![0]);
        assert_eq!(rank_centroids(&centroids, &query, 9).unwrap().len(), 3);
        let zero = vec![0.0f32; 8];
        assert!(rank_centroids(&centroids, &zero, 1).is_err());
        assert!(rank_centroids(&centroids, &[1.0, 0.0], 1).is_err());
        let nan = vec![f32::NAN, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        assert!(rank_centroids(&centroids, &nan, 1).is_err());
        assert!(rank_centroids(&[], &query, 1).is_err());
    }

    #[test]
    fn build_rejects_dimension_zero_finite_and_duplicate_id_violations() {
        let bad_dimensions = vec![
            IndexVector {
                memory_id: "a".to_string(),
                values: vec![1.0; 8],
            },
            IndexVector {
                memory_id: "b".to_string(),
                values: vec![1.0; 7],
            },
        ];
        assert!(build_kmeans(&bad_dimensions, 2, 1).is_err());
        let zero = vec![IndexVector {
            memory_id: "a".to_string(),
            values: vec![0.0; 8],
        }];
        assert!(build_kmeans(&zero, 1, 1).is_err());
        let nan = vec![IndexVector {
            memory_id: "a".to_string(),
            values: vec![f32::NAN; 8],
        }];
        assert!(build_kmeans(&nan, 1, 1).is_err());
        let duplicate = vec![
            IndexVector {
                memory_id: "a".to_string(),
                values: vec![1.0; 8],
            },
            IndexVector {
                memory_id: "a".to_string(),
                values: vec![1.0; 8],
            },
        ];
        assert!(build_kmeans(&duplicate, 1, 1).is_err());
        assert!(build_kmeans(&[], 1, 1).is_err());
        assert!(build_kmeans(&duplicate, MAX_IVF_NLIST + 1, 1).is_err());
    }

    #[test]
    fn seed_is_stable_and_changes_with_metadata() {
        let ids = vec!["a".to_string(), "b".to_string()];
        let first = index_seed("p", "m", 8, 4, &ids);
        assert_eq!(first, index_seed("p", "m", 8, 4, &ids));
        assert_ne!(first, index_seed("p2", "m", 8, 4, &ids));
        assert_ne!(first, index_seed("p", "m2", 8, 4, &ids));
        assert_ne!(first, index_seed("p", "m", 16, 4, &ids));
        assert_ne!(first, index_seed("p", "m", 8, 8, &ids));
    }

    #[test]
    fn status_validation_reports_each_explicit_fallback_reason() {
        let policy = policy_fixture();
        let counts = MemoryEmbeddingIndexCounts {
            vector_count: 8,
            assigned_vectors: 8,
        };
        let centroids =
            encode_centroids(8, &[vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]]).unwrap();
        let row = MemoryEmbeddingIndexRow {
            project_id: "prj".to_string(),
            model: "embed-test".to_string(),
            dimensions: 8,
            generation: 2,
            nlist: 4,
            cluster_count: 1,
            vector_count: 8,
            centroids: centroids.clone(),
            updated_at: "2026-11-11T00:00:00Z".to_string(),
            last_error: None,
            fallback_count: 0,
            last_fallback_at: None,
            last_fallback_reason: None,
        };
        assert!(validate_index(&policy, Some(&row), counts).ready);
        assert_eq!(
            validate_index(&policy, None, counts).reason.as_deref(),
            Some("not_built")
        );
        let mut scan = policy.clone();
        scan.index = MemoryIndexMode::Scan;
        assert_eq!(
            validate_index(&scan, Some(&row), counts).reason.as_deref(),
            Some("scan_mode")
        );
        let mut disabled = policy.clone();
        disabled.enabled = false;
        assert_eq!(
            validate_index(&disabled, Some(&row), counts)
                .reason
                .as_deref(),
            Some("embedding_disabled")
        );
        let below = MemoryEmbeddingIndexCounts {
            vector_count: 1,
            assigned_vectors: 1,
        };
        assert_eq!(
            validate_index(&policy, Some(&row), below).reason.as_deref(),
            Some("below_min_vectors")
        );
        let mut mismatch = row.clone();
        mismatch.model = "other".to_string();
        assert_eq!(
            validate_index(&policy, Some(&mismatch), counts)
                .reason
                .as_deref(),
            Some("model_mismatch")
        );
        let mut nlist = row.clone();
        nlist.nlist = 8;
        assert_eq!(
            validate_index(&policy, Some(&nlist), counts)
                .reason
                .as_deref(),
            Some("nlist_mismatch")
        );
        let mut corrupt = row.clone();
        corrupt.centroids[0] = b'X';
        assert!(
            validate_index(&policy, Some(&corrupt), counts)
                .reason
                .as_deref()
                .unwrap()
                .starts_with("index_corrupt")
        );
        let mut unassigned = counts;
        unassigned.assigned_vectors = 0;
        assert_eq!(
            validate_index(&policy, Some(&row), unassigned)
                .reason
                .as_deref(),
            Some("no_assignments")
        );
    }

    fn synthetic_cluster_fixture() -> (Vec<IndexVector>, Vec<Vec<f32>>) {
        let mut sample = Vec::new();
        let mut queries = Vec::new();
        for cluster in 0..8usize {
            for item in 0..32usize {
                let mut values = vec![0.004f32; 8];
                values[cluster] = 1.0;
                // Small deterministic variation keeps same-cluster points
                // close without making them identical.
                values[(cluster + 1) % 8] = 0.015 + item as f32 * 0.0005;
                values[(cluster + 2) % 8] = 0.008 + item as f32 * 0.0002;
                sample.push(IndexVector {
                    memory_id: format!("cluster-{cluster}-item-{item:02}"),
                    values,
                });
            }
            let mut query = vec![0.002f32; 8];
            query[cluster] = 1.0;
            queries.push(query);
        }
        (sample, queries)
    }

    fn top_k(scores: &mut Vec<(f64, String)>, k: usize) -> Vec<String> {
        scores.sort_by(|left, right| {
            right
                .0
                .total_cmp(&left.0)
                .then_with(|| left.1.cmp(&right.1))
        });
        scores.truncate(k);
        scores.iter().map(|(_, id)| id.clone()).collect()
    }

    fn fixture_recall_at_10(sample: &[IndexVector], queries: &[Vec<f32>], nprobe: u32) -> f64 {
        let seed = index_seed("recall-prj", "embed-recall", 8, 64, &ids(sample));
        let build = build_kmeans(sample, 64, seed).unwrap();
        assert_eq!(build.centroids.len(), 64);
        let values_by_id = sample
            .iter()
            .map(|vector| (vector.memory_id.clone(), vector.values.clone()))
            .collect::<std::collections::HashMap<_, _>>();
        let assignment_by_id = build
            .assignments
            .iter()
            .map(|(id, cluster)| (id.clone(), *cluster))
            .collect::<std::collections::HashMap<_, _>>();
        let k = 10usize;
        let mut hit_total = 0usize;
        let mut total = 0usize;
        for query in queries {
            let mut exact = sample
                .iter()
                .map(|vector| {
                    let similarity =
                        crate::memory::cosine_similarity(query, &values_by_id[&vector.memory_id])
                            .unwrap_or(0.0);
                    (similarity, vector.memory_id.clone())
                })
                .collect::<Vec<_>>();
            let exact_top = top_k(&mut exact, k);

            let probed = rank_centroids(&build.centroids, query, nprobe).unwrap();
            let probed_set = probed
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>();
            let mut ann = sample
                .iter()
                .filter(|vector| probed_set.contains(&assignment_by_id[&vector.memory_id]))
                .map(|vector| {
                    let similarity =
                        crate::memory::cosine_similarity(query, &values_by_id[&vector.memory_id])
                            .unwrap_or(0.0);
                    (similarity, vector.memory_id.clone())
                })
                .collect::<Vec<_>>();
            let ann_top = top_k(&mut ann, k);
            let hits = exact_top.iter().filter(|id| ann_top.contains(id)).count();
            hit_total += hits;
            total += k;
            assert!(ann_top.iter().all(|id| values_by_id.contains_key(id)));
        }
        hit_total as f64 / total as f64
    }

    #[test]
    fn ivf_recall_at_k_meets_the_documented_fixture_threshold() {
        let (sample, queries) = synthetic_cluster_fixture();
        let recall = fixture_recall_at_10(&sample, &queries, 8);
        assert!(
            recall >= 0.90,
            "fixture recall@10 must be >= 0.90 with nprobe=8, got {recall:.3}"
        );
        // No claim of exactness: this is a measured fixture, not a proof.
        assert!(recall <= 1.0);
    }

    #[test]
    fn probing_more_centroids_does_not_reduce_fixture_recall() {
        let (sample, queries) = synthetic_cluster_fixture();
        let narrow = fixture_recall_at_10(&sample, &queries, 1);
        let wide = fixture_recall_at_10(&sample, &queries, 8);
        assert!(
            wide + 1e-9 >= narrow,
            "nprobe=8 recall {wide:.3} should not trail nprobe=1 recall {narrow:.3}"
        );
        assert!((0.0..=1.0).contains(&narrow));
    }

    #[test]
    fn nlist_larger_than_the_sample_uses_one_centroid_per_vector() {
        let sample = (0..5)
            .map(|index| {
                let mut values = vec![0.01f32; 8];
                values[index] = 1.0;
                IndexVector {
                    memory_id: format!("mem-{index}"),
                    values,
                }
            })
            .collect::<Vec<_>>();
        let build = build_kmeans(&sample, 64, 7).unwrap();
        assert_eq!(build.centroids.len(), 5);
        let clusters = build
            .assignments
            .iter()
            .map(|(_, cluster)| *cluster)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(clusters.len(), 5);
        assert!(clusters.iter().all(|cluster| *cluster < 5));
    }

    #[test]
    fn centroid_encoding_supports_the_maximum_dimension() {
        let mut values = vec![0.0f32; MAX_EMBEDDING_DIMENSIONS as usize];
        values[0] = 1.0;
        let encoded = encode_centroids(MAX_EMBEDDING_DIMENSIONS, &[values.clone()]).unwrap();
        assert_eq!(
            encoded.len(),
            CENTROID_HEADER_BYTES + MAX_EMBEDDING_DIMENSIONS as usize * 4
        );
        assert_eq!(
            decode_centroids(&encoded, MAX_EMBEDDING_DIMENSIONS).unwrap(),
            vec![values]
        );
    }

    #[test]
    fn centroid_header_rejects_count_and_length_violations() {
        let valid = encode_centroids(8, &[vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]]).unwrap();
        let mut zero_count = valid.clone();
        zero_count[9..13].copy_from_slice(&0u32.to_le_bytes());
        assert!(validate_centroid_blob(&zero_count, 8).is_err());
        let mut huge_count = valid.clone();
        huge_count[9..13].copy_from_slice(&(MAX_IVF_NLIST + 1).to_le_bytes());
        assert!(validate_centroid_blob(&huge_count, 8).is_err());
        let mut extra = valid.clone();
        extra.push(0);
        assert!(validate_centroid_blob(&extra, 8).is_err());
        assert!(validate_centroid_blob(&[], 8).is_err());
        assert!(validate_centroid_blob(&valid, 16).is_err());
    }

    #[test]
    fn assignment_and_ranking_reject_dimension_mismatches() {
        let centroids = vec![vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]];
        assert!(assign_to_centroids(&centroids, &[1.0, 0.0]).is_err());
        let wrong_centroid = vec![vec![1.0, 0.0]];
        assert!(assign_to_centroids(&wrong_centroid, &[1.0; 8]).is_err());
        assert!(rank_centroids(&centroids, &[0.0; 8], 1).is_err());
        assert!(rank_centroids(&centroids, &[1.0; 9], 1).is_err());
        assert!(
            build_kmeans(
                &[IndexVector {
                    memory_id: "a".to_string(),
                    values: vec![1.0; 8],
                }],
                0,
                1
            )
            .is_err()
        );
    }

    #[test]
    fn status_validation_distinguishes_dimension_mismatch_and_metadata_corruption() {
        let policy = policy_fixture();
        let centroids =
            encode_centroids(8, &[vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]]).unwrap();
        let row = MemoryEmbeddingIndexRow {
            project_id: "prj".to_string(),
            model: "embed-test".to_string(),
            dimensions: 8,
            generation: 2,
            nlist: 4,
            cluster_count: 1,
            vector_count: 8,
            centroids,
            updated_at: "2026-11-11T00:00:00Z".to_string(),
            last_error: None,
            fallback_count: 0,
            last_fallback_at: None,
            last_fallback_reason: None,
        };
        let counts = MemoryEmbeddingIndexCounts {
            vector_count: 8,
            assigned_vectors: 8,
        };
        let mut dimensions = row.clone();
        dimensions.dimensions = 16;
        assert_eq!(
            validate_index(&policy, Some(&dimensions), counts)
                .reason
                .as_deref(),
            Some("dimensions_mismatch")
        );
        let mut count_mismatch = row.clone();
        count_mismatch.cluster_count = 2;
        assert!(
            validate_index(&policy, Some(&count_mismatch), counts)
                .reason
                .as_deref()
                .unwrap()
                .contains("does not match metadata")
        );
        let mut generation_zero = row.clone();
        generation_zero.generation = 0;
        assert_eq!(
            validate_index(&policy, Some(&generation_zero), counts)
                .reason
                .as_deref(),
            Some("not_built"),
            "a diagnostic row is never a usable generation"
        );
        // With enough live vectors, a generation-0 diagnostic row is still not built.
        let plenty = MemoryEmbeddingIndexCounts {
            vector_count: 300,
            assigned_vectors: 0,
        };
        assert_eq!(
            validate_index(&policy, Some(&generation_zero), plenty)
                .reason
                .as_deref(),
            Some("not_built")
        );
        assert_eq!(
            validate_index(&policy, None, plenty).reason.as_deref(),
            Some("not_built")
        );
    }

    fn index_row(
        model: &str,
        dimensions: u32,
        nlist: u32,
        vector_count: u32,
    ) -> MemoryEmbeddingIndexRow {
        MemoryEmbeddingIndexRow {
            project_id: "prj".to_string(),
            model: model.to_string(),
            dimensions,
            generation: 3,
            nlist,
            cluster_count: nlist,
            vector_count,
            centroids: b"blob".to_vec(),
            updated_at: "2026-11-11T00:00:00Z".to_string(),
            last_error: None,
            fallback_count: 0,
            last_fallback_at: None,
            last_fallback_reason: None,
        }
    }

    #[test]
    fn probe_candidate_limit_is_bounded_by_policy_and_hard_cap() {
        let mut policy = policy_fixture();
        policy.max_candidates = 64;
        policy.ivf_nprobe = 8;
        assert_eq!(probe_candidate_limit(&policy), 512);
        policy.ivf_nprobe = 1024;
        policy.max_candidates = 256;
        assert_eq!(probe_candidate_limit(&policy), 2_048);
        policy.max_candidates = 0;
        policy.ivf_nprobe = 0;
        assert_eq!(probe_candidate_limit(&policy), 1);
        policy.max_candidates = 999;
        policy.ivf_nprobe = 4;
        assert_eq!(probe_candidate_limit(&policy), 1_024);
    }

    #[test]
    fn audit_counts_combine_without_overflow() {
        let audit = IndexAudit {
            total: 10,
            invalid: 7,
            repaired: 3,
            assigned: 7,
        };
        assert_eq!(audit.total, audit.assigned + audit.repaired);
        let empty = IndexAudit::default();
        assert_eq!(empty.invalid, 0);
        let maintenance = IndexMaintenance::default();
        assert_eq!(maintenance.repaired_assignments, 0);
        assert_eq!(maintenance.invalid_assignments, 0);
        assert!(maintenance.rebuilt_generation.is_none());
    }

    #[test]
    fn rebuild_decision_covers_missing_corrupt_metadata_and_growth() {
        let policy = MemoryEmbeddingPolicy {
            ivf_rebuild_vectors: 16,
            ..policy_fixture()
        };
        assert!(should_rebuild(&policy, None, true, "embed-test", 8, 100));
        let row = index_row("embed-test", 8, 4, 12);
        assert!(should_rebuild(
            &policy,
            Some(&row),
            false,
            "embed-test",
            8,
            12
        ));
        assert!(!should_rebuild(
            &policy,
            Some(&row),
            true,
            "embed-test",
            8,
            12
        ));
        assert!(should_rebuild(&policy, Some(&row), true, "other", 8, 12));
        assert!(should_rebuild(
            &policy,
            Some(&row),
            true,
            "embed-test",
            16,
            12
        ));
        let mut nlist = row.clone();
        nlist.nlist = 8;
        assert!(should_rebuild(
            &policy,
            Some(&nlist),
            true,
            "embed-test",
            8,
            12
        ));

        // First crossing of the threshold.
        let below = index_row("embed-test", 8, 4, 8);
        assert!(should_rebuild(
            &policy,
            Some(&below),
            true,
            "embed-test",
            8,
            16
        ));
        // Growth smaller than another threshold is incremental assignment.
        let at = index_row("embed-test", 8, 4, 16);
        assert!(!should_rebuild(
            &policy,
            Some(&at),
            true,
            "embed-test",
            8,
            31
        ));
        // Another full threshold of growth rebuilds.
        assert!(should_rebuild(
            &policy,
            Some(&at),
            true,
            "embed-test",
            8,
            32
        ));
        // Fewer vectors than the threshold never triggers a growth rebuild.
        assert!(!should_rebuild(
            &policy,
            Some(&at),
            true,
            "embed-test",
            8,
            15
        ));
        // A large jump still respects the threshold arithmetic.
        assert!(should_rebuild(
            &policy,
            Some(&at),
            true,
            "embed-test",
            8,
            10_000
        ));
    }

    #[test]
    fn status_defaults_and_coverage_label_are_bounded() {
        let mut status = MemoryEmbeddingIndexStatus::default();
        assert_eq!(status.coverage_label(), "0.0%");
        status.coverage = 1.75;
        assert_eq!(status.coverage_label(), "100.0%");
        status.coverage = -0.3;
        assert_eq!(status.coverage_label(), "0.0%");
        assert_eq!(status.vector_count, 0);
        assert_eq!(status.invalid_assignments, 0);
        assert_eq!(status.fallback_count, 0);
        assert_eq!(status.mode, "scan");
        assert!(!status.ready);
    }

    #[test]
    fn default_index_status_is_scan_and_contains_no_vector_data() {
        let status = MemoryEmbeddingIndexStatus::default();
        assert_eq!(status.mode, "scan");
        assert!(!status.ready);
        assert_eq!(status.coverage_label(), "0.0%");
        let encoded = serde_json::to_string(&status).unwrap();
        assert!(!encoded.contains("VPIV"));
        assert!(!encoded.contains("centroid"));
    }

    #[test]
    fn policy_uses_ivf_requires_all_three_conditions() {
        let mut policy = policy_fixture();
        assert!(policy.uses_ivf_index());
        policy.enabled = false;
        assert!(!policy.uses_ivf_index());
        let mut policy = policy_fixture();
        policy.endpoint = None;
        assert!(!policy.uses_ivf_index());
        let mut policy = policy_fixture();
        policy.index = MemoryIndexMode::Scan;
        assert!(!policy.uses_ivf_index());
    }

    #[test]
    fn vector_helper_tracks_memory_ids_stably() {
        let sample = vec![vector(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0])];
        let mut ordered = ids(&sample);
        ordered.sort();
        assert_eq!(ordered.len(), 1);
    }
}
