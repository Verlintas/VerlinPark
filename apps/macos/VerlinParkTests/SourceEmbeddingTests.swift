import Foundation
@testable import VerlinPark
import XCTest

final class SourceEmbeddingTests: XCTestCase {
    func testLegacySourceSearchHitDefaultsToLexicalLabels() throws {
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
                  "score": 1.25
                }
                """.utf8
            )
        )
        XCTAssertEqual(hit.source, "lexical")
        XCTAssertEqual(hit.confidence, 1.25)
        XCTAssertEqual(hit.origins, ["lexical"])
        XCTAssertNil(hit.lexicalScore)
        XCTAssertNil(hit.semanticScore)
    }

    func testLegacySourceSearchResultDefaultsToLexicalMode() throws {
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
                    "score": 1.25
                  }],
                  "degraded": false
                }
                """.utf8
            )
        )
        XCTAssertEqual(result.mode, "lexical")
        XCTAssertEqual(result.requestedMode, "lexical")
        XCTAssertNil(result.semanticUnavailable)
        XCTAssertEqual(result.candidatesScanned, 0)
        XCTAssertEqual(result.hits.count, 1)
    }

    func testHybridResultDecodesOriginsAndFallback() throws {
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
                    "score": 0.87,
                    "source": "hybrid",
                    "confidence": 0.87,
                    "origins": ["lexical", "semantic"],
                    "lexical_score": 1.4,
                    "semantic_score": 0.81
                  }],
                  "degraded": false,
                  "mode": "hybrid",
                  "requested_mode": "hybrid",
                  "semantic_unavailable": "stale_vectors",
                  "candidates_scanned": 12
                }
                """.utf8
            )
        )
        XCTAssertEqual(result.mode, "hybrid")
        XCTAssertEqual(result.requestedMode, "hybrid")
        XCTAssertEqual(result.semanticUnavailable, "stale_vectors")
        XCTAssertEqual(result.candidatesScanned, 12)
        XCTAssertEqual(result.hits[0].source, "hybrid")
        XCTAssertEqual(result.hits[0].origins, ["lexical", "semantic"])
        XCTAssertEqual(result.hits[0].lexicalScore ?? 0, 1.4, accuracy: 0.0001)
        XCTAssertEqual(result.hits[0].semanticScore ?? 0, 0.81, accuracy: 0.0001)
    }

    func testSourceEmbeddingPolicyDecodesDefaultsAndJSONNames() throws {
        let policy = try JSONDecoder().decode(
            SourceEmbeddingPolicy.self,
            from: Data(
                """
                {
                  "enabled": true,
                  "endpoint_env": "SOURCE_EMBED_ENDPOINT",
                  "model": "embed-small"
                }
                """.utf8
            )
        )
        XCTAssertTrue(policy.enabled)
        XCTAssertTrue(policy.isConfigured)
        XCTAssertEqual(policy.dimensions, 1536)
        XCTAssertEqual(policy.batchSize, 8)
        XCTAssertEqual(policy.maxChunksPerSource, 200)
        XCTAssertEqual(policy.retentionDays, 30)
        XCTAssertEqual(policy.timeoutSecs, 30)
        XCTAssertNil(policy.apiKeyEnv)
        let object = policy.jsonObject()
        XCTAssertEqual(object["endpoint_env"]?.stringValue, "SOURCE_EMBED_ENDPOINT")
        XCTAssertEqual(object["max_chunks_per_source"]?.intValue, 200)
        XCTAssertEqual(object["retention_days"]?.intValue, 30)
        XCTAssertEqual(object["batch_size"]?.intValue, 8)
        XCTAssertFalse(object.values.contains { $0.stringValue?.hasPrefix("sk-") == true })
    }

    func testSourceEmbeddingStatusDecodesCountsJobsAndReason() throws {
        let status = try JSONDecoder().decode(
            SourceEmbeddingStatusResult.self,
            from: Data(
                """
                {
                  "project_id": "prj-1",
                  "enabled": true,
                  "configured": true,
                  "endpoint_env": "SOURCE_EMBED_ENDPOINT",
                  "endpoint_env_present": false,
                  "api_key_env": null,
                  "api_key_env_present": true,
                  "model": "embed-small",
                  "dimensions": 512,
                  "batch_size": 4,
                  "max_chunks_per_source": 200,
                  "retention_days": 30,
                  "indexable_chunks": 20,
                  "indexed_chunks": 16,
                  "stale_chunks": 2,
                  "invalid_chunks": 0,
                  "pending_jobs": 1,
                  "running_jobs": 1,
                  "failed_jobs": 1,
                  "dead_jobs": 0,
                  "done_jobs": 3,
                  "coverage": 0.8,
                  "semantic_unavailable": "endpoint_env_missing:SOURCE_EMBED_ENDPOINT",
                  "last_error": "http_503",
                  "usage": {
                    "calls": 4,
                    "input_tokens": 120,
                    "estimated_tokens": 20,
                    "bytes": 480,
                    "provider_reported_calls": 3
                  },
                  "recent_jobs": [{
                    "source_id": "src-1",
                    "source_title": "Runtime",
                    "status": "failed",
                    "attempts": 2,
                    "generation": 4,
                    "chunks_total": 10,
                    "chunks_done": 8,
                    "last_error": "http_503",
                    "next_attempt_at": "2026-11-11T00:10:00Z",
                    "updated_at": "2026-11-11T00:00:00Z"
                  }],
                  "integrity": {
                    "valid_vectors": 16,
                    "invalid_vectors": 0,
                    "stale_vectors": 2,
                    "orphan_vectors": 0,
                    "generation_mismatched": 2
                  }
                }
                """.utf8
            )
        )
        XCTAssertEqual(status.projectId, "prj-1")
        XCTAssertTrue(status.enabled)
        XCTAssertFalse(status.endpointEnvPresent)
        XCTAssertEqual(status.indexedChunks, 16)
        XCTAssertEqual(status.indexableChunks, 20)
        XCTAssertEqual(status.activeJobs, 2)
        XCTAssertEqual(status.attentionJobs, 1)
        XCTAssertEqual(status.coverage, 0.8, accuracy: 0.0001)
        XCTAssertEqual(
            status.semanticUnavailable,
            "endpoint_env_missing:SOURCE_EMBED_ENDPOINT"
        )
        XCTAssertEqual(status.recentJobs[0].generation, 4)
        XCTAssertEqual(status.recentJobs[0].chunksDone, 8)
        XCTAssertEqual(status.integrity.generationMismatched, 2)
        XCTAssertEqual(status.usage.providerReportedCalls, 3)
    }

    func testSourceEmbeddingReindexResultDecodesLegacyDefaults() throws {
        let result = try JSONDecoder().decode(
            SourceEmbeddingReindexResult.self,
            from: Data(#"{"project_id":"prj-1"}"#.utf8)
        )
        XCTAssertFalse(result.enabled)
        XCTAssertEqual(result.clearedVectors, 0)
        XCTAssertEqual(result.enqueuedSources, 0)
    }

    func testSourceSearchModeRawValuesAreProtocolStable() {
        XCTAssertEqual(SourceSearchMode.lexical.rawValue, "lexical")
        XCTAssertEqual(SourceSearchMode.semantic.rawValue, "semantic")
        XCTAssertEqual(SourceSearchMode.hybrid.rawValue, "hybrid")
        XCTAssertNil(SourceSearchMode.automatic.protocolValue)
        XCTAssertEqual(
            SourceSearchMode.hybrid.protocolValue?.rawValue,
            "hybrid"
        )
        XCTAssertEqual(SourceSearchMode.allCases.count, 4)
    }

    func testSourceSearchResultEncodesNewFieldsAndExtensionDefaults() throws {
        let hit = SourceSearchHit(
            sourceID: "src-1",
            sourceTitle: "Runtime",
            chunkID: "src-1:0001:abcdef",
            chunkNo: 1,
            content: "Deterministic leases.",
            score: 0.75,
            source: "semantic",
            confidence: 0.75,
            origins: ["semantic"],
            semanticScore: 0.75
        )
        let result = SourceSearchResult(
            hits: [hit],
            mode: "semantic",
            requestedMode: "semantic",
            candidatesScanned: 9
        )
        let data = try JSONEncoder().encode(result)
        let decoded = try JSONDecoder().decode(SourceSearchResult.self, from: data)
        XCTAssertEqual(decoded.mode, "semantic")
        XCTAssertEqual(decoded.candidatesScanned, 9)
        XCTAssertEqual(decoded.hits[0].source, "semantic")
        XCTAssertEqual(decoded.hits[0].origins, ["semantic"])
        XCTAssertNil(decoded.hits[0].lexicalScore)
        XCTAssertEqual(decoded.hits[0].semanticScore ?? 0, 0.75, accuracy: 0.0001)
    }

    func testSourceEmbeddingUsageDecodesMissingAdditiveFields() throws {
        let usage = try JSONDecoder().decode(
            SourceEmbeddingUsage.self,
            from: Data("{}".utf8)
        )
        XCTAssertEqual(usage.calls, 0)
        XCTAssertEqual(usage.inputTokens, 0)
        XCTAssertEqual(usage.estimatedTokens, 0)
        XCTAssertEqual(usage.bytes, 0)
        XCTAssertEqual(usage.providerReportedCalls, 0)
    }
}
