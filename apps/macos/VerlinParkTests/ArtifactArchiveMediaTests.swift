@testable import VerlinPark
import XCTest

final class ArtifactArchiveMediaTests: XCTestCase {
    // MARK: Archive models

    func testArchiveListDecodesSafeAndUnsafeEntries() throws {
        let json = """
        {
          "artifact_id": "art-1",
          "name": "bundle.zip",
          "mime_type": "application/zip",
          "size_bytes": 2048,
          "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          "format": "zip",
          "entry_count": 3,
          "truncated": true,
          "total_uncompressed_bytes": 4096,
          "total_compressed_bytes": 1024,
          "limits": {
            "max_entries": 2000,
            "max_entry_bytes": 33554432,
            "max_total_bytes": 134217728,
            "max_archive_bytes": 67108864,
            "max_compression_ratio": 100
          },
          "warnings": ["entry_count_truncated"],
          "entries": [
            {"index": 0, "path": "docs/a.txt", "kind": "file", "size_bytes": 10, "compressed_size": 5, "safe": true, "duplicate": false},
            {"index": 1, "path": "../escape.txt", "kind": "file", "size_bytes": 4, "safe": false, "unsafe_reason": "path_traversal", "duplicate": false},
            {"index": 2, "path": "link", "kind": "symlink", "size_bytes": 6, "safe": false, "unsafe_reason": "unsupported_entry_kind:symlink", "duplicate": false}
          ]
        }
        """
        let result = try JSONDecoder().decode(
            ArtifactArchiveListResult.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(result.format, "zip")
        XCTAssertEqual(result.entryCount, 3)
        XCTAssertTrue(result.truncated)
        XCTAssertEqual(result.safeFileCount, 1)
        XCTAssertTrue(result.entries[0].safe)
        XCTAssertEqual(result.entries[0].compressedSize, 5)
        XCTAssertFalse(result.entries[1].safe)
        XCTAssertEqual(result.entries[1].unsafeReason, "path_traversal")
        XCTAssertEqual(result.entries[2].kind, "symlink")
        XCTAssertEqual(result.entries[2].id, "2:link")
        XCTAssertEqual(result.limits.maxEntries, 2000)
    }

    func testArchiveExtractDecodesPublishedArtifactsAndProvenance() throws {
        let json = """
        {
          "artifact_id": "art-1",
          "extraction_id": "extract-1",
          "format": "zip",
          "total_bytes": 4,
          "scratch_retained": false,
          "warnings": ["a warning"],
          "entries": [
            {
              "path": "dir/b.txt",
              "kind": "file",
              "size_bytes": 4,
              "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
              "published_artifact_id": "art-2",
              "preview": "beta",
              "preview_encoding": "utf8",
              "truncated": false
            }
          ],
          "published_artifacts": [
            {
              "id": "art-2",
              "session_id": "ses-1",
              "agent_id": "daemon",
              "agent_name": "Archive",
              "name": "b.txt",
              "kind": "archive-entry",
              "mime_type": "text/plain",
              "content": "",
              "storage": "file",
              "compression": "zstd",
              "size_bytes": 4,
              "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
              "version": 1,
              "created_at": "2026-11-12T00:00:00Z",
              "provenance": {
                "operation": "archive_extract",
                "source_artifact_id": "art-1",
                "source_artifact_name": "bundle.zip",
                "source_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "source_mime_type": "application/zip",
                "entry_path": "dir/b.txt",
                "archive_format": "zip",
                "extraction_id": "extract-1",
                "entry_sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "truncated": false
              }
            }
          ]
        }
        """
        let result = try JSONDecoder().decode(
            ArtifactArchiveExtractResult.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(result.entries.count, 1)
        XCTAssertEqual(result.entries[0].preview, "beta")
        XCTAssertEqual(result.entries[0].previewEncoding, "utf8")
        XCTAssertEqual(result.publishedArtifacts.first?.id, "art-2")
        XCTAssertFalse(result.scratchRetained)
        let provenance = result.publishedArtifacts.first?.provenance
        XCTAssertEqual(provenance?.operation, "archive_extract")
        XCTAssertEqual(provenance?.sourceArtifactId, "art-1")
        XCTAssertEqual(provenance?.entryPath, "dir/b.txt")
        XCTAssertEqual(provenance?.archiveFormat, "zip")
    }

    // MARK: Media models

    func testMediaInfoDecodesFfprobeAndFallbackSources() throws {
        let json = """
        {
          "artifact_id": "art-1",
          "name": "clip.mp4",
          "mime_type": "video/mp4",
          "size_bytes": 4096,
          "sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
          "info": {
            "container": "mov,mp4,m4a",
            "format_long_name": "QuickTime / MOV",
            "duration_secs": 12.5,
            "bit_rate": 4700000,
            "width": 1920,
            "height": 1080,
            "tags": {"encoder": "Lavf"},
            "source": "ffprobe",
            "tool": "ffprobe",
            "warnings": [],
            "streams": [
              {
                "index": 0,
                "codec_type": "video",
                "codec_name": "h264",
                "codec_long_name": "H.264 / AVC",
                "width": 1920,
                "height": 1080,
                "duration_secs": 12.5,
                "bit_rate": 4500000,
                "frames": 300
              },
              {
                "index": 1,
                "codec_type": "audio",
                "codec_name": "aac",
                "sample_rate": 48000,
                "channels": 2,
                "language": "eng"
              }
            ]
          }
        }
        """
        let result = try JSONDecoder().decode(MediaInfoResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.info.source, "ffprobe")
        XCTAssertEqual(result.info.container, "mov,mp4,m4a")
        XCTAssertEqual(result.info.durationSecs, 12.5)
        XCTAssertEqual(result.info.width, 1920)
        XCTAssertEqual(result.info.height, 1080)
        XCTAssertEqual(result.info.tags["encoder"], "Lavf")
        XCTAssertEqual(result.info.streams.count, 2)
        XCTAssertEqual(result.info.streams[0].codecName, "h264")
        XCTAssertEqual(result.info.streams[0].frames, 300)
        XCTAssertEqual(result.info.streams[1].channels, 2)
        XCTAssertEqual(result.info.streams[1].language, "eng")

        let fallback = """
        {
          "artifact_id": "art-2",
          "name": "dot.png",
          "mime_type": "image/png",
          "size_bytes": 100,
          "sha256": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
          "info": {
            "container": "png",
            "width": 1,
            "height": 1,
            "source": "image-metadata",
            "tool": "sips",
            "warnings": ["ffprobe is not installed"],
            "streams": []
          }
        }
        """
        let image = try JSONDecoder().decode(MediaInfoResult.self, from: Data(fallback.utf8))
        XCTAssertEqual(image.info.source, "image-metadata")
        XCTAssertEqual(image.info.width, 1)
        XCTAssertTrue(image.info.streams.isEmpty)
        XCTAssertEqual(image.info.warnings.first, "ffprobe is not installed")
    }

    func testMediaTranscodeDecodesBoundsAndWarnings() throws {
        let json = """
        {
          "source_artifact_id": "art-1",
          "format": "mp4",
          "duration_secs": 5.0,
          "size_bytes": 2048,
          "elapsed_ms": 120,
          "command_summary": "ffmpeg · mp4 · max 30s",
          "max_duration_secs": 30,
          "max_bytes": 5242880,
          "warnings": ["capped"],
          "artifact": {
            "id": "art-2",
            "session_id": "ses-1",
            "agent_id": "daemon",
            "agent_name": "Media",
            "name": "clip-transcoded.mp4",
            "kind": "media-transcode",
            "mime_type": "video/mp4",
            "content": "",
            "storage": "file",
            "compression": "zstd",
            "size_bytes": 2048,
            "sha256": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            "version": 1,
            "created_at": "2026-11-12T00:00:00Z",
            "provenance": {
              "operation": "media_transcode",
              "source_artifact_id": "art-1",
              "source_format": "mov",
              "output_format": "mp4",
              "tool": "ffmpeg",
              "max_duration_secs": 30,
              "truncated": false
            }
          }
        }
        """
        let result = try JSONDecoder().decode(
            MediaTranscodeResult.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(result.format, "mp4")
        XCTAssertEqual(result.maxDurationSecs, 30)
        XCTAssertEqual(result.maxBytes, 5_242_880)
        XCTAssertEqual(result.warnings, ["capped"])
        XCTAssertEqual(result.artifact.provenance?.outputFormat, "mp4")
        XCTAssertEqual(result.artifact.provenance?.sourceFormat, "mov")
    }

    // MARK: Compatibility and typed errors

    func testArtifactWithoutProvenanceStillDecodes() throws {
        let json = """
        {
          "id": "art-0",
          "session_id": "ses-1",
          "agent_id": "agent",
          "agent_name": "Agent",
          "name": "plain.txt",
          "kind": "note",
          "mime_type": "text/plain",
          "content": "hello",
          "created_at": "2026-11-12T00:00:00Z"
        }
        """
        let artifact = try JSONDecoder().decode(Artifact.self, from: Data(json.utf8))
        XCTAssertNil(artifact.provenance)
        XCTAssertEqual(artifact.storage, "inline")
        XCTAssertEqual(artifact.version, 1)

        let versionJSON = """
        {
          "artifact_id": "art-0",
          "version": 1,
          "name": "plain.txt",
          "kind": "note",
          "mime_type": "text/plain",
          "content": "hello",
          "storage": "inline",
          "compression": "none",
          "size_bytes": 5,
          "sha256": "",
          "created_at": "2026-11-12T00:00:00Z"
        }
        """
        let version = try JSONDecoder().decode(
            ArtifactVersion.self,
            from: Data(versionJSON.utf8)
        )
        XCTAssertNil(version.provenance)
    }

    func testRPCErrorPayloadCarriesTypedArchiveFailure() throws {
        let json = """
        {
          "code": -32602,
          "message": "archive entry is not safe to extract: path_traversal",
          "data": {
            "code": "ARCHIVE_UNSAFE_ENTRY",
            "message": "archive entry is not safe to extract: path_traversal",
            "retryable": false,
            "hint": "Select only entries marked safe."
          }
        }
        """
        let payload = try JSONDecoder().decode(RPCErrorPayload.self, from: Data(json.utf8))
        XCTAssertEqual(payload.data?.objectValue?["code"]?.stringValue, "ARCHIVE_UNSAFE_ENTRY")
        XCTAssertEqual(payload.data?.objectValue?["retryable"]?.boolValue, false)
        let error = CoreError.rpc(code: payload.code, message: payload.message, data: payload.data)
        XCTAssertEqual(error.typedCode, "ARCHIVE_UNSAFE_ENTRY")
        XCTAssertEqual(error.typedHint, "Select only entries marked safe.")

        let legacy = try JSONDecoder().decode(
            RPCErrorPayload.self,
            from: Data(#"{"code":-32603,"message":"internal"}"#.utf8)
        )
        XCTAssertNil(legacy.data)
    }
}
