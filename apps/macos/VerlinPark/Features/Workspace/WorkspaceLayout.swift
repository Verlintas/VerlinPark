import CoreGraphics

/// Pure sizing rules for the three-column workspace. Keeping the arithmetic out
/// of `RootView` makes resizing predictable and testable: the stage always
/// keeps a readable minimum, the rail and inspector clamp to sane ranges, and
/// the inspector disappears instead of forcing an unusable stage.
struct WorkspaceLayout: Equatable, Sendable {
    static let railRange: ClosedRange<CGFloat> = 240 ... 320
    static let inspectorRange: ClosedRange<CGFloat> = 320 ... 560
    static let stageMinimum: CGFloat = 520
    static let dividerWidth: CGFloat = 7
    static let defaultRailWidth: CGFloat = 272
    static let defaultInspectorWidth: CGFloat = 382

    var railWidth: CGFloat
    var inspectorWidth: CGFloat
    var inspectorVisible: Bool

    func resolved(windowWidth: CGFloat, focusMode: Bool) -> ResolvedWorkspaceLayout {
        guard !focusMode else {
            return ResolvedWorkspaceLayout(
                railWidth: 0,
                inspectorWidth: 0,
                stageWidth: max(0, windowWidth),
                railVisible: false,
                inspectorVisible: false
            )
        }

        let width = max(0, windowWidth)
        let inspectorPossible = width - Self.dividerWidth * 2
            - Self.railRange.lowerBound
            - Self.stageMinimum >= Self.inspectorRange.lowerBound
        let wantsInspector = inspectorVisible && inspectorPossible
        let dividerCount = wantsInspector ? 2 : 1
        let contentWidth = max(0, width - Self.dividerWidth * CGFloat(dividerCount))

        let maximumRail = max(
            Self.railRange.lowerBound,
            contentWidth
                - Self.stageMinimum
                - (wantsInspector ? Self.inspectorRange.lowerBound : 0)
        )
        let rail = min(max(railWidth, Self.railRange.lowerBound), min(Self.railRange.upperBound, maximumRail))

        guard wantsInspector else {
            return ResolvedWorkspaceLayout(
                railWidth: rail,
                inspectorWidth: 0,
                stageWidth: max(0, contentWidth - rail),
                railVisible: true,
                inspectorVisible: false
            )
        }

        let maximumInspector = max(
            Self.inspectorRange.lowerBound,
            contentWidth - rail - Self.stageMinimum
        )
        let inspector = min(
            max(inspectorWidth, Self.inspectorRange.lowerBound),
            min(Self.inspectorRange.upperBound, maximumInspector)
        )

        return ResolvedWorkspaceLayout(
            railWidth: rail,
            inspectorWidth: inspector,
            stageWidth: max(0, contentWidth - rail - inspector),
            railVisible: true,
            inspectorVisible: true
        )
    }

    static func clampRail(_ value: CGFloat, windowWidth: CGFloat, inspectorVisible: Bool) -> CGFloat {
        WorkspaceLayout(
            railWidth: value,
            inspectorWidth: defaultInspectorWidth,
            inspectorVisible: inspectorVisible
        )
        .resolved(windowWidth: windowWidth, focusMode: false)
        .railWidth
    }

    static func clampInspector(_ value: CGFloat, windowWidth: CGFloat, railWidth: CGFloat) -> CGFloat {
        WorkspaceLayout(
            railWidth: railWidth,
            inspectorWidth: value,
            inspectorVisible: true
        )
        .resolved(windowWidth: windowWidth, focusMode: false)
        .inspectorWidth
    }
}

struct ResolvedWorkspaceLayout: Equatable, Sendable {
    let railWidth: CGFloat
    let inspectorWidth: CGFloat
    let stageWidth: CGFloat
    let railVisible: Bool
    let inspectorVisible: Bool
}
