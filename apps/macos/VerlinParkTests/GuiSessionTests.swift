@testable import VerlinPark
import XCTest

final class GuiSessionTests: XCTestCase {
    func testGuiPolicyDefaultsAndLegacyPayload() throws {
        let legacy = try JSONDecoder().decode(GuiPolicy.self, from: Data("{}".utf8))
        XCTAssertEqual(legacy.graceSecs, 45)
        XCTAssertEqual(legacy.waitMs, 15000)
        XCTAssertEqual(legacy.statusTtlSecs, 30)
        XCTAssertEqual(legacy.sensitiveMode, .deny)
        XCTAssertTrue(legacy.sensitiveBundles.isEmpty)
        XCTAssertTrue(legacy.sensitiveTitles.isEmpty)

        let payload = """
        {
          "grace_secs": 0,
          "wait_ms": 250,
          "status_ttl_secs": 10,
          "sensitive_mode": "require_approval",
          "sensitive_bundles": ["com.example.Vault"],
          "sensitive_titles": ["Vault"]
        }
        """
        let policy = try JSONDecoder().decode(GuiPolicy.self, from: Data(payload.utf8))
        XCTAssertEqual(policy.graceSecs, 0)
        XCTAssertEqual(policy.waitMs, 250)
        XCTAssertEqual(policy.sensitiveMode, .requireApproval)
        XCTAssertEqual(policy.sensitiveBundles, ["com.example.Vault"])
        XCTAssertEqual(policy.sensitiveTitles, ["Vault"])
    }

    func testGuiStatusDecodesHolderQueueAndComputerCache() throws {
        let payload = """
        {
          "runtime_id": "runtime-1",
          "lease_surface": "gui",
          "holder": {
            "lease_id": "lease-1",
            "owner_id": "owner-1",
            "session_id": "ses-1",
            "agent_id": "agent-1",
            "agent_name": "Agent One",
            "surface": "computer",
            "lease_generation": 4,
            "acquired_at": "2026-11-11T00:00:00Z",
            "last_action_at": "2026-11-11T00:00:01Z",
            "expires_at": "2026-11-11T00:00:46Z"
          },
          "queue": [
            {
              "session_id": "ses-2",
              "agent_id": "agent-2",
              "agent_name": "Agent Two",
              "surface": "browser",
              "position": 1,
              "waiting_ms": 1200
            }
          ],
          "policy": {"grace_secs": 45, "wait_ms": 15000, "status_ttl_secs": 30},
          "computer": {
            "platform_supported": true,
            "helper_available": true,
            "permissions_checked": true,
            "cached": true,
            "cache_age_ms": 1200,
            "grace_secs": 45,
            "wait_ms": 15000,
            "snapshot_generations": true,
            "permissions": {"accessibility": true, "screen_recording": false}
          },
          "browser_helper_available": true,
          "browser_sandbox_available": true,
          "warm_computer_helpers": 1,
          "warm_browser_helpers": 2,
          "stale_ref_supported": true
        }
        """
        let status = try JSONDecoder().decode(GuiStatusResult.self, from: Data(payload.utf8))
        XCTAssertEqual(status.leaseSurface, "gui")
        XCTAssertEqual(status.holder?.leaseGeneration, 4)
        XCTAssertEqual(status.holder?.surface, "computer")
        XCTAssertEqual(status.queue.count, 1)
        XCTAssertEqual(status.queue.first?.position, 1)
        XCTAssertEqual(status.computer?.cached, true)
        XCTAssertEqual(status.computer?.accessibility, true)
        XCTAssertEqual(status.computer?.screenRecording, false)
        XCTAssertEqual(status.warmComputerHelpers, 1)
        XCTAssertEqual(status.warmBrowserHelpers, 2)
        XCTAssertTrue(status.staleRefSupported)
    }

    func testGuiStatusToleratesOlderDaemonPayload() throws {
        let payload = """
        {
          "runtime_id": "runtime-1",
          "lease_surface": "computer",
          "helper_available": true,
          "computer": {"platform_supported": true}
        }
        """
        let status = try JSONDecoder().decode(GuiStatusResult.self, from: Data(payload.utf8))
        XCTAssertNil(status.holder)
        XCTAssertTrue(status.queue.isEmpty)
        XCTAssertEqual(status.policy, .standard)
        XCTAssertEqual(status.warmComputerHelpers, 0)
        XCTAssertTrue(status.staleRefSupported)
    }

    func testGuiActionTimelineDecodesAndKeepsNoScreenContent() throws {
        let payload = """
        {
          "actions": [
            {
              "id": "action-1",
              "session_id": "ses-1",
              "agent_id": "agent-1",
              "agent_name": "Agent One",
              "surface": "computer",
              "action": "click",
              "target_app": "Example",
              "target_bundle_id": "com.example.App",
              "sensitive_target": true,
              "snapshot_generation": 7,
              "lease_generation": 4,
              "queued_ms": 12,
              "latency_ms": 18,
              "result": "error",
              "error_code": "STALE_REF",
              "occurred_at": "2026-11-11T00:00:02Z"
            }
          ],
          "limit": 100
        }
        """
        let result = try JSONDecoder().decode(GuiTimelineResult.self, from: Data(payload.utf8))
        XCTAssertEqual(result.actions.count, 1)
        let action = try XCTUnwrap(result.actions.first)
        XCTAssertEqual(action.snapshotGeneration, 7)
        XCTAssertEqual(action.leaseGeneration, 4)
        XCTAssertEqual(action.errorCode, "STALE_REF")
        XCTAssertTrue(action.sensitiveTarget)
        XCTAssertNil(action.targetWindow)
        let encoded = try JSONEncoder().encode(result)
        let text = String(decoding: encoded, as: UTF8.self)
        XCTAssertFalse(text.contains("hunter2"))
        XCTAssertFalse(text.contains("screen content"))
    }

    func testGuiPolicyEncodingUsesSnakeCaseModes() throws {
        let policy = GuiPolicy(
            graceSecs: 12,
            waitMs: 900,
            statusTtlSecs: 11,
            sensitiveMode: .requireApproval,
            sensitiveBundles: ["com.example.Vault"],
            sensitiveTitles: ["Vault"]
        )
        let data = try JSONEncoder().encode(policy)
        let object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: data) as? [String: Any]
        )
        XCTAssertEqual(object["grace_secs"] as? Int, 12)
        XCTAssertEqual(object["sensitive_mode"] as? String, "require_approval")
        XCTAssertEqual(object["sensitive_bundles"] as? [String], ["com.example.Vault"])
    }

    func testUnknownSensitiveModeFailsClosedInsteadOfFailingDecode() throws {
        let payload = """
        {
          "grace_secs": 10,
          "wait_ms": 500,
          "status_ttl_secs": 30,
          "sensitive_mode": "future_escalation_mode"
        }
        """
        let policy = try JSONDecoder().decode(GuiPolicy.self, from: Data(payload.utf8))
        XCTAssertEqual(policy.sensitiveMode, .deny)
        XCTAssertEqual(policy.graceSecs, 10)
    }

    func testGuiStatusIgnoresAnUnknownSensitiveMode() throws {
        let payload = """
        {
          "runtime_id": "runtime-1",
          "lease_surface": "gui",
          "policy": {"sensitive_mode": "future_mode"},
          "browser_helper_available": true
        }
        """
        let status = try JSONDecoder().decode(GuiStatusResult.self, from: Data(payload.utf8))
        XCTAssertEqual(status.policy.sensitiveMode, .deny)
        XCTAssertTrue(status.browserHelperAvailable)
    }

    func testGuiComputerStatusEncodesPermissionSubset() throws {
        let payload = """
        {
          "platform_supported": true,
          "helper_available": true,
          "permissions_checked": true,
          "permissions": {"accessibility": true, "screen_recording": false},
          "snapshot_generations": true
        }
        """
        let status = try JSONDecoder().decode(GuiComputerStatus.self, from: Data(payload.utf8))
        XCTAssertEqual(status.accessibility, true)
        XCTAssertEqual(status.screenRecording, false)
        let data = try JSONEncoder().encode(status)
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        let permissions = object["permissions"] as? [String: Any]
        XCTAssertEqual(permissions?["accessibility"] as? Bool, true)
        XCTAssertEqual(permissions?["screen_recording"] as? Bool, false)
        XCTAssertEqual(object["snapshot_generations"] as? Bool, true)
    }
}
