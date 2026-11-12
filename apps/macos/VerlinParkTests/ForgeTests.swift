import Foundation
@testable import VerlinPark
import XCTest

final class ForgeTests: XCTestCase {
    func testPullRequestResultDecodesLegacyWithoutProviderFields() throws {
        let json = """
        {
          "session_id": "ses-1",
          "branch": "feature",
          "base_branch": "main",
          "pushed": true,
          "url": "https://example.test/pr/1",
          "detail": "opened"
        }
        """
        let result = try JSONDecoder().decode(PullRequestResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.sessionId, "ses-1")
        XCTAssertEqual(result.url, "https://example.test/pr/1")
        XCTAssertNil(result.provider)
        XCTAssertNil(result.repository)
        XCTAssertNil(result.change)
    }

    func testPullRequestResultDecodesBitbucketFields() throws {
        let json = """
        {
          "session_id": "ses-1",
          "branch": "feature",
          "base_branch": "main",
          "pushed": true,
          "url": "https://bitbucket.org/acme/widgets/pull-requests/7",
          "detail": "opened",
          "provider": "bitbucket",
          "repository": "acme/widgets",
          "change": "PR"
        }
        """
        let result = try JSONDecoder().decode(PullRequestResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.provider, "bitbucket")
        XCTAssertEqual(result.repository, "acme/widgets")
        XCTAssertEqual(result.change, "PR")
    }

    func testForgeStatusDecodesProviderAndOrigin() throws {
        let json = """
        {
          "provider_override": "bitbucket",
          "default_provider": "bitbucket",
          "origin": {
            "host": "bitbucket.org",
            "provider": "bitbucket",
            "repository": "acme/widgets",
            "supported": true,
            "server_detected": false,
            "detail": "Bitbucket Cloud origin detected"
          },
          "providers": [
            {
              "provider": "bitbucket",
              "change_name": "PR",
              "transport": "rest",
              "command_available": true,
              "auth_configured": true,
              "credential_source": "BITBUCKET_TOKEN",
              "supported": true,
              "detail": "credentials configured"
            }
          ],
          "detail": "selected bitbucket"
        }
        """
        let status = try JSONDecoder().decode(ForgeStatus.self, from: Data(json.utf8))
        XCTAssertEqual(status.providerOverride, "bitbucket")
        XCTAssertEqual(status.defaultProvider, "bitbucket")
        XCTAssertEqual(status.origin?.repository, "acme/widgets")
        XCTAssertEqual(status.origin?.serverDetected, false)
        XCTAssertEqual(status.bitbucket?.authConfigured, true)
        XCTAssertEqual(status.bitbucket?.credentialSource, "BITBUCKET_TOKEN")
    }

    func testForgeStatusDefaultsMissingOptionalFields() throws {
        let json = """
        {
          "default_provider": "github",
          "providers": [
            {"provider": "github", "detail": "gh CLI found"}
          ],
          "detail": "no origin"
        }
        """
        let status = try JSONDecoder().decode(ForgeStatus.self, from: Data(json.utf8))
        XCTAssertNil(status.providerOverride)
        XCTAssertNil(status.origin)
        XCTAssertEqual(status.providers.count, 1)
        XCTAssertEqual(status.providers[0].changeName, "PR")
        XCTAssertEqual(status.providers[0].transport, "cli")
        XCTAssertFalse(status.providers[0].commandAvailable)
        XCTAssertNil(status.providers[0].authConfigured)
    }

    func testForgeOriginReportsServerRefusal() throws {
        let json = """
        {
          "host": "bitbucket.example.com",
          "supported": false,
          "server_detected": true,
          "detail": "Bitbucket Server/Data Center is not supported"
        }
        """
        let origin = try JSONDecoder().decode(ForgeOriginStatus.self, from: Data(json.utf8))
        XCTAssertTrue(origin.serverDetected)
        XCTAssertFalse(origin.supported)
        XCTAssertNil(origin.repository)
    }

    func testForgePreflightResultDecodesBearerModeWithoutSecrets() throws {
        let json = """
        {
          "provider": "bitbucket",
          "change_name": "PR",
          "repository": "acme/widgets",
          "auth": "bearer",
          "detail": "Bitbucket Cloud credentials accepted for acme/widgets (bearer auth)"
        }
        """
        let result = try JSONDecoder().decode(ForgePreflightResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.provider, "bitbucket")
        XCTAssertEqual(result.changeName, "PR")
        XCTAssertEqual(result.repository, "acme/widgets")
        XCTAssertEqual(result.auth, "bearer")
        XCTAssertFalse(result.detail.contains("ATATT"))
    }

    func testForgePreflightResultDefaultsMissingFields() throws {
        let result = try JSONDecoder().decode(
            ForgePreflightResult.self,
            from: Data(#"{"provider": "github"}"#.utf8)
        )
        XCTAssertEqual(result.changeName, "PR")
        XCTAssertNil(result.repository)
        XCTAssertEqual(result.auth, "unknown")
        XCTAssertEqual(result.detail, "")
    }
}
