//! Deterministic, opt-in source IVF index.
//!
//! The exact cosine scan remains the default and the fallback. This module
//! only adds a bounded candidate lane: when a project explicitly enables
//! `ann_enabled` and has at least `ann_min_vectors` current-generation
//! vectors, a derived k-means index is built from a bounded sample. Recall
//! probes the nearest centroids, then scores every probed candidate with the
//! same exact cosine function used by the scan. Lexical candidates, fusion, and
//! rerank are unchanged.
//!
//! Invariants:
//!
//! - Derived operational state. The index is not an event, not a replay
//!   projection, and can be deleted and rebuilt from `source_embeddings`.
//! - No vector, centroid, content hash, or embedded text crosses an RPC, event,
//!   log, or UI boundary. Only aggregate counts and generations are exposed.
//! - k-means is deterministic: input is sorted by chunk id, initialization is
//!   seeded from a SHA-256 of project/model/dimensions/nlist plus that stable
//!   ordering, iterations are bounded, and ties break by lower centroid index
//!   (then chunk id for assignments).
//! - Every entry point validates dimensions, finiteness, non-zero norm, the
//!   centroid header, exact byte length, and metadata consistency. Corrupt or
//!   stale state fails closed to the exact scan with an explicit reason.
//! - No universal recall guarantee is claimed. The recall fixture measures
//!   recall@k against the exact scan; real recall depends on the embedding
//!   distribution, `nlist`, and `ann_nprobe`.
//!
//! Centroid encoding (`versioned little-endian f32`):
//!
//! ```text
//! bytes 0..4   magic "VPSA"
//! byte  4      version (1)
//! bytes 5..9   dimensions, little-endian u32
//! bytes 9..13  centroid count, little-endian u32
//! bytes 13..   count * dimensions little-endian f32 values
//! ```

use anyhow::{Result, anyhow, bail};
use vp_protocol::types::SourceRetrievalPolicy;
use vp_store::Store;
use vp_store::store::{
    SourceAnnAssignmentAudit, SourceAnnCounts, SourceAnnIndexRow, SourceSemanticCandidate,
};

use crate::memory::{MAX_EMBEDDING_DIMENSIONS, MIN_EMBEDDING_DIMENSIONS, decode_embedding_vector};
use crate::memory_index::{assign_to_centroids, rank_centroids};

pub const CENTROID_MAGIC: [u8; 4] = *b"VPSA";
pub const CENTROID_VERSION: u8 = 1;
pub const CENTROID_HEADER_BYTES: usize = 13;

/// Hard centroid ceiling. Source corpora are bounded by
/// `max_chunks_per_source` (500) times the number of sources, so 256 clusters
/// is already finer than most projects need.
pub const MAX_SOURCE_ANN_NLIST: u32 = 256;
/// Training reads at most this many vectors. A project with a very large
/// corpus trains on a bounded, stable prefix by chunk id.
pub const MAX_SOURCE_ANN_SAMPLE_VECTORS: u32 = 4_096;
/// Training also stops at this many vector bytes so a high-dimensional
/// embedding cannot allocate without bound.
pub const MAX_SOURCE_ANN_SAMPLE_BYTES: u64 = 8 * 1024 * 1024;
/// Candidate window produced by a probe before cosine scoring. The exact scan
/// stays at 512; ANN is allowed a wider window because it is already bounded
/// by cluster count and is used only when the operator opts in.
pub const MAX_SOURCE_ANN_CANDIDATES: u32 = 1_024;
/// Incremental assignment cap for one daemon maintenance tick.
pub const MAX_SOURCE_ANN_ASSIGNMENTS_PER_TICK: u32 = 1_024;
/// Rebuild assignment cap. A rebuild performs at most this many assignments;
/// later ticks catch up incrementally.
pub const MAX_SOURCE_ANN_ASSIGNMENTS_PER_REBUILD: u32 = 20_000;
pub const MAX_SOURCE_ANN_KMEANS_ITERATIONS: u32 = 16;
pub const MAX_SOURCE_ANN_ASSIGNMENT_BATCH: u32 = 256;
/// A rebuild refreshing centroids when the live vector count has grown by at
/// least this many vectors over the stored build, or by 50%, whichever is
/// larger. Incremental assignment keeps the index usable in between.
pub const SOURCE_ANN_REBUILD_MIN_GROWTH: u32 = 64;

/// One indexed vector supplied to a k-means build. The chunk id is the stable
/// tie-break and ordering key.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceIndexVector {
    pub chunk_id: String,
    pub values: Vec<f32>,
}

/// Deterministic k-means outcome. `assignments` is parallel to the sorted
/// sample; the caller persists it keyed by chunk id.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceKMeansBuild {
    pub dimensions: u32,
    pub centroids: Vec<Vec<f32>>,
    pub assignments: Vec<(String, u32)>,
    pub iterations: u32,
}

/// Successful probe metadata. Candidates are exact-scored by the caller.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceAnnReady {
    pub generation: u64,
    pub probed_clusters: u32,
    pub candidates: Vec<SourceSemanticCandidate>,
}

/// Explicit fallback. A fallback is not an error: the caller performs the exact
/// scan and keeps semantic retrieval active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceAnnFallback {
    pub reason: String,
}

impl SourceAnnFallback {
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: sanitize_reason(&reason.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SourceAnnProbe {
    Ready(SourceAnnReady),
    Fallback(SourceAnnFallback),
}

/// Read-only status for the derived index and the effective next-search path.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceAnnStatus {
    pub ready: bool,
    pub path: String,
    pub generation: Option<u64>,
    pub model: Option<String>,
    pub dimensions: u32,
    pub vector_count: u32,
    pub assigned_vectors: u32,
    pub invalid_assignments: u32,
    pub centroid_count: u32,
    pub nlist: u32,
    pub nprobe: u32,
    pub min_vectors: u32,
    pub coverage: f64,
    pub bytes: u64,
    pub fallback_reason: Option<String>,
    pub last_rebuilt_at: Option<String>,
    pub last_error: Option<String>,
}

/// Outcome of one bounded maintenance pass.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourceAnnMaintenance {
    pub rebuilt_generation: Option<u64>,
    pub assigned_vectors: u32,
    pub repaired_assignments: u32,
    pub invalid_assignments: u32,
    pub generation: Option<u64>,
    pub vector_count: u32,
    pub centroid_count: u32,
}

/// Outcome of an explicit rebuild request.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourceAnnRebuildReport {
    pub rebuilt: bool,
    pub reason: Option<String>,
    pub generation: Option<u64>,
    pub vector_count: u32,
    pub assigned_vectors: u32,
    pub centroid_count: u32,
    pub nlist: u32,
    pub nprobe: u32,
    pub path: String,
}

/// Structural readiness of the stored index for the current policy and live
/// vector count. `reason` is machine-readable and safe to expose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceAnnValidation {
    pub ready: bool,
    pub reason: Option<String>,
    pub generation: Option<u64>,
}

#[derive(Debug, Clone)]
struct PreparedVector {
    chunk_id: String,
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
    if centroids.len() > MAX_SOURCE_ANN_NLIST as usize {
        bail!("centroid count exceeds the {MAX_SOURCE_ANN_NLIST} ceiling");
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
    if count == 0 || count > MAX_SOURCE_ANN_NLIST {
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
pub fn source_ann_seed(
    project_id: &str,
    model: &str,
    dimensions: u32,
    nlist: u32,
    sorted_chunk_ids: &[String],
) -> u64 {
    let mut ordered = sorted_chunk_ids.to_vec();
    ordered.sort();
    let mut material = String::with_capacity(80 + ordered.len() * 48);
    material.push_str("verlinpark-source-ivf-v2");
    material.push('\u{1f}');
    material.push_str(project_id);
    material.push('\u{1f}');
    material.push_str(model);
    material.push('\u{1f}');
    material.push_str(&dimensions.to_string());
    material.push('\u{1f}');
    material.push_str(&nlist.to_string());
    material.push('\u{1f}');
    for chunk_id in &ordered {
        material.push_str(chunk_id);
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

/// Deterministic derived cluster target. `sqrt(n)` clusters are a reasonable
/// default for a bounded IVF and keep centroid storage small. The stored
/// `nlist` is diagnostic; validation does not reject a derived change because
/// operators never configure it directly.
pub fn source_nlist(vector_count: u32) -> u32 {
    let target = (f64::from(vector_count).sqrt().ceil() as u32).max(1);
    target.clamp(1, MAX_SOURCE_ANN_NLIST)
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
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn prepare_vector(chunk_id: &str, values: &[f32], dimensions: u32) -> Result<PreparedVector> {
    if values.len() != dimensions as usize {
        bail!(
            "vector {chunk_id} has {} dimensions, expected {dimensions}",
            values.len()
        );
    }
    if values.iter().any(|value| !value.is_finite()) {
        bail!("vector {chunk_id} contains a non-finite value");
    }
    let norm = values
        .iter()
        .map(|value| f64::from(*value).powi(2))
        .sum::<f64>();
    if norm <= 0.0 || !norm.is_finite() {
        bail!("vector {chunk_id} has zero norm");
    }
    Ok(PreparedVector {
        chunk_id: chunk_id.to_string(),
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

fn nearest_centroid(prepared: &PreparedVector, centroids: &[Vec<f32>]) -> Result<u32> {
    let mut best = None::<(f64, u32)>;
    for (index, centroid) in centroids.iter().enumerate() {
        if centroid.len() != prepared.values.len() {
            bail!("centroid dimensions do not match the assignment vector");
        }
        let similarity = prepared_cosine(
            &prepared.values,
            prepared.norm,
            centroid,
            centroid_norm(centroid),
        )
        .ok_or_else(|| anyhow!("centroid {index} cannot be scored"))?;
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

/// Build deterministic spherical k-means centroids from a bounded sample.
///
/// The sample is sorted by chunk id before initialization so input order
/// cannot change the result. k-means++ uses a deterministic splitmix64 stream
/// seeded from stable project/model/dimension/nlist metadata. Empty clusters
/// retain their previous centroid; no random restart occurs. The objective is
/// cosine similarity, matching recall scoring.
pub fn build_kmeans(
    sample: &[SourceIndexVector],
    nlist: u32,
    seed: u64,
) -> Result<SourceKMeansBuild> {
    if sample.is_empty() {
        bail!("cannot build an index from an empty sample");
    }
    if !(1..=MAX_SOURCE_ANN_NLIST).contains(&nlist) {
        bail!("nlist must be between 1 and {MAX_SOURCE_ANN_NLIST}");
    }
    let dimensions = u32::try_from(sample[0].values.len())
        .map_err(|_| anyhow!("sample dimensions exceed u32"))?;
    if !(MIN_EMBEDDING_DIMENSIONS..=MAX_EMBEDDING_DIMENSIONS).contains(&dimensions) {
        bail!(
            "sample dimensions must be between {MIN_EMBEDDING_DIMENSIONS} and {MAX_EMBEDDING_DIMENSIONS}"
        );
    }
    let mut ordered = sample.to_vec();
    ordered.sort_by(|left, right| left.chunk_id.cmp(&right.chunk_id));
    for pair in ordered.windows(2) {
        if pair[0].chunk_id == pair[1].chunk_id {
            bail!("sample contains a duplicate chunk id: {}", pair[0].chunk_id);
        }
    }
    let prepared = ordered
        .iter()
        .map(|vector| prepare_vector(&vector.chunk_id, &vector.values, dimensions))
        .collect::<Result<Vec<_>>>()?;
    let count = prepared.len();
    let k = (nlist as usize)
        .clamp(1, MAX_SOURCE_ANN_NLIST as usize)
        .min(count);
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
    while iterations < MAX_SOURCE_ANN_KMEANS_ITERATIONS {
        iterations += 1;
        let mut changed = false;
        for (index, vector) in prepared.iter().enumerate() {
            let cluster = nearest_centroid(vector, &centroids)?;
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
        for (index, vector) in prepared.iter().enumerate() {
            assignments[index] = nearest_centroid(vector, &centroids)?;
        }
    }
    Ok(SourceKMeansBuild {
        dimensions,
        centroids,
        assignments: prepared
            .into_iter()
            .zip(assignments)
            .map(|(vector, cluster)| (vector.chunk_id, cluster))
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

/// Assign one decoded vector to its nearest centroid using the same stable
/// tie-break as recall. Used by maintenance tests and by the assignment loop.
pub fn assign_vector(centroids: &[Vec<f32>], vector: &[f32]) -> Result<u32> {
    assign_to_centroids(centroids, vector)
}

/// Validate a stored index against the current policy and live counts. The
/// order is intentional: a configured minimum that is not met is reported as
/// `below_min_vectors` even when an old blob is also corrupt, because that is
/// the actionable operator state.
pub fn validate_index(
    policy: &SourceRetrievalPolicy,
    embedding_enabled: bool,
    embedding_configured: bool,
    row: Option<&SourceAnnIndexRow>,
    counts: SourceAnnCounts,
    vector_count: u32,
) -> SourceAnnValidation {
    let generation = row
        .map(|row| row.generation)
        .filter(|generation| *generation > 0);
    if !embedding_enabled {
        return SourceAnnValidation {
            ready: false,
            reason: Some("embedding_disabled".to_string()),
            generation,
        };
    }
    if !embedding_configured {
        return SourceAnnValidation {
            ready: false,
            reason: Some("not_configured".to_string()),
            generation,
        };
    }
    if !policy.ann_enabled {
        return SourceAnnValidation {
            ready: false,
            reason: Some("ann_disabled".to_string()),
            generation,
        };
    }
    if vector_count < policy.ann_min_vectors {
        return SourceAnnValidation {
            ready: false,
            reason: Some("below_min_vectors".to_string()),
            generation,
        };
    }
    let Some(row) = row else {
        return SourceAnnValidation {
            ready: false,
            reason: Some("not_built".to_string()),
            generation: None,
        };
    };
    if row.generation == 0 {
        return SourceAnnValidation {
            ready: false,
            reason: Some("not_built".to_string()),
            generation: None,
        };
    }
    if row.model.is_empty() {
        return SourceAnnValidation {
            ready: false,
            reason: Some("model_mismatch".to_string()),
            generation,
        };
    }
    if counts.vector_count < policy.ann_min_vectors {
        return SourceAnnValidation {
            ready: false,
            reason: Some("below_min_vectors".to_string()),
            generation,
        };
    }
    let cluster_count = match validate_centroid_blob(&row.centroids, row.dimensions) {
        Ok(count) => count,
        Err(error) => {
            return SourceAnnValidation {
                ready: false,
                reason: Some(format!(
                    "index_corrupt: {}",
                    sanitize_reason(&error.to_string())
                )),
                generation,
            };
        }
    };
    if cluster_count != row.centroid_count {
        return SourceAnnValidation {
            ready: false,
            reason: Some("index_corrupt: centroid count does not match metadata".to_string()),
            generation,
        };
    }
    if counts.assigned_vectors == 0 {
        return SourceAnnValidation {
            ready: false,
            reason: Some("no_assignments".to_string()),
            generation,
        };
    }
    SourceAnnValidation {
        ready: true,
        reason: None,
        generation,
    }
}

/// Per-cluster candidate limit for one probe. Each probed cluster contributes a
/// bounded share; the caller hard-truncates the merged window.
pub fn candidate_limit_per_cluster(candidate_limit: u32, nprobe: u32) -> u32 {
    let nprobe = nprobe.clamp(1, 32);
    (candidate_limit.max(1) / nprobe)
        .clamp(32, 256)
        .min(MAX_SOURCE_ANN_CANDIDATES)
}

/// Probe the index for one query. Store errors and validation failures are
/// converted into a fallback reason so a recall never fails because derived
/// index state is unhealthy. The caller performs the exact scan.
#[allow(clippy::too_many_arguments)]
pub fn probe_source_ann(
    store: &Store,
    project_id: &str,
    policy: &SourceRetrievalPolicy,
    model: &str,
    dimensions: u32,
    max_chunks: u32,
    source_ids: &[String],
    collections: &[String],
    labels: &[String],
    query_vector: &[f32],
    candidate_limit: u32,
) -> SourceAnnProbe {
    if !policy.ann_enabled {
        return SourceAnnProbe::Fallback(SourceAnnFallback::new("ann_disabled"));
    }
    if model.trim().is_empty() {
        return SourceAnnProbe::Fallback(SourceAnnFallback::new("not_configured"));
    }
    if query_vector.len() != dimensions as usize
        || query_vector.is_empty()
        || query_vector.iter().any(|value| !value.is_finite())
    {
        return SourceAnnProbe::Fallback(SourceAnnFallback::new("query_invalid"));
    }
    let vector_count =
        match store.source_embedding_ready_count(project_id, model, dimensions, max_chunks) {
            Ok(count) => count,
            Err(error) => {
                return SourceAnnProbe::Fallback(SourceAnnFallback::new(format!(
                    "index_read_error: {}",
                    sanitize_reason(&error.to_string())
                )));
            }
        };
    if vector_count < policy.ann_min_vectors {
        return SourceAnnProbe::Fallback(SourceAnnFallback::new("below_min_vectors"));
    }
    let row = match store.source_ann_index(project_id) {
        Ok(row) => row,
        Err(error) => {
            return SourceAnnProbe::Fallback(SourceAnnFallback::new(format!(
                "index_read_error: {}",
                sanitize_reason(&error.to_string())
            )));
        }
    };
    let Some(row) = row else {
        return SourceAnnProbe::Fallback(SourceAnnFallback::new("not_built"));
    };
    let counts = match store.source_ann_counts(
        project_id,
        model,
        dimensions,
        max_chunks,
        row.generation,
        row.centroid_count,
    ) {
        Ok(counts) => counts,
        Err(error) => {
            return SourceAnnProbe::Fallback(SourceAnnFallback::new(format!(
                "index_read_error: {}",
                sanitize_reason(&error.to_string())
            )));
        }
    };
    if row.model != model {
        return SourceAnnProbe::Fallback(SourceAnnFallback::new("model_mismatch"));
    }
    if row.dimensions != dimensions {
        return SourceAnnProbe::Fallback(SourceAnnFallback::new("dimensions_mismatch"));
    }
    let validation = validate_index(policy, true, true, Some(&row), counts, vector_count);
    if !validation.ready {
        return SourceAnnProbe::Fallback(SourceAnnFallback::new(
            validation.reason.unwrap_or_else(|| "not_ready".to_string()),
        ));
    }
    let centroids = match decode_centroids(&row.centroids, dimensions) {
        Ok(centroids) => centroids,
        Err(error) => {
            return SourceAnnProbe::Fallback(SourceAnnFallback::new(format!(
                "index_corrupt: {}",
                sanitize_reason(&error.to_string())
            )));
        }
    };
    let probed = match rank_centroids(&centroids, query_vector, policy.ann_nprobe) {
        Ok(probed) => probed,
        Err(error) => {
            return SourceAnnProbe::Fallback(SourceAnnFallback::new(format!(
                "query_invalid: {}",
                sanitize_reason(&error.to_string())
            )));
        }
    };
    let per_cluster = candidate_limit_per_cluster(candidate_limit, policy.ann_nprobe);
    let candidates = match store.source_ann_candidates(
        project_id,
        model,
        dimensions,
        max_chunks,
        row.generation,
        &probed,
        source_ids,
        collections,
        labels,
        per_cluster,
    ) {
        Ok(candidates) => candidates,
        Err(error) => {
            return SourceAnnProbe::Fallback(SourceAnnFallback::new(format!(
                "index_read_error: {}",
                sanitize_reason(&error.to_string())
            )));
        }
    };
    let mut candidates = candidates;
    candidates.truncate(candidate_limit.max(1) as usize);
    SourceAnnProbe::Ready(SourceAnnReady {
        generation: row.generation,
        probed_clusters: u32::try_from(probed.len()).unwrap_or(u32::MAX),
        candidates,
    })
}

/// Read-only status with no vector or centroid values. Header validation is
/// enough to report corruption.
#[allow(clippy::too_many_arguments)]
pub fn source_ann_status(
    store: &Store,
    project_id: &str,
    policy: &SourceRetrievalPolicy,
    embedding_enabled: bool,
    embedding_configured: bool,
    model: Option<&str>,
    dimensions: u32,
    max_chunks: u32,
) -> Result<SourceAnnStatus> {
    let row = store.source_ann_index(project_id)?;
    let Some(model) = model.map(str::trim).filter(|value| !value.is_empty()) else {
        let reason = if !embedding_enabled {
            "embedding_disabled"
        } else {
            "not_configured"
        };
        return Ok(SourceAnnStatus {
            ready: false,
            path: "exact".to_string(),
            generation: row
                .as_ref()
                .map(|row| row.generation)
                .filter(|generation| *generation > 0),
            model: None,
            dimensions,
            vector_count: 0,
            assigned_vectors: 0,
            invalid_assignments: 0,
            centroid_count: row.as_ref().map(|row| row.centroid_count).unwrap_or(0),
            nlist: row
                .as_ref()
                .map(|row| row.nlist)
                .unwrap_or_else(|| source_nlist(0)),
            nprobe: policy.ann_nprobe,
            min_vectors: policy.ann_min_vectors,
            coverage: 0.0,
            bytes: row
                .as_ref()
                .map(|row| u64::try_from(row.centroids.len()).unwrap_or(u64::MAX))
                .unwrap_or(0),
            fallback_reason: Some(reason.to_string()),
            last_rebuilt_at: row
                .as_ref()
                .filter(|row| row.generation > 0)
                .map(|row| row.updated_at.clone()),
            last_error: row
                .as_ref()
                .and_then(|row| row.last_error.as_deref())
                .map(sanitize_reason),
        });
    };
    let vector_count =
        store.source_embedding_ready_count(project_id, model, dimensions, max_chunks)?;
    let generation = row.as_ref().map(|row| row.generation).unwrap_or(0);
    let counts = store.source_ann_counts(
        project_id,
        model,
        dimensions,
        max_chunks,
        generation,
        row.as_ref().map(|row| row.centroid_count).unwrap_or(0),
    )?;
    let validation = validate_index(
        policy,
        embedding_enabled,
        embedding_configured,
        row.as_ref(),
        counts,
        vector_count,
    );
    let nlist = row
        .as_ref()
        .map(|row| row.nlist)
        .unwrap_or_else(|| source_nlist(vector_count));
    Ok(SourceAnnStatus {
        ready: validation.ready,
        path: if validation.ready { "ann" } else { "exact" }.to_string(),
        generation: validation.generation,
        model: Some(model.to_string()),
        dimensions,
        vector_count,
        assigned_vectors: counts.assigned_vectors,
        invalid_assignments: counts.invalid_assignments,
        centroid_count: row.as_ref().map(|row| row.centroid_count).unwrap_or(0),
        nlist,
        nprobe: policy.ann_nprobe,
        min_vectors: policy.ann_min_vectors,
        coverage: if vector_count == 0 {
            0.0
        } else {
            f64::from(counts.assigned_vectors) / f64::from(vector_count)
        },
        bytes: row
            .as_ref()
            .map(|row| u64::try_from(row.centroids.len()).unwrap_or(u64::MAX))
            .unwrap_or(0),
        fallback_reason: validation.reason,
        last_rebuilt_at: row
            .as_ref()
            .filter(|row| row.generation > 0)
            .map(|row| row.updated_at.clone()),
        last_error: row
            .as_ref()
            .and_then(|row| row.last_error.as_deref())
            .map(sanitize_reason),
    })
}

/// Bounded maintenance for one enabled ANN project: rebuild when metadata, the
/// generation, or the growth threshold requires it; otherwise assign
/// missing/stale vectors to the current centroids. Concurrent embedding writes
/// are never blocked; a vector written after the sample is picked up by a later
/// assignment pass.
pub fn maintain_source_ann(
    store: &Store,
    project_id: &str,
    policy: &SourceRetrievalPolicy,
    model: &str,
    dimensions: u32,
    max_chunks: u32,
) -> Result<SourceAnnMaintenance> {
    if !policy.ann_enabled || model.trim().is_empty() {
        return Ok(SourceAnnMaintenance::default());
    }
    let vector_count =
        store.source_embedding_ready_count(project_id, model, dimensions, max_chunks)?;
    let row = store.source_ann_index(project_id)?;
    if vector_count < policy.ann_min_vectors {
        if row.as_ref().is_some_and(|row| {
            row.model == model && row.dimensions == dimensions && row.generation > 0
        }) {
            store.delete_source_ann(project_id)?;
        }
        return Ok(SourceAnnMaintenance {
            vector_count,
            ..SourceAnnMaintenance::default()
        });
    }
    let decoded = row
        .as_ref()
        .and_then(|row| decode_centroids(&row.centroids, dimensions).ok());
    let rebuild = should_rebuild(
        row.as_ref(),
        decoded.is_some(),
        model,
        dimensions,
        vector_count,
    );
    if rebuild {
        let report = rebuild_source_ann(store, project_id, policy, model, dimensions, max_chunks)?;
        return Ok(SourceAnnMaintenance {
            rebuilt_generation: report.generation,
            assigned_vectors: report.assigned_vectors,
            generation: report.generation,
            vector_count: report.vector_count,
            centroid_count: report.centroid_count,
            ..SourceAnnMaintenance::default()
        });
    }
    let (Some(row), Some(centroids)) = (row, decoded) else {
        let report = rebuild_source_ann(store, project_id, policy, model, dimensions, max_chunks)?;
        return Ok(SourceAnnMaintenance {
            rebuilt_generation: report.generation,
            assigned_vectors: report.assigned_vectors,
            generation: report.generation,
            vector_count: report.vector_count,
            centroid_count: report.centroid_count,
            ..SourceAnnMaintenance::default()
        });
    };
    let audit = audit_assignments(
        store,
        project_id,
        model,
        dimensions,
        max_chunks,
        row.generation,
        row.centroid_count,
    )?;
    let assigned = assign_pending(
        store,
        project_id,
        model,
        dimensions,
        max_chunks,
        row.generation,
        &centroids,
        MAX_SOURCE_ANN_ASSIGNMENTS_PER_TICK,
    )?;
    let counts = store.source_ann_counts(
        project_id,
        model,
        dimensions,
        max_chunks,
        row.generation,
        row.centroid_count,
    )?;
    Ok(SourceAnnMaintenance {
        rebuilt_generation: None,
        assigned_vectors: assigned,
        repaired_assignments: audit.repaired,
        invalid_assignments: audit.invalid.saturating_sub(audit.repaired),
        generation: Some(row.generation),
        vector_count: counts.vector_count,
        centroid_count: u32::try_from(centroids.len()).unwrap_or(u32::MAX),
    })
}

/// Pure rebuild decision shared by maintenance and tests. A missing,
/// undecodable, or metadata-mismatched generation always rebuilds. Growth by
/// at least 64 vectors or 50% refreshes centroids; smaller changes are handled
/// by incremental assignment.
fn should_rebuild(
    row: Option<&SourceAnnIndexRow>,
    decoded: bool,
    model: &str,
    dimensions: u32,
    vector_count: u32,
) -> bool {
    let Some(row) = row else {
        return true;
    };
    if !decoded || row.generation == 0 {
        return true;
    }
    if row.model != model || row.dimensions != dimensions {
        return true;
    }
    let growth = (row.vector_count / 2).max(SOURCE_ANN_REBUILD_MIN_GROWTH);
    vector_count >= row.vector_count.saturating_add(growth)
}

/// Explicit operator rebuild. Unlike maintenance this always creates a new
/// generation. It also clears state for disabled, unconfigured, or
/// below-minimum projects so the operator receives an honest result.
pub fn rebuild_source_ann(
    store: &Store,
    project_id: &str,
    policy: &SourceRetrievalPolicy,
    model: &str,
    dimensions: u32,
    max_chunks: u32,
) -> Result<SourceAnnRebuildReport> {
    if !policy.ann_enabled {
        let _ = store.delete_source_ann(project_id)?;
        return Ok(SourceAnnRebuildReport {
            reason: Some("ann_disabled".to_string()),
            nprobe: policy.ann_nprobe,
            path: "exact".to_string(),
            ..SourceAnnRebuildReport::default()
        });
    }
    if model.trim().is_empty() {
        return Ok(SourceAnnRebuildReport {
            reason: Some("not_configured".to_string()),
            nprobe: policy.ann_nprobe,
            path: "exact".to_string(),
            ..SourceAnnRebuildReport::default()
        });
    }
    let vector_count =
        store.source_embedding_ready_count(project_id, model, dimensions, max_chunks)?;
    if vector_count < policy.ann_min_vectors {
        store.delete_source_ann(project_id)?;
        return Ok(SourceAnnRebuildReport {
            reason: Some("below_min_vectors".to_string()),
            vector_count,
            nprobe: policy.ann_nprobe,
            path: "exact".to_string(),
            ..SourceAnnRebuildReport::default()
        });
    }
    let rows = store.source_ann_training_vectors(
        project_id,
        model,
        dimensions,
        max_chunks,
        MAX_SOURCE_ANN_SAMPLE_VECTORS,
        MAX_SOURCE_ANN_SAMPLE_BYTES,
    )?;
    if rows.is_empty() {
        store.delete_source_ann(project_id)?;
        return Ok(SourceAnnRebuildReport {
            reason: Some("no_vectors".to_string()),
            vector_count,
            nprobe: policy.ann_nprobe,
            path: "exact".to_string(),
            ..SourceAnnRebuildReport::default()
        });
    }
    let vectors = rows
        .iter()
        .map(|row| {
            decode_embedding_vector(&row.vector, dimensions).map(|values| SourceIndexVector {
                chunk_id: row.chunk_id.clone(),
                values,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let target_nlist = source_nlist(vector_count).min(MAX_SOURCE_ANN_NLIST);
    let mut sorted_ids = vectors
        .iter()
        .map(|vector| vector.chunk_id.clone())
        .collect::<Vec<_>>();
    sorted_ids.sort();
    let seed = source_ann_seed(project_id, model, dimensions, target_nlist, &sorted_ids);
    let target_clusters = target_nlist.min(u32::try_from(vectors.len()).unwrap_or(u32::MAX));
    let build = build_kmeans(&vectors, target_clusters, seed)?;
    let centroids = encode_centroids(dimensions, &build.centroids)?;
    let cluster_count = u32::try_from(build.centroids.len()).unwrap_or(u32::MAX);
    let current_generation = store
        .source_ann_index(project_id)?
        .map(|row| row.generation)
        .unwrap_or(0);
    let generation = store.save_source_ann_index(
        project_id,
        model,
        dimensions,
        target_nlist,
        cluster_count,
        vector_count,
        &centroids,
        Some(current_generation),
    )?;
    let Some(generation) = generation else {
        return Ok(SourceAnnRebuildReport {
            reason: Some("generation_conflict".to_string()),
            vector_count,
            nprobe: policy.ann_nprobe,
            path: "exact".to_string(),
            ..SourceAnnRebuildReport::default()
        });
    };
    let assigned = assign_pending(
        store,
        project_id,
        model,
        dimensions,
        max_chunks,
        generation,
        &build.centroids,
        MAX_SOURCE_ANN_ASSIGNMENTS_PER_REBUILD,
    )?;
    let counts = store.source_ann_counts(
        project_id,
        model,
        dimensions,
        max_chunks,
        generation,
        cluster_count,
    )?;
    Ok(SourceAnnRebuildReport {
        rebuilt: true,
        reason: None,
        generation: Some(generation),
        vector_count: counts.vector_count,
        assigned_vectors: assigned,
        centroid_count: cluster_count,
        nlist: target_nlist,
        nprobe: policy.ann_nprobe,
        path: "ann".to_string(),
    })
}

/// Audit and repair assignment rows for the current generation. Invalid rows
/// are deleted in one bounded statement, then normal assignment maintenance
/// refills coverage. Recall never performs this repair.
pub fn audit_assignments(
    store: &Store,
    project_id: &str,
    model: &str,
    dimensions: u32,
    max_chunks: u32,
    generation: u64,
    centroid_count: u32,
) -> Result<SourceAnnAssignmentAudit> {
    let audit = store.source_ann_assignment_audit(
        project_id,
        model,
        dimensions,
        max_chunks,
        generation,
        centroid_count,
    )?;
    let repaired = if audit.invalid() > 0 {
        store.delete_invalid_source_ann_assignments(
            project_id,
            model,
            dimensions,
            max_chunks,
            generation,
            centroid_count,
        )?
    } else {
        0
    };
    Ok(SourceAnnAssignmentAudit {
        total: audit.total,
        invalid: audit.invalid(),
        repaired,
    })
}

#[allow(clippy::too_many_arguments)]
fn assign_pending(
    store: &Store,
    project_id: &str,
    model: &str,
    dimensions: u32,
    max_chunks: u32,
    generation: u64,
    centroids: &[Vec<f32>],
    cap: u32,
) -> Result<u32> {
    let mut remaining = cap;
    let mut assigned = 0u32;
    while remaining > 0 {
        let batch_limit = remaining.min(MAX_SOURCE_ANN_ASSIGNMENT_BATCH);
        let candidates = store.source_ann_unassigned_vectors(
            project_id,
            model,
            dimensions,
            max_chunks,
            generation,
            batch_limit,
        )?;
        if candidates.is_empty() {
            break;
        }
        let mut assignments = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let values = decode_embedding_vector(&candidate.vector, dimensions)?;
            let cluster = assign_vector(centroids, &values)?;
            assignments.push((candidate.chunk_id, cluster));
        }
        let saved = store.save_source_ann_assignments(
            project_id,
            model,
            dimensions,
            generation,
            &assignments,
        )?;
        if !saved {
            break;
        }
        let count = u32::try_from(assignments.len()).unwrap_or(u32::MAX);
        assigned = assigned.saturating_add(count);
        remaining = remaining.saturating_sub(count);
    }
    Ok(assigned)
}

/// Validate and normalize a source vector for indexing tests. Mirrors the exact
/// cosine header rules used by the embedding store.
pub fn decode_source_vector(bytes: &[u8], dimensions: u32) -> Result<Vec<f32>> {
    decode_embedding_vector(bytes, dimensions)
}

fn sanitize_reason(value: &str) -> String {
    let mut sanitized = value.trim().replace(['\n', '\r', '\t'], " ");
    if sanitized.chars().count() > 300 {
        sanitized = sanitized.chars().take(299).collect::<String>();
        sanitized.push('…');
    }
    sanitized
}

// Re-export the store candidate type so callers do not need two imports. The
// alias is intentionally a type alias: ANN candidates are stored chunks, not a
// second protocol shape.
pub type SourceAnnCandidate = SourceSemanticCandidate;
pub type SourceAnnSemanticCandidate = SourceSemanticCandidate;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    use crate::source_embedding::encode_source_vector;

    fn vector(seed: f32, dimensions: u32) -> Vec<f32> {
        (0..dimensions)
            .map(|index| seed + index as f32 * 0.001)
            .collect()
    }

    fn sample(count: usize, dimensions: u32) -> Vec<SourceIndexVector> {
        (0..count)
            .map(|index| SourceIndexVector {
                chunk_id: format!("chunk-{index:04}"),
                values: vector(0.1 + (index % 7) as f32 * 0.1, dimensions),
            })
            .collect()
    }

    fn row(generation: u64, centroids: Vec<u8>, centroid_count: u32) -> SourceAnnIndexRow {
        SourceAnnIndexRow {
            project_id: "prj".into(),
            model: "embed".into(),
            dimensions: 8,
            nlist: 4,
            centroid_count,
            vector_count: 64,
            generation,
            centroids,
            last_error: None,
            created_at: "2026-11-12T00:00:00Z".into(),
            updated_at: "2026-11-12T00:00:00Z".into(),
        }
    }

    fn counts(vector_count: u32, assigned: u32) -> SourceAnnCounts {
        SourceAnnCounts {
            vector_count,
            assigned_vectors: assigned,
            invalid_assignments: 0,
            total_assignments: assigned,
        }
    }

    #[test]
    fn centroid_blob_round_trips_and_rejects_structural_corruption() {
        let centroids = vec![vector(0.2, 8), vector(0.4, 8)];
        let bytes = encode_centroids(8, &centroids).unwrap();
        assert_eq!(validate_centroid_blob(&bytes, 8).unwrap(), 2);
        assert_eq!(decode_centroids(&bytes, 8).unwrap(), centroids);
        assert!(decode_centroids(&bytes, 16).is_err());

        let mut bad_magic = bytes.clone();
        bad_magic[0] = b'X';
        assert!(validate_centroid_blob(&bad_magic, 8).is_err());
        let mut bad_version = bytes.clone();
        bad_version[4] = 9;
        assert!(validate_centroid_blob(&bad_version, 8).is_err());
        let mut bad_length = bytes.clone();
        bad_length.pop();
        assert!(validate_centroid_blob(&bad_length, 8).is_err());
        let mut bad_count = bytes.clone();
        bad_count[9] = 0;
        bad_count[10] = 0;
        bad_count[11] = 0;
        bad_count[12] = 0;
        assert!(validate_centroid_blob(&bad_count, 8).is_err());
        let mut non_finite = bytes.clone();
        non_finite[CENTROID_HEADER_BYTES..CENTROID_HEADER_BYTES + 4]
            .copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(decode_centroids(&non_finite, 8).is_err());
        assert!(encode_centroids(8, &[]).is_err());
        assert!(encode_centroids(8, &[vec![0.0; 8]]).is_err());
        assert!(encode_centroids(8, &[vec![1.0; 7]]).is_err());
    }

    #[test]
    fn seed_is_stable_and_ignores_generation() {
        let ids = vec!["a".to_string(), "b".to_string()];
        let first = source_ann_seed("prj", "embed", 8, 2, &ids);
        let second = source_ann_seed("prj", "embed", 8, 2, &ids);
        assert_eq!(first, second);
        assert_ne!(first, source_ann_seed("prj2", "embed", 8, 2, &ids));
        assert_ne!(first, source_ann_seed("prj", "embed2", 8, 2, &ids));
        assert_ne!(first, source_ann_seed("prj", "embed", 16, 2, &ids));
        assert_ne!(first, source_ann_seed("prj", "embed", 8, 3, &ids));
        let mut reordered = ids.clone();
        reordered.reverse();
        assert_eq!(first, source_ann_seed("prj", "embed", 8, 2, &reordered));
    }

    #[test]
    fn kmeans_is_deterministic_and_order_independent() {
        let vectors = sample(80, 8);
        let mut shuffled = vectors.clone();
        shuffled.reverse();
        shuffled.swap(3, 17);
        let seed = 42;
        let first = build_kmeans(&vectors, 8, seed).unwrap();
        let second = build_kmeans(&shuffled, 8, seed).unwrap();
        assert_eq!(first.centroids, second.centroids);
        assert_eq!(first.assignments, second.assignments);
        assert_eq!(first.iterations, second.iterations);
        assert!(first.iterations <= MAX_SOURCE_ANN_KMEANS_ITERATIONS);
        assert!(first.centroids.len() <= 8);
        assert_eq!(first.assignments.len(), 80);
        assert!(first.assignments.iter().all(|(_, cluster)| *cluster < 8));
    }

    #[test]
    fn kmeans_handles_small_and_duplicate_samples() {
        let one = build_kmeans(&sample(1, 8), 8, 7).unwrap();
        assert_eq!(one.centroids.len(), 1);
        assert_eq!(one.assignments.len(), 1);
        let duplicates = vec![
            SourceIndexVector {
                chunk_id: "a".into(),
                values: vector(0.2, 8),
            },
            SourceIndexVector {
                chunk_id: "b".into(),
                values: vector(0.2, 8),
            },
        ];
        let build = build_kmeans(&duplicates, 4, 9).unwrap();
        assert_eq!(build.centroids.len(), 2);
        assert!(build_kmeans(&[], 4, 9).is_err());
        let mut bad = sample(2, 8);
        bad[1].chunk_id = bad[0].chunk_id.clone();
        assert!(build_kmeans(&bad, 1, 9).is_err());
        let mut nan = sample(2, 8);
        nan[0].values[0] = f32::NAN;
        assert!(build_kmeans(&nan, 1, 9).is_err());
    }

    #[test]
    fn centroid_ranking_is_stable_and_truncates_to_nprobe() {
        let centroids = vec![
            vector(0.2, 8),
            vector(0.21, 8),
            vector(0.9, 8),
            vector(0.91, 8),
        ];
        let query = vector(0.2, 8);
        let probed = rank_centroids(&centroids, &query, 2).unwrap();
        assert_eq!(probed.len(), 2);
        assert!(probed.contains(&0));
        assert!(probed.contains(&1));
        let all = rank_centroids(&centroids, &query, 99).unwrap();
        assert_eq!(all.len(), 4);
        assert!(rank_centroids(&[], &query, 1).is_err());
        assert!(rank_centroids(&centroids, &[], 1).is_err());
        assert!(rank_centroids(&centroids, &[0.0; 8], 1).is_err());
        assert!(rank_centroids(&centroids, &query, 0).unwrap().len() == 1);
    }

    #[test]
    fn assignment_ties_choose_the_lower_centroid() {
        let centroids = vec![vec![1.0f32, 0.0], vec![1.0f32, 0.0]];
        assert_eq!(assign_vector(&centroids, &[1.0, 0.0]).unwrap(), 0);
    }

    #[test]
    fn validation_reports_actionable_reasons_in_order() {
        let policy = SourceRetrievalPolicy {
            ann_enabled: false,
            ann_min_vectors: 16,
            ..SourceRetrievalPolicy::default()
        };
        let valid_blob = encode_centroids(8, &[vector(0.2, 8)]).unwrap();
        let valid_row = row(1, valid_blob.clone(), 1);
        let value = validate_index(&policy, true, true, Some(&valid_row), counts(64, 64), 64);
        assert_eq!(value.reason.as_deref(), Some("ann_disabled"));
        let mut enabled = policy.clone();
        enabled.ann_enabled = true;
        assert_eq!(
            validate_index(&enabled, false, true, Some(&valid_row), counts(64, 64), 64)
                .reason
                .as_deref(),
            Some("embedding_disabled")
        );
        assert_eq!(
            validate_index(&enabled, true, false, Some(&valid_row), counts(64, 64), 64)
                .reason
                .as_deref(),
            Some("not_configured")
        );
        assert_eq!(
            validate_index(&enabled, true, true, Some(&valid_row), counts(8, 8), 8)
                .reason
                .as_deref(),
            Some("below_min_vectors")
        );
        assert_eq!(
            validate_index(&enabled, true, true, None, counts(64, 64), 64)
                .reason
                .as_deref(),
            Some("not_built")
        );
        assert_eq!(
            validate_index(&enabled, true, true, Some(&valid_row), counts(64, 0), 64)
                .reason
                .as_deref(),
            Some("no_assignments")
        );
        let mut corrupt = valid_row.clone();
        corrupt.centroids[0] = b'X';
        assert!(
            validate_index(&enabled, true, true, Some(&corrupt), counts(64, 64), 64)
                .reason
                .as_deref()
                .is_some_and(|reason| reason.starts_with("index_corrupt:"))
        );
        let mut mismatched = valid_row.clone();
        mismatched.centroid_count = 9;
        assert!(
            validate_index(&enabled, true, true, Some(&mismatched), counts(64, 64), 64)
                .reason
                .as_deref()
                .is_some_and(|reason| reason.starts_with("index_corrupt:"))
        );
        let ready = validate_index(&enabled, true, true, Some(&valid_row), counts(64, 64), 64);
        assert!(ready.ready);
        assert!(ready.reason.is_none());
        assert_eq!(ready.generation, Some(1));
    }

    #[test]
    fn nlist_and_candidate_limits_are_bounded() {
        assert_eq!(source_nlist(0), 1);
        assert_eq!(source_nlist(1), 1);
        assert_eq!(source_nlist(256), 16);
        assert_eq!(source_nlist(u32::MAX), MAX_SOURCE_ANN_NLIST);
        assert_eq!(candidate_limit_per_cluster(512, 8), 64);
        assert_eq!(candidate_limit_per_cluster(1, 32), 32);
        assert_eq!(candidate_limit_per_cluster(1_000_000, 1), 256);
        assert_eq!(candidate_limit_per_cluster(0, 0), 32);
    }

    #[test]
    fn recall_fixture_matches_exact_scan_above_threshold() {
        // Eight well-separated synthetic clusters in 8 dimensions. With
        // nprobe=3 the probe should cover the true cluster and return the
        // nearest exact-scored candidates.
        let mut vectors = Vec::new();
        let mut queries = Vec::new();
        for cluster in 0..8 {
            let mut base = vec![0.0f32; 8];
            base[cluster % 8] = 1.0;
            base[(cluster + 1) % 8] = 0.1;
            queries.push(base.clone());
            for point in 0..20 {
                let mut values = base.clone();
                values[(cluster + 2) % 8] += point as f32 * 0.001;
                vectors.push(SourceIndexVector {
                    chunk_id: format!("c{cluster}-p{point:02}"),
                    values,
                });
            }
        }
        let build = build_kmeans(&vectors, 8, 1234).unwrap();
        let bytes = encode_centroids(8, &build.centroids).unwrap();
        let centroids = decode_centroids(&bytes, 8).unwrap();
        let mut total_exact = 0usize;
        let mut total_ann = 0usize;
        for query in &queries {
            let mut exact = build
                .assignments
                .iter()
                .map(|(chunk_id, _)| {
                    let values = &vectors
                        .iter()
                        .find(|vector| &vector.chunk_id == chunk_id)
                        .unwrap()
                        .values;
                    let score = prepared_cosine(
                        values,
                        values
                            .iter()
                            .map(|v| f64::from(*v).powi(2))
                            .sum::<f64>()
                            .sqrt(),
                        query,
                        query
                            .iter()
                            .map(|v| f64::from(*v).powi(2))
                            .sum::<f64>()
                            .sqrt(),
                    )
                    .unwrap_or(0.0);
                    (chunk_id.clone(), score)
                })
                .collect::<Vec<_>>();
            exact.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let probed = rank_centroids(&centroids, query, 3).unwrap();
            let mut ann = build
                .assignments
                .iter()
                .filter(|(_, cluster)| probed.contains(cluster))
                .map(|(chunk_id, _)| {
                    let values = &vectors
                        .iter()
                        .find(|vector| &vector.chunk_id == chunk_id)
                        .unwrap()
                        .values;
                    let score = prepared_cosine(
                        values,
                        values
                            .iter()
                            .map(|v| f64::from(*v).powi(2))
                            .sum::<f64>()
                            .sqrt(),
                        query,
                        query
                            .iter()
                            .map(|v| f64::from(*v).powi(2))
                            .sum::<f64>()
                            .sqrt(),
                    )
                    .unwrap_or(0.0);
                    (chunk_id.clone(), score)
                })
                .collect::<Vec<_>>();
            ann.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            ann.truncate(10);
            let exact_top = exact
                .iter()
                .take(10)
                .map(|(id, _)| id)
                .collect::<HashSet<_>>();
            total_exact += exact_top.len();
            total_ann += ann.iter().filter(|(id, _)| exact_top.contains(id)).count();
        }
        let recall = total_ann as f64 / total_exact as f64;
        assert!(
            recall >= 0.75,
            "deterministic IVF recall should be high for separated fixtures, got {recall}"
        );
    }

    #[test]
    fn should_rebuild_respects_metadata_and_growth_thresholds() {
        let valid_blob = encode_centroids(8, &[vector(0.2, 8)]).unwrap();
        let row = row(1, valid_blob, 1);
        assert!(!should_rebuild(Some(&row), true, "embed", 8, 64));
        assert!(should_rebuild(Some(&row), false, "embed", 8, 64));
        assert!(should_rebuild(None, true, "embed", 8, 64));
        assert!(should_rebuild(Some(&row), true, "other", 8, 64));
        assert!(should_rebuild(Some(&row), true, "embed", 16, 64));
        assert!(!should_rebuild(Some(&row), true, "embed", 8, 100));
        // Stored 64: growth threshold is max(32, 64) = 64 -> 128.
        assert!(should_rebuild(Some(&row), true, "embed", 8, 128));
    }

    #[test]
    fn encoded_vectors_are_accepted_only_with_matching_headers() {
        let bytes = encode_source_vector(&vector(0.2, 8)).unwrap();
        assert_eq!(decode_source_vector(&bytes, 8).unwrap().len(), 8);
        assert!(decode_source_vector(&bytes, 16).is_err());
        assert!(decode_source_vector(&[0u8; 9], 8).is_err());
    }
}
