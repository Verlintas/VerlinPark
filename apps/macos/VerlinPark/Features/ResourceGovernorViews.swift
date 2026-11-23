import SwiftUI

/// Compact Stage read-only view of `session.resources`. It shows the bottleneck,
/// readiness, and the primary resource dimensions; the full dimension list is
/// available on demand. Every value is daemon-provided and R1 never mutates.
struct StageResourceCard: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let resources: ResourceSnapshotView

    @State private var expanded = false

    private var readiness: ReadinessSnapshotView? {
        resources.readiness
    }

    private var attention: ResourceAttentionView {
        resources.attention
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            header
            if expanded {
                Divider().overlay(VPTheme.hairline)
                dimensionRows
                if !resources.warnings.isEmpty {
                    warnings
                }
            } else {
                primaryStrip
            }
            if let error = model.sessionResourcesError {
                Label(error, systemImage: "exclamationmark.triangle.fill")
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.warning)
                    .lineLimit(2)
            }
        }
        .padding(.horizontal, 11)
        .padding(.vertical, 7)
        .background(
            VPTheme.surface.opacity(0.82),
            in: RoundedRectangle(cornerRadius: 11, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 11, style: .continuous)
                .strokeBorder(tint.opacity(resources.needsAttention ? 0.28 : 0.12), lineWidth: 1)
        }
        .vpHoverLift(scale: 1.001, lift: 1)
        .onAppear {
            if resources.needsAttention {
                expanded = true
            }
        }
        .animation(
            VPMotion.resolved(VPMotion.snap, reduceMotion: reduceMotion),
            value: expanded
        )
    }

    private var tint: Color {
        if readiness?.level == "blocked" || resources.dimensions.contains(where: {
            $0.state == "hard_limit"
        }) {
            return VPTheme.danger
        }
        if resources.needsAttention {
            return VPTheme.warning
        }
        return VPTheme.accent
    }

    private var header: some View {
        HStack(spacing: 8) {
            Image(systemName: "gauge.with.dots.needle.50percent")
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(tint)
                .frame(width: 12)
            Text("RESOURCES")
                .font(VPTheme.mono(7, weight: .semibold))
                .tracking(1.0)
                .foregroundStyle(VPTheme.textTertiary)
            Text(resources.profile.uppercased())
                .font(VPTheme.mono(7, weight: .semibold))
                .tracking(0.5)
                .foregroundStyle(tint)
                .padding(.horizontal, 6)
                .padding(.vertical, 2)
                .background(tint.opacity(0.11), in: Capsule())
                .overlay {
                    Capsule().strokeBorder(tint.opacity(0.22), lineWidth: 1)
                }
            if let readiness {
                Text(readiness.levelLabel)
                    .font(VPTheme.mono(7, weight: .semibold))
                    .foregroundStyle(readiness.needsAttention ? VPTheme.warning : VPTheme.info)
            }
            if let bottleneck = resources.bottleneck {
                Text("bottleneck \(bottleneck)")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.warning)
                    .lineLimit(1)
            }
            Spacer(minLength: 6)
            if model.sessionResourcesBusy {
                ProgressView().controlSize(.mini)
            }
            Button {
                expanded.toggle()
            } label: {
                Image(systemName: expanded ? "chevron.up" : "chevron.down")
            }
            .buttonStyle(VPGlassIconButtonStyle(size: 20, tint: VPTheme.textTertiary))
            .help(expanded ? "Collapse resources" : "Expand resources")
            Button {
                Task { await model.loadSessionResources() }
            } label: {
                Image(systemName: "arrow.clockwise")
            }
            .buttonStyle(VPGlassIconButtonStyle(size: 20, tint: VPTheme.textTertiary))
            .help("Refresh resource snapshot")
        }
    }

    private var primaryStrip: some View {
        HStack(spacing: 12) {
            ForEach(primaryDimensions) { dimension in
                compactMetric(dimension)
            }
            Spacer(minLength: 4)
            if attention.interrupts > 0 {
                Text("\(attention.interrupts) interrupts")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
            }
        }
    }

    private var primaryDimensions: [ResourceDimensionView] {
        ["money", "time", "attention", "risk", "context"].compactMap {
            resources.dimension($0)
        }
    }

    private func compactMetric(_ dimension: ResourceDimensionView) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(dimension.label.uppercased())
                .font(VPTheme.mono(6, weight: .semibold))
                .tracking(0.6)
                .foregroundStyle(VPTheme.textTertiary)
            Text(dimensionValue(dimension))
                .font(VPTheme.mono(8, weight: .medium))
                .foregroundStyle(dimension.isAttention ? VPTheme.warning : VPTheme.textSecondary)
                .lineLimit(1)
        }
        .help(dimension.note ?? dimension.source)
    }

    private var dimensionRows: some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(resources.dimensions) { dimension in
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(dimension.label)
                        .font(VPTheme.mono(7.5, weight: .medium))
                        .foregroundStyle(VPTheme.textSecondary)
                        .frame(width: 108, alignment: .leading)
                    Text(dimensionValue(dimension))
                        .font(VPTheme.mono(7.5))
                        .foregroundStyle(dimension.isAttention ? VPTheme.warning : VPTheme.textSecondary)
                    Spacer(minLength: 4)
                    Text(dimension.stateLabel)
                        .font(VPTheme.mono(6.5, weight: .semibold))
                        .foregroundStyle(dimension.isAttention ? VPTheme.warning : VPTheme.textTertiary)
                    Text(dimension.source)
                        .font(VPTheme.mono(6.5))
                        .foregroundStyle(VPTheme.textTertiary.opacity(0.8))
                        .lineLimit(1)
                }
                if let note = dimension.note, !note.isEmpty {
                    Text(note)
                        .font(VPTheme.mono(6.5))
                        .foregroundStyle(VPTheme.textTertiary)
                        .padding(.leading, 116)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            if let readiness {
                readinessLine(readiness)
            }
        }
    }

    private func readinessLine(_ readiness: ReadinessSnapshotView) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(
                "shippable \(readiness.shippable ? "yes" : "no") · mergeable \(readiness.mergeable ? "yes" : "no") · \(readiness.remainingWork.count) remaining"
            )
            .font(VPTheme.mono(7))
            .foregroundStyle(readiness.needsAttention ? VPTheme.warning : VPTheme.textTertiary)
            ForEach(readiness.blockers.prefix(3), id: \.self) { blocker in
                Text("blocked: \(blocker)")
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.danger)
                    .lineLimit(2)
            }
        }
    }

    private var warnings: some View {
        VStack(alignment: .leading, spacing: 2) {
            ForEach(resources.warnings.prefix(4), id: \.self) { warning in
                Text(warning)
                    .font(VPTheme.mono(6.5))
                    .foregroundStyle(VPTheme.textTertiary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private func dimensionValue(_ dimension: ResourceDimensionView) -> String {
        switch dimension.key {
        case "money":
            if resources.pricingSource == "unconfigured" {
                return "prices unset"
            }
            return String(format: "$%.2f", dimension.used)
        case "time":
            return formatSeconds(dimension.used)
        case "attention":
            return "\(Int(dimension.used)) / \(Int(dimension.hardCap ?? 0))"
        case "context":
            if dimension.state == "unavailable" {
                return "n/a"
            }
            return "\(compactNumber(dimension.used)) / \(compactNumber(dimension.hardCap ?? 0))"
        case "risk":
            return "\(Int(dimension.used)) units"
        default:
            return "\(compactNumber(dimension.used)) \(dimension.unit)"
        }
    }

    private func compactNumber(_ value: Double) -> String {
        if value >= 1_000_000 {
            return String(format: "%.1fm", value / 1_000_000)
        }
        if value >= 1000 {
            return String(format: "%.1fk", value / 1000)
        }
        return String(format: "%.0f", value)
    }

    private func formatSeconds(_ value: Double) -> String {
        let seconds = Int(value.rounded())
        if seconds >= 3600 {
            return "\(seconds / 3600)h \((seconds % 3600) / 60)m"
        }
        if seconds >= 60 {
            return "\(seconds / 60)m \(seconds % 60)s"
        }
        return "\(seconds)s"
    }
}
