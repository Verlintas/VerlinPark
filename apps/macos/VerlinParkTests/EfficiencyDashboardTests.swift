import Foundation
@testable import VerlinPark
import XCTest

final class EfficiencyDashboardTests: XCTestCase {
    func testUsageTotalsDecodesReportedCacheHitRatio() throws {
        let ledger = try decodeLedger(
            totals: """
            {
              "calls": 2,
              "input_tokens": 1000,
              "output_tokens": 100,
              "cache_read_tokens": 600,
              "cache_write_tokens": 100,
              "cache_hit_ratio": 0.9936,
              "cost_usd": 0.25,
              "by_role": [],
              "by_model": [],
              "by_agent": [],
              "by_task": []
            }
            """
        )
        XCTAssertEqual(ledger.totals.cacheHitRatio ?? 0, 0.9936, accuracy: 0.0001)
        XCTAssertEqual(EfficiencyMetrics.cacheHitRatio(ledger.totals) ?? 0, 0.9936, accuracy: 0.0001)
    }

    func testUsageTotalsWithoutCacheHitRatioFallsBackToTokenMath() throws {
        let ledger = try decodeLedger(
            totals: """
            {
              "calls": 2,
              "input_tokens": 1000,
              "output_tokens": 100,
              "cache_read_tokens": 600,
              "cache_write_tokens": 100,
              "cost_usd": 0.25,
              "by_role": [],
              "by_model": [],
              "by_agent": [],
              "by_task": []
            }
            """
        )
        XCTAssertNil(ledger.totals.cacheHitRatio)
        XCTAssertEqual(EfficiencyMetrics.uncachedInputTokens(ledger.totals), 300)
        XCTAssertEqual(EfficiencyMetrics.cacheHitRatio(ledger.totals) ?? 0, 0.6, accuracy: 0.0001)
    }

    func testSessionCacheStatusDecodesEmptyPayload() throws {
        let status = try JSONDecoder().decode(
            SessionCacheStatus.self,
            from: Data("{}".utf8)
        )
        XCTAssertNil(status.sessionID)
        XCTAssertNil(status.inputTokens)
        XCTAssertNil(status.cacheReadTokens)
        XCTAssertNil(status.cacheWriteTokens)
        XCTAssertNil(status.cacheHitRatio)
        XCTAssertNil(status.promptPrefixHash)
        XCTAssertNil(status.preambleTokensEstimate)
        XCTAssertNil(status.resolvedUncachedInputTokens)
    }

    func testSessionCacheStatusDecodesPopulatedPayloadAndAliases() throws {
        let json = """
        {
          "session_id": "session-1",
          "input_tokens": 1000,
          "cache_read_tokens": 600,
          "cache_write_tokens": 100,
          "cache_hit_ratio": 0.75,
          "last_call_at": "2026-11-11T00:00:00Z",
          "prefix_hash": "abc123",
          "preamble_tokens_estimate": 7000
        }
        """
        let status = try JSONDecoder().decode(SessionCacheStatus.self, from: Data(json.utf8))
        XCTAssertEqual(status.sessionID, "session-1")
        XCTAssertEqual(status.inputTokens, 1000)
        XCTAssertEqual(status.cacheReadTokens, 600)
        XCTAssertEqual(status.cacheWriteTokens, 100)
        XCTAssertEqual(status.cacheHitRatio, 0.75)
        XCTAssertEqual(status.lastCallAt, "2026-11-11T00:00:00Z")
        XCTAssertEqual(status.promptPrefixHash, "abc123")
        XCTAssertEqual(status.preambleTokensEstimate, 7000)
        XCTAssertEqual(status.resolvedUncachedInputTokens, 300)
        XCTAssertEqual(EfficiencyMetrics.cacheHitRatio(status) ?? 0, 0.75, accuracy: 0.0001)
    }

    func testSessionCacheStatusDerivesRatioWhenDaemonOmitsIt() throws {
        let json = """
        {
          "session_id": "session-1",
          "uncached_input_tokens": 400,
          "cache_read_tokens": 600,
          "cache_write_tokens": 100
        }
        """
        let status = try JSONDecoder().decode(SessionCacheStatus.self, from: Data(json.utf8))
        XCTAssertNil(status.cacheHitRatio)
        XCTAssertEqual(status.resolvedUncachedInputTokens, 400)
        XCTAssertEqual(
            EfficiencyMetrics.cacheHitRatio(status) ?? 0,
            600 / 1100,
            accuracy: 0.0001
        )
    }

    func testCacheHitRatioHandlesZeroDenominatorAndClamps() throws {
        XCTAssertNil(
            EfficiencyMetrics.cacheHitRatio(
                cacheReadTokens: 0,
                cacheWriteTokens: 0,
                uncachedInputTokens: 0
            )
        )
        XCTAssertEqual(EfficiencyMetrics.clampRatio(1.4), 1)
        XCTAssertEqual(EfficiencyMetrics.clampRatio(-0.2), 0)
        XCTAssertNil(EfficiencyMetrics.clampRatio(.infinity))

        let ledger = try decodeLedger(
            totals: """
            {
              "calls": 1,
              "input_tokens": 100,
              "output_tokens": 10,
              "cache_read_tokens": 600,
              "cache_write_tokens": 100,
              "cost_usd": 0.1,
              "by_role": [],
              "by_model": [],
              "by_agent": [],
              "by_task": []
            }
            """
        )
        XCTAssertEqual(EfficiencyMetrics.uncachedInputTokens(ledger.totals), 0)
        XCTAssertEqual(
            EfficiencyMetrics.cacheHitRatio(ledger.totals) ?? 0,
            600 / 700,
            accuracy: 0.0001
        )
    }

    func testEfficiencyFormattingHelpers() {
        XCTAssertEqual(EfficiencyMetrics.percent(0.9936), "99.4%")
        XCTAssertEqual(EfficiencyMetrics.percent(nil), "—")
        XCTAssertEqual(EfficiencyMetrics.tokenLabel(17_350_000_000), "17.35B")
        XCTAssertEqual(EfficiencyMetrics.tokenLabel(111_300_000), "111.3M")
        XCTAssertEqual(EfficiencyMetrics.tokenLabel(1500), "1.5K")
        XCTAssertEqual(EfficiencyMetrics.tokenLabel(999), "999")
        XCTAssertEqual(EfficiencyMetrics.tokenLabel(nil), "—")
        XCTAssertEqual(EfficiencyMetrics.usd(68.75), "$68.7500")
        XCTAssertEqual(EfficiencyMetrics.usd(0.0034, digits: 2), "$0.00")
        XCTAssertEqual(EfficiencyMetrics.average(total: 10, count: 4), 2.5)
        XCTAssertNil(EfficiencyMetrics.average(total: 10, count: 0))
        XCTAssertEqual(EfficiencyMetrics.shortHash("abcdef123456"), "abcdef1234")
        XCTAssertNil(EfficiencyMetrics.shortHash(""))
    }

    func testCompactionCountUsesEventsOrLastCompaction() {
        XCTAssertEqual(
            EfficiencyMetrics.compactionCount(
                eventTypes: ["context.compacted", "agent.usage", "context.compacted"],
                hasLastCompaction: true
            ),
            2
        )
        XCTAssertEqual(
            EfficiencyMetrics.compactionCount(
                eventTypes: ["agent.usage"],
                hasLastCompaction: true
            ),
            1
        )
        XCTAssertNil(
            EfficiencyMetrics.compactionCount(
                eventTypes: ["agent.usage"],
                hasLastCompaction: false
            )
        )
    }

    func testSnapshotPrefersDaemonRatioAndReportsProjectScope() throws {
        let projectUsage = try JSONDecoder().decode(
            ProjectUsageResult.self,
            from: Data(
                """
                {
                  "project_id": "project-1",
                  "sessions": [],
                  "totals": {
                    "calls": 4,
                    "input_tokens": 1000,
                    "output_tokens": 200,
                    "cache_read_tokens": 600,
                    "cache_write_tokens": 100,
                    "cache_hit_ratio": 0.9,
                    "cost_usd": 1.0,
                    "by_role": [],
                    "by_model": [],
                    "by_agent": [],
                    "by_task": []
                  }
                }
                """.utf8
            )
        )
        let context = try JSONDecoder().decode(
            SessionContextStatus.self,
            from: Data(
                """
                {
                  "session_id": "session-1",
                  "usage": {
                    "estimated_tokens": 91000,
                    "context_window": 128000,
                    "threshold_tokens": 90000,
                    "hard_limit_tokens": 120000,
                    "messages": 42,
                    "preamble_tokens_estimate": 7000,
                    "created_at": "2026-11-11T00:00:00Z"
                  },
                  "last_compaction": {
                    "mode": "summary",
                    "summarized_messages": 18,
                    "dropped_messages": 0,
                    "kept_messages": 4,
                    "estimated_tokens_before": 110000,
                    "estimated_tokens_after": 12000,
                    "created_at": "2026-11-11T00:00:01Z"
                  }
                }
                """.utf8
            )
        )
        let cache = try JSONDecoder().decode(
            SessionCacheStatus.self,
            from: Data(
                #"{"session_id":"session-1","cache_hit_ratio":0.8,"prompt_prefix_hash":"abc123"}"#
                    .utf8
            )
        )

        let snapshot = EfficiencyMetrics.makeSnapshot(
            projectUsage: projectUsage,
            usage: nil,
            context: context,
            cache: cache,
            eventTypes: ["context.compacted"],
            latestPrefixHash: nil
        )

        XCTAssertEqual(snapshot?.scope, EfficiencySnapshot.Scope.project)
        XCTAssertEqual(snapshot?.cacheHitRatio ?? 0, 0.8, accuracy: 0.0001)
        XCTAssertTrue(snapshot?.ratioIsFromDaemon == true)
        XCTAssertEqual(snapshot?.sessionCount, 0)
        XCTAssertEqual(snapshot?.compactionCount, 1)
        XCTAssertEqual(snapshot?.preambleTokensEstimate, 7000)
        XCTAssertEqual(snapshot?.promptPrefixHash, "abc123")
        XCTAssertEqual(snapshot?.contextEstimatedTokens, 91000)
        XCTAssertEqual(snapshot?.contextWindow, 128_000)
        XCTAssertEqual(snapshot?.contextThresholdTokens, 90000)
        XCTAssertNil(snapshot?.costPerSession)
    }

    func testSnapshotDerivesSessionRatioFromUsageWithoutCacheRPC() throws {
        let ledger = try decodeLedger(
            totals: """
            {
              "calls": 1,
              "input_tokens": 1000,
              "output_tokens": 100,
              "cache_read_tokens": 600,
              "cache_write_tokens": 100,
              "cost_usd": 0.25,
              "by_role": [],
              "by_model": [],
              "by_agent": [],
              "by_task": []
            }
            """
        )

        let snapshot = EfficiencyMetrics.makeSnapshot(
            projectUsage: nil,
            usage: ledger,
            context: nil,
            cache: nil,
            eventTypes: [],
            latestPrefixHash: "event-prefix"
        )

        XCTAssertEqual(snapshot?.scope, EfficiencySnapshot.Scope.session)
        XCTAssertEqual(snapshot?.cacheHitRatio ?? 0, 0.6, accuracy: 0.0001)
        XCTAssertFalse(snapshot?.ratioIsFromDaemon == true)
        XCTAssertEqual(snapshot?.sessionCount, 1)
        XCTAssertEqual(snapshot?.costPerSession ?? 0, 0.25, accuracy: 0.0001)
        XCTAssertEqual(snapshot?.promptPrefixHash, "event-prefix")
        XCTAssertNil(snapshot?.compactionCount)
    }

    private func decodeLedger(totals: String) throws -> UsageLedgerResult {
        let json = """
        {
          "session_id": "session-1",
          "entries": [],
          "totals": \(totals)
        }
        """
        return try JSONDecoder().decode(UsageLedgerResult.self, from: Data(json.utf8))
    }
}
