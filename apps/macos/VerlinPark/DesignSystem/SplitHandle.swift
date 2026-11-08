import AppKit
import SwiftUI

/// A narrow draggable divider for the workspace columns. It owns no layout
/// policy: callers pass a range and the binding is re-clamped by
/// `WorkspaceLayout` on the next render.
struct VPSplitHandle: View {
    enum Direction {
        case rightIncreases
        case leftIncreases

        var multiplier: CGFloat {
            switch self {
            case .rightIncreases: 1
            case .leftIncreases: -1
            }
        }
    }

    @Binding var value: CGFloat
    let range: ClosedRange<CGFloat>
    var direction: Direction = .rightIncreases
    var onReset: () -> Void = {}

    @State private var hovering = false
    @State private var dragStart: CGFloat?

    var body: some View {
        ZStack {
            Rectangle()
                .fill(Color.clear)
            Rectangle()
                .fill(hovering || dragStart != nil ? VPTheme.accent.opacity(0.45) : VPTheme.hairline)
                .frame(width: 1)
        }
        .frame(width: WorkspaceLayout.dividerWidth)
        .frame(maxHeight: .infinity)
        .contentShape(Rectangle())
        .onHover { inside in
            hovering = inside
            if inside {
                NSCursor.resizeLeftRight.set()
            } else {
                NSCursor.arrow.set()
            }
        }
        .gesture(
            DragGesture(minimumDistance: 1, coordinateSpace: .global)
                .onChanged { gesture in
                    let start = dragStart ?? value
                    if dragStart == nil {
                        dragStart = start
                    }
                    let proposed = start + gesture.translation.width * direction.multiplier
                    value = min(max(proposed, range.lowerBound), range.upperBound)
                }
                .onEnded { _ in
                    dragStart = nil
                    NSCursor.arrow.set()
                }
        )
        .onTapGesture(count: 2) {
            onReset()
        }
        .accessibilityElement()
        .accessibilityLabel(L10n.t("Resize panel"))
        .accessibilityValue("\(Int(value))")
        .accessibilityAdjustableAction { adjustment in
            switch adjustment {
            case .increment:
                value = min(range.upperBound, value + 12)
            case .decrement:
                value = max(range.lowerBound, value - 12)
            @unknown default:
                break
            }
        }
        .help(L10n.t("Drag to resize · double-click to reset"))
    }
}
