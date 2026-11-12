import Foundation
@testable import VerlinPark
import XCTest

final class SourceRetrievalTests: XCTestCase {
    func testRetrievalPolicyDecodesDefaultsAndBoundsMetadata() throws {
        let policy = try JSONDecoder().decode(
            SourceRetrievalPolicy.self,
            from: Data("{}".utf8)
        )
        XCTAssertFalse(policy.annEnabled)
        XCTAssertEqual(policy.annMinVectors, 256)
        XCTAssertEqual(policy.annNProbe, 8)
        XCTAssertEqual(policy.rerank, .deterministic)
        XCTAssertEqual(policy.queryExpansion, .off)
        XCTAssertEqual(policy.maxExpandedTerms, 8)

        let configured = try JSONDecoder().decode(
            SourceRetrievalPolicy.self,
            from: Data(
                """
                {
                  "ann_enabled": true,
                  "ann_min_vectors": 16,
                  "ann_nprobe": 3,
                  "rerank": "off",
                  "query_expansion": "lexical",
                  "max_expanded_terms": 4
                }
                """.utf8
            )
        )
        XCTAssertTrue(configured.annEnabled)
        XCTAssertEqual(configured.annMinVectors, 16)
        XCTAssertEqual(configured.annNProbe, 3)
        XCTAssertEqual(configured.rerank, .off)
        XCTAssertEqual(configured.queryExpansion, .lexical)
        XCTAssertEqual(configured.maxExpandedTerms, 4)
        let object = configured.jsonObject()
        XCTAssertEqual(object["ann_enabled"]?.boolValue, true)
        XCTAssertEqual(object["ann_nprobe"]?.intValue, 3)
        XCTAssertEqual(object["rerank"]?.stringValue, "off")
        XCTAssertEqual(object["query_expansion"]?.stringValue, "lexical")
    }

    func testRetrievalModesUseProtocolRawValues() {
        XCTAssertEqual(SourceRerankMode.off.rawValue, "off")
        XCTAssertEqual(SourceRerankMode.deterministic.rawValue, "deterministic")
        XCTAssertEqual(SourceQueryExpansionMode.off.rawValue, "off")
        XCTAssertEqual(SourceQueryExpansionMode.lexical.rawValue, "lexical")
        XCTAssertEqual(SourceRerankMode.allCases.count, 2)
        XCTAssertEqual(SourceQueryExpansionMode.allCases.count, 2)
    }

    func testIndexStatusDecodesCountsAndStableFallback() throws {
        let status = try JSONDecoder().decode(
            SourceIndexStatusResult.self,
            from: Data(
                """
                {
                  "project_id": "prj-1",
                  "policy": {
                    "ann_enabled": true,
                    "ann_min_vectors": 16,
                    "ann_nprobe": 2,
                    "rerank": "deterministic",
                    "query_expansion": "off",
                    "max_expanded_terms": 8
                  },
                  "embedding_enabled": true,
                  "embedding_configured": true,
                  "model": "embed-small",
                  "dimensions": 8,
                  "ann_enabled": true,
                  "ready": false,
                  "path": "exact",
                  "generation": 4,
                  "vector_count": 40,
                  "assigned_vectors": 32,
                  "invalid_assignments": 1,
                  "centroid_count": 6,
                  "nlist": 6,
                  "nprobe": 2,
                  "min_vectors": 16,
                  "coverage": 0.8,
                  "bytes": 2048,
                  "fallback_reason": "index_corrupt: bad header",
                  "last_rebuilt_at": "2026-11-12T00:00:00Z",
                  "last_error": null
                }
                """.utf8
            )
        )
        XCTAssertEqual(status.projectId, "prj-1")
        XCTAssertTrue(status.annEnabled)
        XCTAssertFalse(status.ready)
        XCTAssertEqual(status.path, "exact")
        XCTAssertEqual(status.generation, 4)
        XCTAssertEqual(status.vectorCount, 40)
        XCTAssertEqual(status.assignedVectors, 32)
        XCTAssertEqual(status.coverage, 0.8, accuracy: 0.0001)
        XCTAssertEqual(status.fallbackReason, "index_corrupt: bad header")
        XCTAssertEqual(status.nprobe, 2)
        XCTAssertFalse(status.activePathLabel.isEmpty)
    }

    func testLegacyIndexStatusDecodesSafeDefaults() throws {
        let status = try JSONDecoder().decode(
            SourceIndexStatusResult.self,
            from: Data(#"{"project_id":"prj-1"}"#.utf8)
        )
        XCTAssertFalse(status.ready)
        XCTAssertEqual(status.path, "exact")
        XCTAssertFalse(status.annEnabled)
        XCTAssertEqual(status.vectorCount, 0)
        XCTAssertEqual(status.policy, .defaults)
        XCTAssertNil(status.fallbackReason)
    }

    func testSearchHitDecodesRerankAndExpansionComponents() throws {
        let hit = try JSONDecoder().decode(
            SourceSearchHit.self,
            from: Data(
                """
                {
                  "source_id": "src-1",
                  "source_title": "Runtime",
                  "chunk_id": "src-1:0001:abcdef",
                  "chunk_no": 1,
                  "content": "Deterministic leases.",
                  "score": 0.72,
                  "source": "lexical",
                  "confidence": 0.72,
                  "origins": ["lexical"],
                  "lexical_score": 1.2,
                  "semantic_score": 0.81,
                  "rerank": "deterministic",
                  "rerank_score": 0.72,
                  "lexical_overlap": 1.0,
                  "title_match": 0.5,
                  "freshness": 0.9,
                  "expanded": true,
                  "expansion_terms": ["sandbox"],
                  "source_updated_at": "2026-11-12T00:00:00Z"
                }
                """.utf8
            )
        )
        XCTAssertEqual(hit.rerank, "deterministic")
        XCTAssertEqual(hit.rerankScore ?? 0, 0.72, accuracy: 0.0001)
        XCTAssertEqual(hit.lexicalOverlap ?? 0, 1.0, accuracy: 0.0001)
        XCTAssertEqual(hit.titleMatch ?? 0, 0.5, accuracy: 0.0001)
        XCTAssertEqual(hit.freshness ?? 0, 0.9, accuracy: 0.0001)
        XCTAssertTrue(hit.expanded)
        XCTAssertEqual(hit.expansionTerms, ["sandbox"])
        XCTAssertEqual(hit.sourceUpdatedAt, "2026-11-12T00:00:00Z")
    }

    func testLegacySearchResultDecodesRetrievalV2Defaults() throws {
        let result = try JSONDecoder().decode(
            SourceSearchResult.self,
            from: Data(
                """
                {
                  "hits": [{
                    "source_id": "src-1",
                    "source_title": "Runtime",
                    "chunk_id": "src-1:0001:abcdef",
                    "chunk_no": 1,
                    "content": "Deterministic leases.",
                    "score": 1.0
                  }],
                  "degraded": false
                }
                """.utf8
            )
        )
        XCTAssertEqual(result.indexPath, "exact")
        XCTAssertEqual(result.rerank, "off")
        XCTAssertEqual(result.expansion, "off")
        XCTAssertNil(result.indexGeneration)
        XCTAssertNil(result.indexFallbackReason)
        XCTAssertEqual(result.probedClusters, 0)
        XCTAssertEqual(result.annCandidates, 0)
        XCTAssertTrue(result.expansionTerms.isEmpty)
        XCTAssertEqual(result.hits[0].rerank, "off")
        XCTAssertFalse(result.hits[0].expanded)
    }

    func testSearchResultEncodesIndexPathAndExpansion() throws {
        let hit = SourceSearchHit(
            sourceID: "src-1",
            sourceTitle: "Runtime",
            chunkID: "src-1:0001:abcdef",
            chunkNo: 1,
            content: "Deterministic leases.",
            score: 0.66,
            source: "hybrid",
            confidence: 0.66,
            origins: ["lexical", "semantic"],
            semanticScore: 0.8,
            rerank: "deterministic",
            rerankScore: 0.66,
            expanded: true,
            expansionTerms: ["sandbox"]
        )
        let result = SourceSearchResult(
            hits: [hit],
            mode: "hybrid",
            requestedMode: "hybrid",
            indexPath: "ann",
            indexGeneration: 7,
            probedClusters: 3,
            annCandidates: 24,
            rerank: "deterministic",
            expansion: "lexical",
            expansionTerms: ["sandbox"]
        )
        let encoded = try JSONEncoder().encode(result)
        let decoded = try JSONDecoder().decode(SourceSearchResult.self, from: encoded)
        XCTAssertEqual(decoded.indexPath, "ann")
        XCTAssertEqual(decoded.indexGeneration, 7)
        XCTAssertEqual(decoded.probedClusters, 3)
        XCTAssertEqual(decoded.annCandidates, 24)
        XCTAssertEqual(decoded.rerank, "deterministic")
        XCTAssertEqual(decoded.expansion, "lexical")
        XCTAssertEqual(decoded.expansionTerms, ["sandbox"])
        XCTAssertEqual(decoded.hits[0].rerankScore ?? 0, 0.66, accuracy: 0.0001)
        XCTAssertEqual(decoded.hits[0].expansionTerms, ["sandbox"])
    }

    func testRetrievalPolicyRoundTripsAndPreservesExplicitFalse() throws {
        let policy = SourceRetrievalPolicy(
            annEnabled: false,
            annMinVectors: 12,
            annNProbe: 2,
            rerank: .off,
            queryExpansion: .lexical,
            maxExpandedTerms: 3
        )
        let data = try JSONEncoder().encode(policy)
        let decoded = try JSONDecoder().decode(SourceRetrievalPolicy.self, from: data)
        XCTAssertEqual(decoded, policy)
        XCTAssertFalse(decoded.annEnabled)
        XCTAssertEqual(decoded.queryExpansion, .lexical)
        XCTAssertEqual(decoded.rerank, .off)
    }

    func testSearchHitLegacyDefaultsDoNotInventRerankEvidence() throws {
        let hit = try JSONDecoder().decode(
            SourceSearchHit.self,
            from: Data(
                """
                {
                  "source_id": "src-1",
                  "source_title": "Runtime",
                  "chunk_id": "src-1:0001:abcdef",
                  "chunk_no": 1,
                  "content": "Deterministic leases.",
                  "score": 0.5
                }
                """.utf8
            )
        )
        XCTAssertEqual(hit.rerank, "off")
        XCTAssertNil(hit.rerankScore)
        XCTAssertNil(hit.lexicalOverlap)
        XCTAssertNil(hit.titleMatch)
        XCTAssertNil(hit.freshness)
        XCTAssertFalse(hit.expanded)
        XCTAssertTrue(hit.expansionTerms.isEmpty)
        XCTAssertNil(hit.sourceUpdatedAt)
    }

    func testRebuildResultDecodesReasonAndGeneration() throws {
        let result = try JSONDecoder().decode(
            SourceIndexRebuildResult.self,
            from: Data(
                """
                {
                  "project_id": "prj-1",
                  "rebuilt": false,
                  "reason": "below_min_vectors",
                  "vector_count": 4,
                  "assigned_vectors": 0,
                  "centroid_count": 0,
                  "nlist": 2,
                  "nprobe": 8,
                  "path": "exact"
                }
                """.utf8
            )
        )
        XCTAssertFalse(result.rebuilt)
        XCTAssertEqual(result.reason, "below_min_vectors")
        XCTAssertEqual(result.vectorCount, 4)
        XCTAssertEqual(result.nprobe, 8)
        XCTAssertNil(result.generation)
        let legacy = try JSONDecoder().decode(
            SourceIndexRebuildResult.self,
            from: Data(#"{"project_id":"prj-1"}"#.utf8)
        )
        XCTAssertFalse(legacy.rebuilt)
        XCTAssertEqual(legacy.path, "exact")
    }
}
