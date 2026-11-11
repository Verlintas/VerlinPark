import Foundation
@testable import VerlinPark
import XCTest

final class TestCacheTests: XCTestCase {
    func testPolicyDecodesAdditivelyWithIntegratorOffByDefault() throws {
        let policy = try JSONDecoder().decode(
            TestCachePolicy.self,
            from: Data("{}".utf8)
        )
        XCTAssertTrue(policy.enabled)
        XCTAssertTrue(policy.reuseLint)
        XCTAssertFalse(policy.includeIntegration)
        XCTAssertEqual(policy.maxEntries, 512)
        XCTAssertEqual(policy.retentionDays, 14)
        XCTAssertEqual(policy.failedRetentionSecs, 900)

        let disabled = try JSONDecoder().decode(
            TestCachePolicy.self,
            from: Data(#"{"enabled":false}"#.utf8)
        )
        XCTAssertFalse(disabled.enabled)
        XCTAssertEqual(disabled.maxOutputBytes, 256 * 1024)
    }

    func testEntryDecodesMetadataOnlyAndNeverAssumesAPass() throws {
        let json = """
        {
          "id": "tr-1",
          "project_id": "prj-1",
          "session_id": "ses-1",
          "kind": "test",
          "command": "cargo test",
          "command_source": "Cargo.toml",
          "cache_key": "v1:abc",
          "inputs_hash": "inputs",
          "impact_generation": "impact",
          "status": "failed",
          "exit_code": 101,
          "duration_ms": 1200,
          "created_at": "2026-11-12T00:00:00Z",
          "expires_at": "2026-11-12T00:15:00Z"
        }
        """
        let entry = try JSONDecoder().decode(TestCacheEntry.self, from: Data(json.utf8))
        XCTAssertFalse(entry.isPass)
        XCTAssertEqual(entry.hitCount, 0)
        XCTAssertEqual(entry.coveredPaths, [])
        XCTAssertEqual(entry.keyVersion, 1)
        XCTAssertEqual(TestCacheFormat.duration(entry.durationMs), "1.2s")
    }

    func testStatsDecodesCountersAndPolicy() throws {
        let json = """
        {
          "project_id": "prj-1",
          "entries": 3,
          "passed": 2,
          "failed": 1,
          "error": 0,
          "expired": 1,
          "total_hits": 7,
          "output_bytes": 4096,
          "by_kind": {"test": 2, "lint": 1},
          "oldest_created_at": "2026-11-10T00:00:00Z",
          "newest_created_at": "2026-11-12T00:00:00Z",
          "policy": {},
          "generated_at": "2026-11-12T01:00:00Z"
        }
        """
        let stats = try JSONDecoder().decode(TestCacheStats.self, from: Data(json.utf8))
        XCTAssertEqual(stats.entries, 3)
        XCTAssertEqual(stats.totalHits, 7)
        XCTAssertEqual(stats.byKind["test"], 2)
        XCTAssertFalse(stats.policy.includeIntegration)
        XCTAssertEqual(stats.passRatio, 2.0 / 3.0, accuracy: 0.0001)
    }

    func testImpactMapLabelsEverySourceAndHandlesTruncation() throws {
        let json = """
        {
          "changed_paths": ["src/lib.rs"],
          "targets": [
            {
              "path": "tests/smoke.rs",
              "reason": "lexical import",
              "confidence": 0.9,
              "source": "lexical"
            },
            {
              "path": "tests/history.rs",
              "reason": "cached coverage",
              "confidence": 0.8,
              "source": "history"
            }
          ],
          "generation": "impact-v1:abc",
          "profile_hash": "profile",
          "sources": {"lexical": 1, "history": 1},
          "dependency_paths": ["src/lib.rs"],
          "truncated": true,
          "lsp_available": false,
          "lexical_only": false,
          "notes": ["heuristic"]
        }
        """
        let result = try JSONDecoder().decode(
            TestCacheImpactResult.self,
            from: Data(
                """
                {
                  "session_id": "ses-1",
                  "project_id": "prj-1",
                  "map": \(json),
                  "test_command": "cargo test"
                }
                """.utf8
            )
        )
        XCTAssertEqual(result.map.targets.count, 2)
        XCTAssertEqual(result.map.targets[0].sourceLabel, "Lexical")
        XCTAssertEqual(result.map.targets[1].sourceLabel, "History")
        XCTAssertTrue(result.map.truncated)
        XCTAssertFalse(result.map.lspAvailable)
    }

    func testCacheBadgeIsOnlyReportedFromStructuredRuntimeProvenance() throws {
        let hit = try JSONDecoder().decode(
            JSONValue.self,
            from: Data(
                #"{"cache":{"cache_hit":true,"decision":"hit"},"cache_hit":true}"#.utf8
            )
        )
        XCTAssertEqual(ToolTrace.cacheProvenance(hit), "CACHED")

        let miss = try JSONDecoder().decode(
            JSONValue.self,
            from: Data(#"{"cache":{"cache_hit":false,"decision":"miss"}}"#.utf8)
        )
        XCTAssertNil(ToolTrace.cacheProvenance(miss))
        XCTAssertNil(ToolTrace.cacheProvenance(nil))
        XCTAssertNil(ToolTrace.cacheProvenance(.object(["cache_hit": .bool(true)])))
    }

    func testPruneAndClearResultsDecode() throws {
        let prune = try JSONDecoder().decode(
            TestCachePruneResult.self,
            from: Data(
                """
                {
                  "project_id": "prj-1",
                  "pruned": 2,
                  "remaining": 4,
                  "dry_run": true,
                  "affected_sessions": 1,
                  "removed_ids": ["tr-1", "tr-2"],
                  "reason": "retention"
                }
                """.utf8
            )
        )
        XCTAssertEqual(prune.pruned, 2)
        XCTAssertTrue(prune.dryRun)
        XCTAssertEqual(prune.removedIds.count, 2)

        let clear = try JSONDecoder().decode(
            TestCacheClearResult.self,
            from: Data(
                #"{"project_id":"prj-1","cleared":3,"remaining":0,"affected_sessions":1}"#
                    .utf8
            )
        )
        XCTAssertEqual(clear.cleared, 3)
        XCTAssertEqual(clear.affectedSessions, 1)
    }
}
