import Foundation
@testable import VerlinPark
import XCTest

final class BackupStatusTests: XCTestCase {
    func testRevokedKeysAndPortableAnchorFieldsDecode() throws {
        let status = try JSONDecoder().decode(
            BackupStatusResult.self,
            from: Data(newPayload.utf8)
        )
        XCTAssertEqual(status.activeKeyID, "new")
        XCTAssertEqual(status.keyCount, 2)
        XCTAssertEqual(status.revokedKeyCount, 1)
        XCTAssertEqual(status.portableAnchorFormatVersion, 1)
        XCTAssertEqual(status.importedRepositories, 1)

        let old = try XCTUnwrap(status.keys.first { $0.keyID == "old" })
        XCTAssertEqual(old.revokedAt, "2026-11-12T02:00:00Z")
        XCTAssertFalse(old.active)

        let current = try XCTUnwrap(status.keys.first { $0.keyID == "new" })
        XCTAssertNil(current.revokedAt)
        XCTAssertTrue(current.active)

        let imported = try XCTUnwrap(status.repositories.first)
        XCTAssertEqual(imported.highestGeneration, 9)
        XCTAssertEqual(imported.lastOperation, "anchor_import")
        XCTAssertEqual(imported.origin, "portable_import")
    }

    func testOlderDaemonPayloadStillDecodesOptionalFields() throws {
        let status = try JSONDecoder().decode(
            BackupStatusResult.self,
            from: Data(legacyPayload.utf8)
        )
        XCTAssertNil(status.revokedKeyCount)
        XCTAssertNil(status.portableAnchorFormatVersion)
        XCTAssertNil(status.importedRepositories)
        XCTAssertNil(status.keys.first?.revokedAt)
        XCTAssertNil(status.repositories.first?.origin)
        XCTAssertEqual(status.keys.first?.keyID, "primary")
    }

    private var newPayload: String {
        """
        {
          "anchor_path": "/tmp/backup-anchor.json",
          "anchor_present": true,
          "anchor_updated_at": "2026-11-12T03:00:00Z",
          "repositories": [{
            "repository_id": "01930000-0000-7000-8000-000000000001",
            "repository_path": "(portable import)",
            "highest_generation": 9,
            "last_verified_generation": 9,
            "last_snapshot_id": null,
            "last_operation": "anchor_import",
            "first_seen_at": "2026-11-12T00:00:00Z",
            "last_verified_at": "2026-11-12T03:00:00Z",
            "origin": "portable_import"
          }],
          "keyring_path": "/tmp/keys/keyring.json",
          "keyring_present": true,
          "active_key_id": "new",
          "key_count": 2,
          "keys": [{
            "key_id": "old",
            "algorithm": "ed25519",
            "public_key_b64": "AAAA",
            "created_at": "2026-11-01T00:00:00Z",
            "retired_at": "2026-11-10T00:00:00Z",
            "revoked_at": "2026-11-12T02:00:00Z",
            "active": false
          }, {
            "key_id": "new",
            "algorithm": "ed25519",
            "public_key_b64": "BBBB",
            "created_at": "2026-11-10T00:00:00Z",
            "retired_at": null,
            "active": true
          }],
          "signed_repository_formats": [3],
          "revoked_key_count": 1,
          "portable_anchor_format_version": 1,
          "imported_repositories": 1,
          "note": "Local anchor, no escrow."
        }
        """
    }

    private var legacyPayload: String {
        """
        {
          "anchor_path": "/tmp/backup-anchor.json",
          "anchor_present": false,
          "anchor_updated_at": null,
          "repositories": [{
            "repository_id": "01930000-0000-7000-8000-000000000002",
            "repository_path": "/backups/repo",
            "highest_generation": 4,
            "last_verified_generation": 4,
            "last_snapshot_id": null,
            "last_operation": "verify",
            "first_seen_at": "2026-11-12T00:00:00Z",
            "last_verified_at": "2026-11-12T00:00:00Z"
          }],
          "keyring_path": "/tmp/keys/keyring.json",
          "keyring_present": true,
          "active_key_id": "primary",
          "key_count": 1,
          "keys": [{
            "key_id": "primary",
            "algorithm": "ed25519",
            "public_key_b64": "AAAA",
            "created_at": "2026-11-01T00:00:00Z",
            "retired_at": null,
            "active": true
          }],
          "signed_repository_formats": [3],
          "note": "Local anchor, no escrow."
        }
        """
    }
}
