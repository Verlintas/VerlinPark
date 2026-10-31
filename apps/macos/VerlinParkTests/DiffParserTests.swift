@testable import VerlinPark
import XCTest

final class DiffParserTests: XCTestCase {
    func testParsesFilesAndCountsAdditionsAndDeletions() {
        let patch = """
         src/main.rs | 2 +-
         1 file changed, 1 insertion(+), 1 deletion(-)

        diff --git a/src/main.rs b/src/main.rs
        index 1111111..2222222 100644
        --- a/src/main.rs
        +++ b/src/main.rs
        @@ -1,2 +1,2 @@
        -old
        +new
         same
        diff --git a/README.md b/README.md
        --- a/README.md
        +++ b/README.md
        @@ -1 +1,2 @@
         hello
        +world
        """
        let files = SessionDiffParser.parse(patch)
        XCTAssertEqual(files.count, 2)
        XCTAssertEqual(files[0].path, "src/main.rs")
        XCTAssertEqual(files[0].additions, 1)
        XCTAssertEqual(files[0].deletions, 1)
        XCTAssertTrue(files[0].patch.contains("diff --git a/src/main.rs b/src/main.rs"))
        XCTAssertEqual(files[1].path, "README.md")
        XCTAssertEqual(files[1].additions, 1)
        XCTAssertEqual(files[1].deletions, 0)
    }

    func testDeletedFileFallsBackToTheOldPath() {
        let patch = """
        diff --git a/old.txt b/old.txt
        deleted file mode 100644
        --- a/old.txt
        +++ /dev/null
        @@ -1 +0,0 @@
        -bye
        """
        let files = SessionDiffParser.parse(patch)
        XCTAssertEqual(files.count, 1)
        XCTAssertEqual(files[0].path, "old.txt")
        XCTAssertEqual(files[0].additions, 0)
        XCTAssertEqual(files[0].deletions, 1)
    }

    func testContentLinesStartingWithPlusAreNotTreatedAsHeaders() {
        let patch = """
        diff --git a/note.txt b/note.txt
        --- a/note.txt
        +++ b/note.txt
        @@ -1 +1,2 @@
         first
        ++++ marker
        """
        let files = SessionDiffParser.parse(patch)
        XCTAssertEqual(files.count, 1)
        XCTAssertEqual(files[0].path, "note.txt")
        XCTAssertEqual(files[0].additions, 1)
        XCTAssertEqual(files[0].deletions, 0)
    }

    func testIgnoresStatOnlyPatches() {
        XCTAssertTrue(SessionDiffParser.parse(" src/a.rs | 2 +-\n").isEmpty)
    }
}
