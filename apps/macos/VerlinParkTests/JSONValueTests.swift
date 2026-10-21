@testable import VerlinPark
import XCTest

final class JSONValueTests: XCTestCase {
    func testDecodesEventPayload() throws {
        let event = try JSONDecoder().decode(
            Event.self,
            from: Data(
                """
                {
                  "session_id": "ses_1",
                  "seq": 1,
                  "type": "session.created",
                  "payload": {"goal": "ship it"},
                  "created_at": "2026-10-21T00:00:00Z"
                }
                """.utf8
            )
        )
        XCTAssertEqual(event.type, "session.created")
        XCTAssertEqual(event.seq, 1)
        XCTAssertEqual(event.payload.prettyString().contains("ship it"), true)
    }

    func testRoundTripsJSONValue() throws {
        let value = JSONValue.object([
            "a": .string("b"),
            "n": .number(3),
            "flag": .bool(true),
            "items": .array([.null, .string("x")]),
        ])
        let data = try JSONEncoder().encode(value)
        let decoded = try JSONDecoder().decode(JSONValue.self, from: data)
        XCTAssertEqual(decoded, value)
    }
}
