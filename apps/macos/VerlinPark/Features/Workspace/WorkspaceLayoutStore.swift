import Foundation
import Observation

enum WorkspaceLayoutPreset: String, CaseIterable, Identifiable, Sendable {
    case balanced
    case reading
    case team

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .balanced: L10n.t("Balanced")
        case .reading: L10n.t("Reading")
        case .team: L10n.t("Team")
        }
    }

    var detail: String {
        switch self {
        case .balanced: L10n.t("Rail, stage, and inspector in equal balance.")
        case .reading: L10n.t("Hide the inspector and give the transcript more room.")
        case .team: L10n.t("Widen the inspector for Collab and Preview.")
        }
    }

    var icon: String {
        switch self {
        case .balanced: "rectangle.split.3x1"
        case .reading: "rectangle.leadinghalf.inset.filled"
        case .team: "rectangle.trailinghalf.inset.filled"
        }
    }

    var railWidth: CGFloat {
        switch self {
        case .balanced: WorkspaceLayout.defaultRailWidth
        case .reading: 248
        case .team: 272
        }
    }

    var inspectorWidth: CGFloat {
        switch self {
        case .balanced: WorkspaceLayout.defaultInspectorWidth
        case .reading: WorkspaceLayout.defaultInspectorWidth
        case .team: 480
        }
    }

    var inspectorVisible: Bool {
        self != .reading
    }
}

@MainActor
@Observable
final class WorkspaceLayoutStore {
    static let railWidthKey = "verlinpark.rail.width"
    static let inspectorWidthKey = "verlinpark.inspector.width"
    static let inspectorVisibleKey = "verlinpark.inspector.visible"

    var railWidth: Double {
        didSet { defaults.set(railWidth, forKey: Self.railWidthKey) }
    }

    var inspectorWidth: Double {
        didSet { defaults.set(inspectorWidth, forKey: Self.inspectorWidthKey) }
    }

    var inspectorVisible: Bool {
        didSet { defaults.set(inspectorVisible, forKey: Self.inspectorVisibleKey) }
    }

    private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        railWidth = Self.readDouble(
            defaults,
            key: Self.railWidthKey,
            fallback: Double(WorkspaceLayout.defaultRailWidth),
            range: WorkspaceLayout.railRange
        )
        inspectorWidth = Self.readDouble(
            defaults,
            key: Self.inspectorWidthKey,
            fallback: Double(WorkspaceLayout.defaultInspectorWidth),
            range: WorkspaceLayout.inspectorRange
        )
        inspectorVisible = defaults.object(forKey: Self.inspectorVisibleKey) as? Bool ?? true
    }

    var layout: WorkspaceLayout {
        WorkspaceLayout(
            railWidth: CGFloat(railWidth),
            inspectorWidth: CGFloat(inspectorWidth),
            inspectorVisible: inspectorVisible
        )
    }

    var currentPreset: WorkspaceLayoutPreset? {
        WorkspaceLayoutPreset.allCases.first { preset in
            abs(preset.railWidth - CGFloat(railWidth)) < 0.5
                && abs(preset.inspectorWidth - CGFloat(inspectorWidth)) < 0.5
                && preset.inspectorVisible == inspectorVisible
        }
    }

    func apply(_ preset: WorkspaceLayoutPreset) {
        railWidth = Double(preset.railWidth)
        inspectorWidth = Double(preset.inspectorWidth)
        inspectorVisible = preset.inspectorVisible
    }

    func reset() {
        apply(.balanced)
    }

    private static func readDouble(
        _ defaults: UserDefaults,
        key: String,
        fallback: Double,
        range: ClosedRange<CGFloat>
    ) -> Double {
        guard defaults.object(forKey: key) != nil else { return fallback }
        let value = defaults.double(forKey: key)
        return Double(min(max(CGFloat(value), range.lowerBound), range.upperBound))
    }
}
