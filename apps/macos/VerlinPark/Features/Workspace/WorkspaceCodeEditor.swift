import AppKit
import SwiftUI

/// A monospaced worktree editor backed by NSTextView. SwiftUI's TextEditor does
/// not expose scroll position, so a content-search match could not reveal its
/// line. This view keeps the same binding contract while adding a line-number
/// ruler and explicit scroll-to-line support.
struct WorkspaceCodeEditor: NSViewRepresentable {
    @Binding var text: String
    var isEditable: Bool
    var revealLine: Int?
    var revealToken: Int

    func makeCoordinator() -> Coordinator {
        Coordinator(self)
    }

    func makeNSView(context: Context) -> NSScrollView {
        let scrollView = NSScrollView()
        scrollView.drawsBackground = false
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = false
        scrollView.autohidesScrollers = true
        scrollView.borderType = .noBorder
        scrollView.postsBoundsChangedNotifications = true

        let textView = NSTextView()
        textView.isRichText = false
        textView.importsGraphics = false
        textView.allowsUndo = true
        textView.isAutomaticQuoteSubstitutionEnabled = false
        textView.isAutomaticDashSubstitutionEnabled = false
        textView.isAutomaticTextReplacementEnabled = false
        textView.isAutomaticSpellingCorrectionEnabled = false
        textView.isContinuousSpellCheckingEnabled = false
        textView.isGrammarCheckingEnabled = false
        textView.usesFindBar = true
        textView.isIncrementalSearchingEnabled = true
        textView.font = NSFont.monospacedSystemFont(ofSize: 10.5, weight: .regular)
        textView.textColor = NSColor(VPTheme.textSecondary)
        textView.backgroundColor = .clear
        textView.insertionPointColor = NSColor(VPTheme.accent)
        textView.textContainerInset = NSSize(width: 8, height: 8)
        textView.minSize = .zero
        textView.maxSize = NSSize(
            width: CGFloat.greatestFiniteMagnitude,
            height: CGFloat.greatestFiniteMagnitude
        )
        textView.isVerticallyResizable = true
        textView.isHorizontallyResizable = false
        textView.autoresizingMask = [.width]
        textView.textContainer?.widthTracksTextView = true
        textView.textContainer?.containerSize = NSSize(
            width: max(1, scrollView.contentSize.width),
            height: CGFloat.greatestFiniteMagnitude
        )
        textView.selectedTextAttributes = [
            .backgroundColor: NSColor(VPTheme.accent).withAlphaComponent(0.24),
            .foregroundColor: NSColor(VPTheme.textPrimary),
        ]
        textView.setAccessibilityLabel(L10n.t("Workspace file editor"))

        let ruler = WorkspaceLineNumberRuler(textView: textView, scrollView: scrollView)
        scrollView.verticalRulerView = ruler
        scrollView.hasVerticalRuler = true
        scrollView.rulersVisible = true
        scrollView.documentView = textView
        textView.delegate = context.coordinator
        context.coordinator.textView = textView
        context.coordinator.ruler = ruler
        context.coordinator.observeScrolling(scrollView)
        return scrollView
    }

    func updateNSView(_: NSScrollView, context: Context) {
        context.coordinator.parent = self
        guard let textView = context.coordinator.textView else { return }
        if textView.string != text {
            context.coordinator.isApplyingExternalText = true
            let selection = textView.selectedRange()
            textView.string = text
            let bounded = min(selection.location, (text as NSString).length)
            textView.setSelectedRange(NSRange(location: bounded, length: 0))
            context.coordinator.isApplyingExternalText = false
            context.coordinator.ruler?.textChanged()
        }
        textView.isEditable = isEditable
        textView.isSelectable = true
        if !isEditable {
            textView.backgroundColor = NSColor(VPTheme.surfaceSunken).withAlphaComponent(0.35)
        } else {
            textView.backgroundColor = .clear
        }
        if let line = revealLine,
           context.coordinator.lastRevealToken != revealToken
        {
            context.coordinator.lastRevealToken = revealToken
            context.coordinator.reveal(line: line, in: textView)
        }
    }

    final class Coordinator: NSObject, NSTextViewDelegate {
        var parent: WorkspaceCodeEditor
        var textView: NSTextView?
        var ruler: WorkspaceLineNumberRuler?
        var isApplyingExternalText = false
        var lastRevealToken = 0
        private var scrollObserver: NSObjectProtocol?

        init(_ parent: WorkspaceCodeEditor) {
            self.parent = parent
        }

        deinit {
            if let scrollObserver {
                NotificationCenter.default.removeObserver(scrollObserver)
            }
        }

        func observeScrolling(_ scrollView: NSScrollView) {
            scrollView.contentView.postsBoundsChangedNotifications = true
            scrollObserver = NotificationCenter.default.addObserver(
                forName: NSView.boundsDidChangeNotification,
                object: scrollView.contentView,
                queue: .main
            ) { [weak self] _ in
                self?.ruler?.needsDisplay = true
            }
        }

        func textDidChange(_ notification: Notification) {
            guard !isApplyingExternalText,
                  let textView = notification.object as? NSTextView
            else { return }
            parent.text = textView.string
            ruler?.textChanged()
        }

        func reveal(line: Int, in textView: NSTextView) {
            let string = textView.string as NSString
            guard string.length > 0 else { return }
            var currentLine = 1
            var location = 0
            while currentLine < max(1, line), location < string.length {
                let range = string.lineRange(for: NSRange(location: location, length: 0))
                location = NSMaxRange(range)
                currentLine += 1
                if range.length == 0 {
                    break
                }
            }
            let safeLocation = min(location, string.length)
            textView.setSelectedRange(NSRange(location: safeLocation, length: 0))
            textView.scrollRangeToVisible(NSRange(location: safeLocation, length: 0))
            textView.window?.makeFirstResponder(textView)
            ruler?.needsDisplay = true
        }
    }
}

/// A lightweight line-number gutter. It uses the layout manager's visual line
/// fragments so wrapped lines keep correct numbering, and it only draws the
/// numbers inside the current viewport.
final class WorkspaceLineNumberRuler: NSRulerView {
    private weak var textView: NSTextView?
    private var lineStarts: [Int] = [0]

    init(textView: NSTextView, scrollView: NSScrollView) {
        self.textView = textView
        super.init(scrollView: scrollView, orientation: .verticalRuler)
        clientView = textView
        ruleThickness = 36
        refreshLineStarts()
    }

    @available(*, unavailable)
    required init(coder _: NSCoder) {
        fatalError("init(coder:) is not supported")
    }

    func textChanged() {
        refreshLineStarts()
        needsDisplay = true
    }

    override func drawHashMarksAndLabels(in _: NSRect) {
        guard let textView,
              let layoutManager = textView.layoutManager,
              let container = textView.textContainer
        else { return }
        NSColor(VPTheme.surfaceSunken).withAlphaComponent(0.45).setFill()
        NSBezierPath(rect: bounds).fill()

        let visible = textView.visibleRect
        let containerOrigin = textView.textContainerOrigin
        let containerRect = visible.offsetBy(
            dx: -containerOrigin.x,
            dy: -containerOrigin.y
        )
        let glyphRange = layoutManager.glyphRange(
            forBoundingRect: containerRect,
            in: container
        )
        let attributes: [NSAttributedString.Key: Any] = [
            .font: NSFont.monospacedSystemFont(ofSize: 8.5, weight: .regular),
            .foregroundColor: NSColor(VPTheme.textTertiary),
        ]
        layoutManager.enumerateLineFragments(forGlyphRange: glyphRange) {
            rect, _, _, glyphRange, _ in
            let charIndex = layoutManager.characterIndexForGlyph(
                at: glyphRange.location
            )
            guard let line = self.lineNumber(for: charIndex) else { return }
            let label = "\(line)" as NSString
            let size = label.size(withAttributes: attributes)
            let y = rect.minY + containerOrigin.y + (rect.height - size.height) / 2
            label.draw(
                at: NSPoint(x: self.ruleThickness - size.width - 7, y: y),
                withAttributes: attributes
            )
        }
    }

    private func refreshLineStarts() {
        guard let textView else {
            lineStarts = [0]
            return
        }
        let string = textView.string as NSString
        var starts = [0]
        var index = 0
        while index < string.length {
            let range = string.lineRange(for: NSRange(location: index, length: 0))
            let next = NSMaxRange(range)
            if next <= index {
                break
            }
            index = next
            if index < string.length {
                starts.append(index)
            }
        }
        lineStarts = starts
    }

    private func lineNumber(for characterIndex: Int) -> Int? {
        guard !lineStarts.isEmpty else { return nil }
        var low = 0
        var high = lineStarts.count - 1
        while low <= high {
            let middle = (low + high) / 2
            if lineStarts[middle] == characterIndex {
                return middle + 1
            }
            if lineStarts[middle] < characterIndex {
                low = middle + 1
            } else {
                high = middle - 1
            }
        }
        return nil
    }
}
