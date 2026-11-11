import Foundation
@testable import VerlinPark
import XCTest

final class MemoryEmbeddingTests: XCTestCase {
    func testLegacyMemoryPolicyDecodesWithoutEmbedding() throws {
        let policy = try JSONDecoder().decode(
            MemoryPolicy.self,
            from: Data(
                """
                {
                  "enabled": true,
                  "max_items": 100,
                  "max_age_days": 180,
                  "task_result_verify_days": 90,
                  "default_verify_days": 365,
                  "auto_supersede": true,
                  "brief_max_chars": 4000,
                  "sweep_stale": false
                }
                """.utf8
            )
        )
        XCTAssertNil(policy.embedding)
        XCTAssertTrue(policy.enabled)
    }

    func testEmbeddingPolicyDecodesDefaultsAndPreservesConfiguration() throws {
        let policy = try JSONDecoder().decode(
            MemoryEmbeddingPolicy.self,
            from: Data(
                """
                {
                  "enabled": true,
                  "endpoint": "http://127.0.0.1:8080/v1",
                  "model": "text-embedding-test"
                }
                """.utf8
            )
        )
        XCTAssertTrue(policy.enabled)
        XCTAssertEqual(policy.dimensions, 1536)
        XCTAssertEqual(policy.batchSize, 16)
        XCTAssertEqual(policy.timeoutSecs, 30)
        XCTAssertEqual(policy.maxCandidates, 64)
        XCTAssertEqual(policy.minSimilarity, 0.20, accuracy: 0.0001)
        XCTAssertNil(policy.apiKeyEnv)
        XCTAssertTrue(policy.isConfigured)
    }

    func testMemoryPolicyEmbeddingRoundTripAndJSONBody() throws {
        let embedding = MemoryEmbeddingPolicy(
            enabled: true,
            endpoint: "https://api.example.test/v1",
            model: "embed-small",
            dimensions: 512,
            batchSize: 8,
            timeoutSecs: 12,
            maxCandidates: 32,
            minSimilarity: 0.35,
            apiKeyEnv: "EMBED_API_KEY"
        )
        let policy = MemoryPolicy(
            enabled: false,
            maxItems: 50,
            maxAgeDays: nil,
            taskResultVerifyDays: 30,
            defaultVerifyDays: 90,
            autoSupersede: true,
            briefMaxChars: 3000,
            sweepStale: true,
            embedding: embedding
        )
        let data = try JSONEncoder().encode(policy)
        let decoded = try JSONDecoder().decode(MemoryPolicy.self, from: data)
        XCTAssertEqual(decoded.embedding, embedding)

        let body = embedding.jsonObject()
        XCTAssertEqual(body["enabled"]?.boolValue, true)
        XCTAssertEqual(body["endpoint"]?.stringValue, "https://api.example.test/v1")
        XCTAssertEqual(body["model"]?.stringValue, "embed-small")
        XCTAssertEqual(body["dimensions"]?.intValue, 512)
        XCTAssertEqual(body["batch_size"]?.intValue, 8)
        XCTAssertEqual(body["timeout_secs"]?.intValue, 12)
        XCTAssertEqual(body["max_candidates"]?.intValue, 32)
        XCTAssertEqual(body["min_similarity"]?.doubleValue ?? 0, 0.35, accuracy: 0.0001)
        XCTAssertEqual(body["api_key_env"]?.stringValue, "EMBED_API_KEY")
        // The environment variable *name* is the only key-related value sent.
        XCTAssertFalse(body.values.contains { $0.stringValue?.hasPrefix("sk-") == true })
    }

    func testEmptyAPIKeyEnvIsEncodedAsNull() {
        var embedding = MemoryEmbeddingPolicy.defaults
        embedding.apiKeyEnv = "   "
        XCTAssertEqual(embedding.jsonObject()["api_key_env"], .null)
    }

    func testEmbeddingStatusDecodesCoverageUsageAndJobs() throws {
        let status = try JSONDecoder().decode(
            MemoryEmbeddingsStatusResult.self,
            from: Data(
                """
                {
                  "project_id": "prj-1",
                  "enabled": true,
                  "configured": true,
                  "model": "embed-small",
                  "dimensions": 512,
                  "indexable": 8,
                  "indexed": 6,
                  "invalid": 1,
                  "pending": 1,
                  "failed": 1,
                  "skipped": 2,
                  "coverage": 0.75,
                  "last_error": "HTTP 503: busy",
                  "usage": {
                    "calls": 4,
                    "input_tokens": 120,
                    "estimated_tokens": 20,
                    "bytes": 480,
                    "provider_reported_calls": 3
                  },
                  "recent_jobs": [
                    {
                      "memory_id": "mem-1",
                      "title": "A decision",
                      "status": "failed",
                      "attempts": 2,
                      "last_error": "HTTP 503: busy",
                      "next_attempt_at": "2026-11-11T00:10:00Z",
                      "updated_at": "2026-11-11T00:00:00Z"
                    }
                  ]
                }
                """.utf8
            )
        )
        XCTAssertEqual(status.projectId, "prj-1")
        XCTAssertTrue(status.enabled)
        XCTAssertTrue(status.configured)
        XCTAssertEqual(status.model, "embed-small")
        XCTAssertEqual(status.dimensions, 512)
        XCTAssertEqual(status.indexable, 8)
        XCTAssertEqual(status.indexed, 6)
        XCTAssertEqual(status.invalid, 1)
        XCTAssertEqual(status.pending, 1)
        XCTAssertEqual(status.failed, 1)
        XCTAssertEqual(status.coverage, 0.75, accuracy: 0.0001)
        XCTAssertEqual(status.coveragePercent, 75, accuracy: 0.0001)
        XCTAssertEqual(status.coverageLabel, "75.0%")
        XCTAssertEqual(status.healthLabel, "degraded")
        XCTAssertEqual(status.lastError, "HTTP 503: busy")
        XCTAssertEqual(status.usage.calls, 4)
        XCTAssertEqual(status.usage.providerReportedCalls, 3)
        XCTAssertEqual(status.recentJobs.count, 1)
        XCTAssertEqual(status.recentJobs[0].title, "A decision")
        XCTAssertEqual(status.recentJobs[0].attempts, 2)
    }

    func testEmbeddingStatusDefaultsAndHealthLabels() throws {
        let status = try JSONDecoder().decode(
            MemoryEmbeddingsStatusResult.self,
            from: Data(
                """
                {
                  "project_id": "prj-empty",
                  "indexable": 0
                }
                """.utf8
            )
        )
        XCTAssertFalse(status.enabled)
        XCTAssertFalse(status.configured)
        XCTAssertEqual(status.dimensions, 1536)
        XCTAssertEqual(status.indexed, 0)
        XCTAssertEqual(status.coverage, 0)
        XCTAssertEqual(status.coverageLabel, "no memories")
        XCTAssertEqual(status.healthLabel, "not configured")
        XCTAssertEqual(status.usage.calls, 0)
        XCTAssertTrue(status.recentJobs.isEmpty)

        var enabled = status
        enabled = MemoryEmbeddingsStatusResult(
            projectId: "p",
            enabled: true,
            configured: true,
            model: "m",
            dimensions: 8,
            indexable: 2,
            indexed: 2,
            pending: 0,
            failed: 0,
            skipped: 0,
            coverage: 1,
            lastError: nil,
            usage: MemoryEmbeddingUsage(),
            recentJobs: []
        )
        XCTAssertEqual(enabled.healthLabel, "healthy")

        let indexing = MemoryEmbeddingsStatusResult(
            projectId: "p",
            enabled: true,
            configured: true,
            model: "m",
            dimensions: 8,
            indexable: 2,
            indexed: 1,
            pending: 1,
            failed: 0,
            skipped: 0,
            coverage: 0.5,
            lastError: nil,
            usage: MemoryEmbeddingUsage(),
            recentJobs: []
        )
        XCTAssertEqual(indexing.healthLabel, "indexing")

        let disabled = MemoryEmbeddingsStatusResult(
            projectId: "p",
            enabled: false,
            configured: true,
            model: "m",
            dimensions: 8,
            indexable: 2,
            indexed: 2,
            pending: 0,
            failed: 0,
            skipped: 0,
            coverage: 1,
            lastError: nil,
            usage: MemoryEmbeddingUsage(),
            recentJobs: []
        )
        XCTAssertEqual(disabled.healthLabel, "disabled")
    }

    func testRebuildAndProbeResultsDecode() throws {
        let rebuild = try JSONDecoder().decode(
            MemoryEmbeddingsRebuildResult.self,
            from: Data(
                """
                {
                  "project_id": "prj-1",
                  "enabled": true,
                  "model": "embed-small",
                  "dimensions": 512,
                  "cleared": 12,
                  "enqueued": 10
                }
                """.utf8
            )
        )
        XCTAssertEqual(rebuild.cleared, 12)
        XCTAssertEqual(rebuild.enqueued, 10)

        let probe = try JSONDecoder().decode(
            MemoryEmbeddingsTestResult.self,
            from: Data(
                """
                {
                  "project_id": "prj-1",
                  "ok": false,
                  "model": "embed-small",
                  "dimensions": 512,
                  "latency_ms": 42,
                  "input_tokens": 3,
                  "estimated_tokens": 3,
                  "provider_reported": false,
                  "error": "embedding request timed out"
                }
                """.utf8
            )
        )
        XCTAssertFalse(probe.ok)
        XCTAssertEqual(probe.latencyMs, 42)
        XCTAssertFalse(probe.providerReported)
        XCTAssertEqual(probe.error, "embedding request timed out")
    }

    func testCoverageLabelClampsAndJobStatusDecodesOptionalFields() throws {
        let status = MemoryEmbeddingsStatusResult(
            projectId: "p",
            enabled: true,
            configured: true,
            model: "m",
            dimensions: 8,
            indexable: 2,
            indexed: 3,
            invalid: 0,
            pending: 0,
            failed: 0,
            skipped: 0,
            coverage: 1.5,
            lastError: nil,
            usage: MemoryEmbeddingUsage(),
            recentJobs: []
        )
        XCTAssertEqual(status.coveragePercent, 100, accuracy: 0.0001)
        XCTAssertEqual(status.coverageLabel, "100.0%")

        let negative = MemoryEmbeddingsStatusResult(
            projectId: "p",
            enabled: false,
            configured: false,
            model: nil,
            dimensions: 8,
            indexable: 0,
            indexed: 0,
            invalid: 0,
            pending: 0,
            failed: 0,
            skipped: 0,
            coverage: -0.4,
            lastError: nil,
            usage: MemoryEmbeddingUsage(),
            recentJobs: []
        )
        XCTAssertEqual(negative.coveragePercent, 0, accuracy: 0.0001)

        let job = try JSONDecoder().decode(
            MemoryEmbeddingJobStatus.self,
            from: Data(
                """
                {
                  "memory_id": "mem-1",
                  "title": "Title",
                  "status": "pending",
                  "attempts": 0,
                  "updated_at": "2026-11-11T00:00:00Z"
                }
                """.utf8
            )
        )
        XCTAssertNil(job.lastError)
        XCTAssertNil(job.nextAttemptAt)
        XCTAssertEqual(job.id, "mem-1")
    }

    func testEmbeddingPolicyBlankConfigurationIsNotConfigured() {
        var policy = MemoryEmbeddingPolicy.defaults
        policy.endpoint = "   "
        policy.model = "embed"
        XCTAssertFalse(policy.isConfigured)
        let body = policy.jsonObject()
        XCTAssertEqual(body["endpoint"]?.stringValue, "   ")
        XCTAssertEqual(body["enabled"]?.boolValue, false)
        XCTAssertEqual(body["dimensions"]?.intValue, 1536)
    }

    func testProtocolDefaultsForMissingInvalidAndUsageFields() throws {
        let status = try JSONDecoder().decode(
            MemoryEmbeddingsStatusResult.self,
            from: Data(
                """
                {
                  "project_id": "p",
                  "indexable": 1,
                  "indexed": 0
                }
                """.utf8
            )
        )
        XCTAssertEqual(status.invalid, 0)
        XCTAssertEqual(status.usage.calls, 0)
        XCTAssertEqual(status.coverageLabel, "0.0%")
        XCTAssertEqual(status.healthLabel, "not configured")
    }

    func testFullMemoryPolicyEmbeddingDecodeAndLegacyUsageDefaults() throws {
        let policy = try JSONDecoder().decode(
            MemoryPolicy.self,
            from: Data(
                """
                {
                  "enabled": false,
                  "embedding": {
                    "enabled": true,
                    "endpoint": "https://example.test/v1",
                    "model": "embed",
                    "dimensions": 512,
                    "batch_size": 8,
                    "timeout_secs": 12,
                    "max_candidates": 32,
                    "min_similarity": 0.35,
                    "api_key_env": "EMBED_KEY"
                  }
                }
                """.utf8
            )
        )
        let embedding = try XCTUnwrap(policy.embedding)
        XCTAssertTrue(embedding.enabled)
        XCTAssertEqual(embedding.endpoint, "https://example.test/v1")
        XCTAssertEqual(embedding.dimensions, 512)
        XCTAssertEqual(embedding.batchSize, 8)
        XCTAssertEqual(embedding.timeoutSecs, 12)
        XCTAssertEqual(embedding.maxCandidates, 32)
        XCTAssertEqual(embedding.minSimilarity, 0.35, accuracy: 0.0001)
        XCTAssertEqual(embedding.apiKeyEnv, "EMBED_KEY")

        let usage = try JSONDecoder().decode(
            MemoryEmbeddingUsage.self,
            from: Data("{}".utf8)
        )
        XCTAssertEqual(usage.calls, 0)
        XCTAssertEqual(usage.inputTokens, 0)
        XCTAssertEqual(usage.estimatedTokens, 0)
        XCTAssertEqual(usage.bytes, 0)
        XCTAssertEqual(usage.providerReportedCalls, 0)
    }

    func testInvalidVectorsAloneMarkHealthDegraded() {
        let status = MemoryEmbeddingsStatusResult(
            projectId: "p",
            enabled: true,
            configured: true,
            model: "embed",
            dimensions: 8,
            indexable: 2,
            indexed: 2,
            invalid: 1,
            pending: 0,
            failed: 0,
            skipped: 0,
            coverage: 1,
            lastError: nil,
            usage: MemoryEmbeddingUsage(),
            recentJobs: []
        )
        XCTAssertEqual(status.healthLabel, "degraded")
        XCTAssertEqual(status.coverageLabel, "100.0%")
    }

    func testPolicyEncodingRoundTripsWithoutEmbeddingOrSecrets() throws {
        let policy = MemoryPolicy(
            enabled: false,
            maxItems: nil,
            maxAgeDays: nil,
            taskResultVerifyDays: nil,
            defaultVerifyDays: nil,
            autoSupersede: nil,
            briefMaxChars: nil,
            sweepStale: nil,
            embedding: nil
        )
        let data = try JSONEncoder().encode(policy)
        let decoded = try JSONDecoder().decode(MemoryPolicy.self, from: data)
        XCTAssertNil(decoded.embedding)
        XCTAssertNil(decoded.maxItems)
        let object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: data) as? [String: Any]
        )
        XCTAssertNil(object["api_key_env"])
        XCTAssertNoThrow(try JSONSerialization.data(withJSONObject: object))
    }
}
