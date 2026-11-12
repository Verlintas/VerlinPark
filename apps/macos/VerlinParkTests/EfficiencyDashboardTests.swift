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

    func testSessionCacheStatusDecodesCacheFirstTelemetry() throws {
        let json = """
        {
          "session_id": "session-1",
          "input_tokens": 1000,
          "cache_read_tokens": 600,
          "cache_write_tokens": 100,
          "cache_hit_ratio": 0.6,
          "cache_read_ratio": 0.6,
          "cache_write_ratio": 0.1,
          "estimated_savings_usd": 0.00155,
          "cache_calls": 4,
          "prompt_prefix_hash": "abc123",
          "cache_generation": "abc123",
          "core_hash": "core",
          "project_hash": "project",
          "tools_hash": "tools",
          "volatile_hash": "volatile",
          "last_breakpoints": 4,
          "last_cache_ttl": "1h",
          "last_miss_reason": "ttl_or_provider_miss",
          "last_first_token_ms": 321,
          "recent_calls": [{
            "at": "2026-11-11T00:00:00Z",
            "agent_name": "Leader",
            "agent_role": "leader",
            "input_tokens": 100,
            "cache_read_tokens": 60,
            "cache_breakpoints": 4,
            "cache_ttl": "1h",
            "first_token_ms": 120,
            "estimated_savings_usd": 0.0002,
            "miss_reason": "first_call",
            "warm": false
          }],
          "policy": {
            "enabled": true,
            "ttl": "1h",
            "automatic_caching": true,
            "warm_on_resume": true,
            "compact_aware": true
          }
        }
        """
        let status = try JSONDecoder().decode(SessionCacheStatus.self, from: Data(json.utf8))
        XCTAssertEqual(status.estimatedSavingsUSD ?? 0, 0.00155, accuracy: 0.000001)
        XCTAssertEqual(status.cacheCalls, 4)
        XCTAssertEqual(status.cacheReadRatio ?? 0, 0.6, accuracy: 0.0001)
        XCTAssertEqual(status.cacheWriteRatio ?? 0, 0.1, accuracy: 0.0001)
        XCTAssertEqual(status.lastBreakpoints, 4)
        XCTAssertEqual(status.lastCacheTTL, "1h")
        XCTAssertEqual(status.lastMissReason, "ttl_or_provider_miss")
        XCTAssertEqual(status.lastFirstTokenMS, 321)
        XCTAssertEqual(status.recentCalls.count, 1)
        XCTAssertEqual(status.recentCalls[0].cacheBreakpoints, 4)
        XCTAssertEqual(status.recentCalls[0].missReason, "first_call")
        XCTAssertEqual(status.policy?.warmOnResume, true)
        XCTAssertEqual(status.policy?.compactAware, true)
    }

    func testCacheWarmResultDecodesOptionalFields() throws {
        let json = """
        {
          "session_id": "session-1",
          "warmed": true,
          "provider": "anthropic",
          "model": "claude",
          "role": "worker",
          "source": "manual",
          "cache_breakpoints": 4,
          "cache_ttl": "1h",
          "automatic": true,
          "identity_hash": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
          "identity_id": "agt-worker-1",
          "identity_generation": 3,
          "input_tokens": 1000,
          "output_tokens": 2,
          "cache_read_tokens": 0,
          "cache_write_tokens": 900,
          "estimated": false,
          "cost_usd": 0.003,
          "latency_ms": 240,
          "first_token_ms": 120,
          "message": "OK"
        }
        """
        let result = try JSONDecoder().decode(SessionCacheWarmResult.self, from: Data(json.utf8))
        XCTAssertTrue(result.warmed)
        XCTAssertEqual(result.role, "worker")
        XCTAssertEqual(result.cacheBreakpoints, 4)
        XCTAssertEqual(result.cacheTTL, "1h")
        XCTAssertEqual(result.costUSD ?? 0, 0.003, accuracy: 0.000001)
        XCTAssertEqual(result.latencyMS, 240)
        XCTAssertEqual(result.identityID, "agt-worker-1")
        XCTAssertEqual(result.identityGeneration, 3)
        XCTAssertEqual(result.identityHash, String(repeating: "d", count: 64))
    }

    func testCacheAuditDecodesFindingsAndEvidence() throws {
        let json = """
        {
          "session_id": "session-1",
          "provider": "anthropic",
          "calls_checked": 6,
          "warm_calls_checked": 1,
          "compactions_checked": 2,
          "hit_ratio": 0.42,
          "cache_read_tokens": 600,
          "cache_write_tokens": 300,
          "findings": [{
            "code": "breakpoint_limit_exceeded",
            "severity": "critical",
            "message": "five breakpoints",
            "evidence": {"breakpoints": 5}
          }],
          "clean": false
        }
        """
        let audit = try JSONDecoder().decode(SessionCacheAuditResult.self, from: Data(json.utf8))
        XCTAssertEqual(audit.callsChecked, 6)
        XCTAssertEqual(audit.findings.count, 1)
        XCTAssertEqual(audit.findings[0].code, "breakpoint_limit_exceeded")
        XCTAssertEqual(audit.findings[0].evidence["breakpoints"], .number(5))
        XCTAssertFalse(audit.clean)
    }

    func testCachePlanDecodesRoleHashes() throws {
        let json = """
        {
          "session_id": "session-1",
          "provider": "anthropic",
          "policy": {"enabled": true, "ttl": "1h", "automatic_caching": true, "warm_on_resume": false, "compact_aware": true},
          "roles": [{
            "role": "leader",
            "provider": "anthropic",
            "model": "claude",
            "cache_breakpoints": 4,
            "cache_ttl": "1h",
            "ttl_explicit": true,
            "automatic": true,
            "tail_message": false,
            "tool_breakpoint": true,
            "core_breakpoint": true,
            "project_breakpoint": true,
            "core_hash": "core",
            "project_hash": "project",
            "tools_hash": "tools",
            "volatile_hash": "volatile",
            "prefix_hash": "prefix",
            "core_chars": 100,
            "project_chars": 50,
            "stable_prefix_chars": 150,
            "tool_count": 20,
            "system_tokens_estimate": 200,
            "tool_tokens_estimate": 400
          }]
        }
        """
        let plan = try JSONDecoder().decode(SessionCachePlanResult.self, from: Data(json.utf8))
        XCTAssertEqual(plan.provider, "anthropic")
        XCTAssertEqual(plan.roles.count, 1)
        XCTAssertEqual(plan.roles[0].role, "leader")
        XCTAssertEqual(plan.roles[0].cacheBreakpoints, 4)
        XCTAssertEqual(plan.roles[0].cacheTTL, "1h")
        XCTAssertEqual(plan.roles[0].prefixHash, "prefix")
        XCTAssertEqual(plan.policy?.ttl, "1h")
    }

    func testSessionCacheCallDecodesLegacyEmptyPayload() throws {
        let call = try JSONDecoder().decode(
            SessionCacheCall.self,
            from: Data("{}".utf8)
        )
        XCTAssertEqual(call.inputTokens, 0)
        XCTAssertEqual(call.cacheBreakpoints, 0)
        XCTAssertFalse(call.warm)
        XCTAssertNil(call.missReason)
        XCTAssertEqual(call.cacheMissTokens, 0)
        XCTAssertEqual(call.theoreticalMaxHitRatio, 0)
        XCTAssertNil(call.appendOnly)
        XCTAssertNil(call.cacheEpoch)
    }

    func testSessionCacheL5FieldsDecodeOnStatusAndCall() throws {
        let json = """
        {
          "session_id": "session-1",
          "input_tokens": 200000,
          "cache_read_tokens": 199000,
          "cache_miss_tokens": 1000,
          "cache_hit_ratio": 0.995,
          "theoretical_max_hit_ratio": 0.997,
          "cache_epoch": "epoch-abc",
          "cache_epoch_source": "prompt_prefix_v3",
          "append_only_calls": 49,
          "append_only_breaks": 0,
          "last_append_only": true,
          "hit_ratio_p50": 0.996,
          "hit_ratio_p95": 0.999,
          "theoretical_max_p50": 0.997,
          "theoretical_max_p95": 0.999,
          "cost_usd": 0.12,
          "cache_epoch_timeline": [{
            "epoch": "epoch-abc",
            "source": "prompt_prefix_v3",
            "calls": 50,
            "first_at": "a",
            "last_at": "b",
            "input_tokens": 1000,
            "cache_read_tokens": 900,
            "cache_miss_tokens": 100,
            "hit_ratio": 0.9,
            "append_only_breaks": 0
          }],
          "per_role": [{
            "role": "leader",
            "calls": 50,
            "input_tokens": 200000,
            "cache_read_tokens": 199000,
            "cache_miss_tokens": 1000,
            "hit_ratio": 0.995,
            "theoretical_max_hit_ratio": 0.997,
            "cost_usd": 0.12
          }],
          "top_miss_reasons": [{"key": "tail_delta", "calls": 49}],
          "cross_session_reuse_sessions": 2,
          "cross_session_hit_tokens": 4000,
          "cross_session_calls_observed": 8,
          "compaction_lost_cache_usd": 0.003,
          "recent_calls": [{
            "at": "now",
            "agent_role": "leader",
            "input_tokens": 200000,
            "cache_read_tokens": 199000,
            "cache_miss_tokens": 1000,
            "cache_hit_ratio": 0.995,
            "theoretical_max_hit_ratio": 0.997,
            "message_prefix_hash": "message",
            "previous_message_prefix_hash": "previous",
            "message_prefix_count": 12,
            "previous_message_prefix_count": 10,
            "append_only": true,
            "cache_epoch": "epoch-abc",
            "cache_epoch_source": "prompt_prefix_v3",
            "previous_input_tokens": 198000,
            "tool_bundle": "leader",
            "tool_bundle_hash": "bundle",
            "miss_reason": "tail_delta"
          }]
        }
        """
        let status = try JSONDecoder().decode(SessionCacheStatus.self, from: Data(json.utf8))
        XCTAssertEqual(status.cacheMissTokens, 1000)
        XCTAssertEqual(status.theoreticalMaxHitRatio ?? 0, 0.997, accuracy: 0.0001)
        XCTAssertEqual(status.cacheEpoch, "epoch-abc")
        XCTAssertEqual(status.cacheEpochSource, "prompt_prefix_v3")
        XCTAssertEqual(status.appendOnlyCalls, 49)
        XCTAssertEqual(status.appendOnlyBreaks, 0)
        XCTAssertEqual(status.lastAppendOnly, true)
        XCTAssertEqual(status.hitRatioP50 ?? 0, 0.996, accuracy: 0.0001)
        XCTAssertEqual(status.cacheEpochTimeline.count, 1)
        XCTAssertEqual(status.cacheEpochTimeline[0].epoch, "epoch-abc")
        XCTAssertEqual(status.perRole.count, 1)
        XCTAssertEqual(status.perRole[0].role, "leader")
        XCTAssertEqual(status.topMissReasons[0].key, "tail_delta")
        XCTAssertEqual(status.crossSessionReuseSessions, 2)
        XCTAssertEqual(status.compactionLostCacheUSD ?? 0, 0.003, accuracy: 0.000001)
        XCTAssertEqual(status.recentCalls.count, 1)
        let call = status.recentCalls[0]
        XCTAssertEqual(call.cacheMissTokens, 1000)
        XCTAssertEqual(call.theoreticalMaxHitRatio, 0.997, accuracy: 0.0001)
        XCTAssertEqual(call.appendOnly, true)
        XCTAssertEqual(call.cacheEpoch, "epoch-abc")
        XCTAssertEqual(call.messagePrefixCount, 12)
        XCTAssertEqual(call.previousMessagePrefixCount, 10)
        XCTAssertEqual(call.toolBundle, "leader")
    }

    func testSnapshotCarriesL5CachePanelFields() throws {
        let cache = try JSONDecoder().decode(
            SessionCacheStatus.self,
            from: Data(
                #"{"session_id":"session-1","input_tokens":1000,"cache_read_tokens":900,"cache_miss_tokens":100,"cache_hit_ratio":0.9,"theoretical_max_hit_ratio":0.95,"last_append_only":true,"append_only_calls":3,"append_only_breaks":0,"hit_ratio_p50":0.9,"hit_ratio_p95":0.95,"cost_usd":0.02,"cross_session_reuse_sessions":2,"cross_session_hit_tokens":500,"compaction_lost_cache_usd":0.001,"top_miss_reasons":[{"key":"tail_delta","calls":3}],"per_role":[{"role":"leader","calls":3,"hit_ratio":0.9,"cache_miss_tokens":100,"cost_usd":0.02}],"cache_epoch":"epoch","cache_epoch_source":"prompt_prefix_v3"}"#
                    .utf8
            )
        )
        let snapshot = EfficiencyMetrics.makeSnapshot(
            projectUsage: nil,
            usage: nil,
            context: nil,
            cache: cache,
            toolOutputs: nil,
            eventTypes: [],
            latestPrefixHash: nil
        )
        XCTAssertEqual(snapshot?.cacheMissTokens, 100)
        XCTAssertEqual(snapshot?.resolvedCacheMissTokens, 100)
        XCTAssertEqual(snapshot?.theoreticalMaxHitRatio ?? 0, 0.95, accuracy: 0.0001)
        XCTAssertEqual(snapshot?.appendOnlyCalls, 3)
        XCTAssertEqual(snapshot?.appendOnlyBreaks, 0)
        XCTAssertEqual(snapshot?.lastAppendOnly, true)
        XCTAssertEqual(snapshot?.cacheEpoch, "epoch")
        XCTAssertEqual(snapshot?.topMissReasons.first?.key, "tail_delta")
        XCTAssertEqual(snapshot?.perRole.first?.role, "leader")
        XCTAssertEqual(snapshot?.crossSessionReuseSessions, 2)
        XCTAssertEqual(snapshot?.compactionLostCacheUSD ?? 0, 0.001, accuracy: 0.000001)
        XCTAssertEqual(snapshot?.cacheHeadroomGap ?? 0, 0.05, accuracy: 0.0001)
        XCTAssertEqual(snapshot?.appendOnlyKnownCalls, 3)
    }

    func testAgentInfoLeaderModelLabelUsesDaemonRoleOverride() throws {
        let json = """
        {
          "configured": true,
          "provider": "deepseek",
          "model": "deepseek-chat",
          "base_url": "https://api.deepseek.com",
          "role_models": {
            "leader": {"model": "deepseek-flash"},
            "manager": {},
            "worker": {},
            "verifier": {},
            "labour": {}
          }
        }
        """
        let info = try JSONDecoder().decode(AgentInfo.self, from: Data(json.utf8))
        XCTAssertEqual(info.leaderModelLabel, "deepseek-flash")
        XCTAssertEqual(info.configuredModelLabel, "deepseek-flash")

        let base = try JSONDecoder().decode(
            AgentInfo.self,
            from: Data(
                #"{"configured":true,"model":"deepseek-chat","role_models":{"leader":{},"manager":{},"worker":{},"verifier":{},"labour":{}}}"#
                    .utf8
            )
        )
        XCTAssertEqual(base.leaderModelLabel, "deepseek-chat")
        XCTAssertEqual(base.configuredModelLabel, "deepseek-chat")
        let unconfigured = try JSONDecoder().decode(
            AgentInfo.self,
            from: Data(
                #"{"configured":false,"model":"stale-local","role_models":{"leader":{},"manager":{},"worker":{},"verifier":{},"labour":{}}}"#
                    .utf8
            )
        )
        XCTAssertNil(unconfigured.configuredModelLabel)
    }

    func testCacheEpochTimelineDecodesLegacyFieldsAndUnknownSource() throws {
        let json = """
        {
          "session_id": "session-1",
          "cache_epoch_timeline": [
            {"epoch": "old", "calls": 2},
            {"epoch": "new", "source": "future_source", "calls": 3, "hit_ratio": 0.99, "append_only_breaks": 1}
          ]
        }
        """
        let status = try JSONDecoder().decode(SessionCacheStatus.self, from: Data(json.utf8))
        XCTAssertEqual(status.cacheEpochTimeline.count, 2)
        XCTAssertEqual(status.cacheEpochTimeline[0].epoch, "old")
        XCTAssertEqual(status.cacheEpochTimeline[0].calls, 2)
        XCTAssertNil(status.cacheEpochTimeline[0].source)
        XCTAssertEqual(status.cacheEpochTimeline[1].source, "future_source")
        XCTAssertEqual(status.cacheEpochTimeline[1].hitRatio, 0.99, accuracy: 0.0001)
        XCTAssertEqual(status.cacheEpochTimeline[1].appendOnlyBreaks, 1)
    }

    func testSessionCacheCallsResultDecodesBoundedTable() throws {
        let json = """
        {
          "session_id": "session-1",
          "observed": 42,
          "returned": 2,
          "truncated": true,
          "limit": 2,
          "role": "leader",
          "miss_reason": null,
          "include_warm": false,
          "calls": [
            {"at": "two", "input_tokens": 200, "cache_read_tokens": 190, "cache_miss_tokens": 10, "cache_hit_ratio": 0.95, "theoretical_max_hit_ratio": 0.96, "append_only": true, "miss_reason": "tail_delta"},
            {"at": "one", "input_tokens": 100, "cache_read_tokens": 90, "cache_miss_tokens": 10, "cache_hit_ratio": 0.9, "append_only": false, "miss_reason": "first_call"}
          ]
        }
        """
        let result = try JSONDecoder().decode(SessionCacheCallsResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.sessionID, "session-1")
        XCTAssertEqual(result.observed, 42)
        XCTAssertEqual(result.returned, 2)
        XCTAssertTrue(result.truncated)
        XCTAssertEqual(result.limit, 2)
        XCTAssertEqual(result.role, "leader")
        XCTAssertEqual(result.calls.count, 2)
        XCTAssertEqual(result.calls[0].appendOnly, true)
        XCTAssertEqual(result.calls[1].appendOnly, false)

        let legacy = try JSONDecoder().decode(
            SessionCacheCallsResult.self,
            from: Data(#"{"session_id":"session-1"}"#.utf8)
        )
        XCTAssertEqual(legacy.returned, 0)
        XCTAssertFalse(legacy.truncated)
        XCTAssertTrue(legacy.calls.isEmpty)
    }

    func testMissReasonLabelsCoverTailDeltaAndProviderMiss() {
        XCTAssertEqual(EfficiencyMetrics.missReasonLabel("tail_delta"), "new tail")
        XCTAssertEqual(EfficiencyMetrics.missReasonLabel("provider_cache_miss"), "provider miss")
        XCTAssertEqual(EfficiencyMetrics.missReasonLabel("ttl_or_provider_miss"), "TTL or provider miss")
    }

    func testSnapshotCarriesSavingsTtlBreakpointsAndMissReason() throws {
        let cache = try JSONDecoder().decode(
            SessionCacheStatus.self,
            from: Data(
                #"{"session_id":"session-1","estimated_savings_usd":-0.000075,"last_cache_ttl":"5m","last_breakpoints":3,"last_miss_reason":"compaction_reset","cache_calls":2}"#
                    .utf8
            )
        )
        let snapshot = EfficiencyMetrics.makeSnapshot(
            projectUsage: nil,
            usage: nil,
            context: nil,
            cache: cache,
            toolOutputs: nil,
            eventTypes: [],
            latestPrefixHash: nil
        )
        XCTAssertEqual(snapshot?.estimatedSavingsUSD ?? 0, -0.000075, accuracy: 0.0000001)
        XCTAssertEqual(snapshot?.lastCacheTTL, "5m")
        XCTAssertEqual(snapshot?.lastBreakpoints, 3)
        XCTAssertEqual(snapshot?.lastMissReason, "compaction_reset")
        XCTAssertEqual(snapshot?.cacheCalls, 2)
        XCTAssertEqual(snapshot?.recentCacheCalls.count, 0)
        XCTAssertEqual(EfficiencyMetrics.missReasonLabel("compaction_reset"), "compaction reset")
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
            toolOutputs: nil,
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
            toolOutputs: nil,
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

    func testToolOutputPolicyAndStatusDecodeDefaultsAndMetrics() throws {
        let policyJSON = """
        {
          "project_id": "project-1",
          "policy": {
            "enabled": true,
            "spill_threshold_bytes": 16384,
            "preview_bytes": 4096,
            "max_total_bytes_per_turn": 65536,
            "retention_secs": 604800,
            "max_entries": 2000,
            "tool_thresholds": {"shell": 131072}
          }
        }
        """
        let policyResult = try JSONDecoder().decode(
            ToolOutputPolicyResult.self,
            from: Data(policyJSON.utf8)
        )
        XCTAssertEqual(policyResult.policy.spillThresholdBytes, 16384)
        XCTAssertEqual(policyResult.policy.toolThresholds?["shell"], 131_072)

        let statusJSON = """
        {
          "project_id": "project-1",
          "session_id": "session-1",
          "policy": {
            "enabled": true,
            "spill_threshold_bytes": 16384,
            "preview_bytes": 4096,
            "max_total_bytes_per_turn": 65536,
            "retention_secs": 604800,
            "max_entries": 2000
          },
          "entries": 3,
          "total_bytes": 90000,
          "preview_bytes": 12000,
          "active_entries": 1,
          "expired_entries": 0,
          "oldest_created_at": "2026-11-11T00:00:00Z",
          "newest_created_at": "2026-11-11T01:00:00Z"
        }
        """
        let status = try JSONDecoder().decode(ToolOutputStatus.self, from: Data(statusJSON.utf8))
        XCTAssertEqual(status.entries, 3)
        XCTAssertEqual(status.totalBytes, 90000)
        XCTAssertEqual(status.activeEntries, 1)
        XCTAssertEqual(status.policy.maxEntries, 2000)
    }

    func testLegacyCompactionSummaryDecodesWithoutMetrics() throws {
        let json = """
        {
          "mode": "summary",
          "summarized_messages": 8,
          "dropped_messages": 0,
          "kept_messages": 4,
          "estimated_tokens_before": 12000,
          "estimated_tokens_after": 5000,
          "created_at": "2026-11-11T00:00:00Z"
        }
        """
        let summary = try JSONDecoder().decode(
            ContextCompactionSummary.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(summary.mode, "summary")
        XCTAssertNil(summary.latencyMs)
        XCTAssertNil(summary.backgroundReuse)
        XCTAssertNil(summary.compactionCount)
    }

    func testCompactionMetricsFeedTheEfficiencySnapshot() throws {
        let summaryJSON = """
        {
          "mode": "summary",
          "summarized_messages": 8,
          "dropped_messages": 0,
          "kept_messages": 4,
          "estimated_tokens_before": 12000,
          "estimated_tokens_after": 5000,
          "created_at": "2026-11-11T00:00:00Z",
          "latency_ms": 42,
          "background_reuse": true,
          "fallback_to_sync": false,
          "compaction_count": 2,
          "structured_state": {"goal": "ship C1", "plan_revision": 3}
        }
        """
        let summary = try JSONDecoder().decode(
            ContextCompactionSummary.self,
            from: Data(summaryJSON.utf8)
        )
        XCTAssertEqual(summary.latencyMs, 42)
        XCTAssertTrue(summary.backgroundReuse == true)
        XCTAssertEqual(summary.compactionCount, 2)

        let contextJSON = """
        {
          "session_id": "session-1",
          "last_compaction": \(summaryJSON),
          "compaction_count": 2,
          "last_compaction_latency_ms": 42,
          "compaction_background_reuse": true,
          "compaction_fallback_to_sync": false,
          "checkpoint_count": 2,
          "latest_checkpoint_id": "ctx-2",
          "latest_checkpoint_at": "2026-11-11T00:00:00Z"
        }
        """
        let context = try JSONDecoder().decode(SessionContextStatus.self, from: Data(contextJSON.utf8))
        let statusJSON = """
        {
          "policy": {
            "enabled": true,
            "spill_threshold_bytes": 16384,
            "preview_bytes": 4096,
            "max_total_bytes_per_turn": 65536,
            "retention_secs": 604800,
            "max_entries": 2000
          },
          "entries": 2,
          "total_bytes": 22000,
          "preview_bytes": 8000,
          "active_entries": 1,
          "expired_entries": 0
        }
        """
        let toolOutputs = try JSONDecoder().decode(ToolOutputStatus.self, from: Data(statusJSON.utf8))
        let snapshot = EfficiencyMetrics.makeSnapshot(
            projectUsage: nil,
            usage: nil,
            context: context,
            cache: nil,
            toolOutputs: toolOutputs,
            eventTypes: [],
            latestPrefixHash: nil
        )
        XCTAssertEqual(snapshot?.compactionCount, 2)
        XCTAssertEqual(snapshot?.checkpointCount, 2)
        XCTAssertEqual(snapshot?.latestCheckpointID, "ctx-2")
        XCTAssertEqual(snapshot?.compactionLatencyMs, 42)
        XCTAssertTrue(snapshot?.compactionBackgroundReuse == true)
        XCTAssertEqual(snapshot?.toolOutputEntries, 2)
        XCTAssertEqual(snapshot?.toolOutputBytes, 22000)
        XCTAssertEqual(snapshot?.toolOutputEnabled, true)
        XCTAssertEqual(snapshot?.toolOutputSpillThresholdBytes, 16384)
        XCTAssertEqual(snapshot?.toolOutputPreviewBytes, 4096)
        XCTAssertEqual(snapshot?.toolOutputRetentionSecs, 604_800)
        XCTAssertEqual(snapshot?.toolOutputMaxEntries, 2000)
    }

    func testToolOutputStatusDecodesPerToolBreakdown() throws {
        let json = """
        {
          "policy": {
            "enabled": true,
            "spill_threshold_bytes": 16384,
            "preview_bytes": 4096,
            "max_total_bytes_per_turn": 65536,
            "retention_secs": 604800,
            "max_entries": 2000
          },
          "entries": 3,
          "total_bytes": 90000,
          "preview_bytes": 12000,
          "active_entries": 1,
          "expired_entries": 0,
          "by_tool": [
            {"tool_name": "shell", "entries": 2, "bytes": 80000, "preview_bytes": 10000},
            {"tool_name": "grep", "entries": 1, "bytes": 10000, "preview_bytes": 2000}
          ]
        }
        """
        let status = try JSONDecoder().decode(ToolOutputStatus.self, from: Data(json.utf8))
        XCTAssertEqual(status.byTool?.count, 2)
        XCTAssertEqual(status.byTool?.first?.toolName, "shell")
        XCTAssertEqual(status.byTool?.first?.entries, 2)
    }

    func testToolOutputAuditDecodesHonestFailureCounts() throws {
        let json = """
        {
          "rows": 4,
          "blob_files": 3,
          "missing_blobs": 1,
          "hash_mismatches": 1,
          "unsafe_blob_files": 1,
          "orphan_blobs": 1,
          "orphan_bytes": 100,
          "cleaned_orphans": 0,
          "cleaned_bytes": 0,
          "checked_at": "2026-11-11T00:00:00Z"
        }
        """
        let audit = try JSONDecoder().decode(ToolOutputAuditResult.self, from: Data(json.utf8))
        XCTAssertEqual(audit.rows, 4)
        XCTAssertEqual(audit.missingBlobs, 1)
        XCTAssertEqual(audit.hashMismatches, 1)
        XCTAssertEqual(audit.orphanBytes, 100)
        XCTAssertEqual(audit.cleanedOrphans, 0)
    }

    func testCompactionSummaryTopLevelTokensDecodeAdditively() throws {
        let json = """
        {
          "session_id": "session-1",
          "compaction_count": 4,
          "last_compaction_latency_ms": 12,
          "last_compaction_tokens_before": 9000,
          "last_compaction_tokens_after": 3000,
          "compaction_background_reuse": false,
          "compaction_fallback_to_sync": true
        }
        """
        let status = try JSONDecoder().decode(SessionContextStatus.self, from: Data(json.utf8))
        XCTAssertEqual(status.compactionCount, 4)
        XCTAssertEqual(status.lastCompactionLatencyMs, 12)
        XCTAssertEqual(status.lastCompactionTokensBefore, 9000)
        XCTAssertEqual(status.lastCompactionTokensAfter, 3000)
        XCTAssertFalse(status.compactionBackgroundReuse == true)
        XCTAssertTrue(status.compactionFallbackToSync == true)
        XCTAssertNil(status.lastCompaction)
        XCTAssertNil(status.usage)
    }

    func testToolOutputPolicyDefaultMatchesTheDaemonDefaults() {
        let policy = ToolOutputPolicy.default
        XCTAssertTrue(policy.enabled)
        XCTAssertEqual(policy.spillThresholdBytes, 16384)
        XCTAssertEqual(policy.previewBytes, 4096)
        XCTAssertEqual(policy.maxTotalBytesPerTurn, 65536)
        XCTAssertEqual(policy.retentionSecs, 604_800)
        XCTAssertEqual(policy.maxEntries, 2000)
        XCTAssertNil(policy.toolThresholds)
    }

    func testToolOutputEntryDecodesAndKeepsMetadataOnlyShape() throws {
        let json = """
        {
          "id": "out_00000000-0000-0000-0000-000000000000",
          "session_id": "session-1",
          "tool_call_id": "call-1",
          "tool_name": "shell",
          "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          "bytes": 1000,
          "lines": 10,
          "preview_bytes": 200,
          "created_at": "2026-11-11T00:00:00Z",
          "expires_at": "2026-11-18T00:00:00Z"
        }
        """
        let entry = try JSONDecoder().decode(ToolOutputEntry.self, from: Data(json.utf8))
        XCTAssertEqual(entry.id, "out_00000000-0000-0000-0000-000000000000")
        XCTAssertEqual(entry.toolName, "shell")
        XCTAssertEqual(entry.toolCallId, "call-1")
        XCTAssertEqual(entry.bytes, 1000)
        XCTAssertEqual(entry.lines, 10)
        XCTAssertEqual(entry.previewBytes, 200)
        XCTAssertEqual(entry.expiresAt, "2026-11-18T00:00:00Z")
    }

    func testStructuredStateDecodesFromACompactionSummary() throws {
        let json = """
        {
          "mode": "trim",
          "summarized_messages": 0,
          "dropped_messages": 2,
          "kept_messages": 3,
          "estimated_tokens_before": 9000,
          "estimated_tokens_after": 4000,
          "created_at": "2026-11-11T00:00:00Z",
          "structured_state": {
            "goal": "ship C1",
            "plan_revision": 2,
            "active_task_id": "T2",
            "active_task_title": "paging",
            "active_task_department": "runtime",
            "active_task_acceptance": ["tests pass"],
            "active_task_write_scope": ["crates/vp-core"],
            "recent_completed_task_ids": ["T1"],
            "generated_at": "2026-11-11T00:00:00Z"
          }
        }
        """
        let summary = try JSONDecoder().decode(
            ContextCompactionSummary.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(summary.mode, "trim")
        XCTAssertEqual(summary.structuredState?.objectValue?["plan_revision"]?.intValue, 2)
        XCTAssertEqual(summary.structuredState?.objectValue?["active_task_id"]?.stringValue, "T2")
    }

    func testLegacyToolOutputStatusHasNoPerToolBreakdown() throws {
        let json = """
        {
          "policy": {
            "enabled": true,
            "spill_threshold_bytes": 16384,
            "preview_bytes": 4096,
            "max_total_bytes_per_turn": 65536,
            "retention_secs": 604800,
            "max_entries": 2000
          },
          "entries": 0,
          "total_bytes": 0,
          "preview_bytes": 0,
          "active_entries": 0,
          "expired_entries": 0
        }
        """
        let status = try JSONDecoder().decode(ToolOutputStatus.self, from: Data(json.utf8))
        XCTAssertNil(status.byTool)
        XCTAssertNil(status.oldestCreatedAt)
        XCTAssertNil(status.newestCreatedAt)
    }

    func testLegacyCompactionSummaryHasNoStructuredState() throws {
        let json = """
        {
          "mode": "trim",
          "summarized_messages": 0,
          "dropped_messages": 2,
          "kept_messages": 3,
          "estimated_tokens_before": 9000,
          "estimated_tokens_after": 4000,
          "created_at": "2026-11-11T00:00:00Z"
        }
        """
        let summary = try JSONDecoder().decode(
            ContextCompactionSummary.self,
            from: Data(json.utf8)
        )
        XCTAssertNil(summary.structuredState)
        XCTAssertNil(summary.latencyMs)
        XCTAssertNil(summary.compactionCount)
    }

    func testToolOutputListResultDecodesMetadataPage() throws {
        let json = """
        {
          "entries": [],
          "total": 2,
          "truncated": true,
          "next_before": "2026-11-11T00:00:00Z"
        }
        """
        let result = try JSONDecoder().decode(ToolOutputListResult.self, from: Data(json.utf8))
        XCTAssertTrue(result.entries.isEmpty)
        XCTAssertEqual(result.total, 2)
        XCTAssertTrue(result.truncated)
        XCTAssertEqual(result.nextBefore, "2026-11-11T00:00:00Z")
    }

    func testSessionInstructionsDecodeOrderedSectionsWithoutText() throws {
        let json = """
        {
          "session_id": "ses-1",
          "prompt_layout": 3,
          "latest_prefix_hash": "prefix-123",
          "latest_cache_epoch": "prefix-123",
          "latest_identity_id": "agt-worker-1",
          "latest_identity_generation": 4,
          "latest_identity_hash": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
          "last_rebuilt": 1,
          "states": [
            {
              "state_id": "ses-1",
              "role": "leader",
              "prompt_layout": 3,
              "head_hash": "head-123",
              "tail_hash": "tail-456",
              "identity_hash": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
              "sections": [
                {"key": "core/charter", "hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "ordinal": 10, "tail": false, "chars": 100, "available": true},
                {"key": "session/context", "hash": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "ordinal": 50, "tail": false, "chars": 80, "available": true},
                {"key": "tail/memory", "hash": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc", "ordinal": 90, "tail": true, "chars": 20, "available": false}
              ]
            }
          ]
        }
        """
        let report = try JSONDecoder().decode(SessionInstructionsReport.self, from: Data(json.utf8))
        XCTAssertEqual(report.sessionID, "ses-1")
        XCTAssertEqual(report.promptLayout, 3)
        XCTAssertEqual(report.lastRebuilt, 1)
        XCTAssertEqual(report.latestIdentityID, "agt-worker-1")
        XCTAssertEqual(report.latestIdentityGeneration, 4)
        XCTAssertEqual(report.latestIdentityHash, String(repeating: "d", count: 64))
        let leader = try XCTUnwrap(report.leaderState)
        XCTAssertEqual(leader.role, "leader")
        XCTAssertEqual(leader.identityHash, String(repeating: "d", count: 64))
        XCTAssertEqual(leader.headSections.count, 2)
        XCTAssertEqual(leader.tailSections.count, 1)
        XCTAssertEqual(leader.tailSections[0].key, "tail/memory")
        XCTAssertFalse(leader.tailSections[0].available)
        XCTAssertEqual(leader.headSections[0].shortHash.count, 12)
    }

    func testSessionInstructionsDecodeEmptyPayloadWithLegacyDefaults() throws {
        let report = try JSONDecoder().decode(
            SessionInstructionsReport.self,
            from: Data("{}".utf8)
        )
        XCTAssertEqual(report.promptLayout, 2)
        XCTAssertTrue(report.states.isEmpty)
        XCTAssertNil(report.leaderState)
    }
}
