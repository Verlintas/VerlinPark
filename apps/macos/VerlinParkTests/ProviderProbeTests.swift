import Foundation
@testable import VerlinPark
import XCTest

final class ProviderProbeTests: XCTestCase {
    func testToolProbeDecodesWireFields() throws {
        let json = """
        {
          "provider": "deepseek",
          "model": "deepseek-flash",
          "latency_ms": 850,
          "done": true,
          "input_tokens": 42,
          "output_tokens": 7,
          "tool_calling": true,
          "tool_name": "probe_echo",
          "arguments_preview": "ok",
          "text": "",
          "note": "provider emitted the expected tool call"
        }
        """
        let result = try JSONDecoder().decode(
            AgentToolProbeResult.self,
            from: Data(json.utf8)
        )
        XCTAssertTrue(result.toolCalling)
        XCTAssertEqual(result.toolName, "probe_echo")
        XCTAssertEqual(result.argumentsPreview, "ok")
        XCTAssertEqual(result.inputTokens, 42)
        XCTAssertEqual(result.outputTokens, 7)
    }

    func testTextOnlyProbeIsNotLabeledToolCalling() throws {
        let json = """
        {
          "provider": "local",
          "model": "small",
          "latency_ms": 12,
          "done": true,
          "input_tokens": 1,
          "output_tokens": 2,
          "tool_calling": false,
          "tool_name": null,
          "arguments_preview": null,
          "text": "OK",
          "note": "provider returned text but no tool call"
        }
        """
        let result = try JSONDecoder().decode(
            AgentToolProbeResult.self,
            from: Data(json.utf8)
        )
        XCTAssertFalse(result.toolCalling)
        XCTAssertNil(result.toolName)
        XCTAssertNil(result.argumentsPreview)
    }

    func testHardenedShellPolicyIsExplicitlyRestrictive() {
        let policy = ShellPolicy.hardened
        XCTAssertEqual(policy.network, "deny")
        XCTAssertTrue(policy.restrictHome)
        XCTAssertTrue(policy.readJail)
        XCTAssertTrue(policy.readDeny.isEmpty)
    }
}
