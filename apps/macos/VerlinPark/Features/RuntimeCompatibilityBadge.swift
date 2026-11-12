import SwiftUI

/// Compact read-only runtime pinning status. The daemon owns the snapshot,
/// compatibility classification, and migration; this view only presents the
/// bounded report. A migration is performed explicitly with
/// `vp plan-migrate <session> --yes`.
struct RuntimeCompatibilityBadge: View {
    let report: CompatibilityReportView

    private var tint: Color {
        switch report.status {
        case "incompatible": VPTheme.warning
        case "drift": VPTheme.accent
        default: VPTheme.textSecondary
        }
    }

    private var label: String {
        switch report.status {
        case "incompatible": "RUNTIME MIGRATION REQUIRED"
        case "drift": "RUNTIME DRIFT"
        default: "RUNTIME PINNED"
        }
    }

    private var icon: String {
        switch report.status {
        case "incompatible": "exclamationmark.triangle"
        case "drift": "arrow.triangle.2.circlepath"
        default: "checkmark.seal"
        }
    }

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: icon)
                .font(.system(size: 9, weight: .semibold))
            Text(label)
                .font(.system(size: 9, weight: .semibold, design: .monospaced))
            Text("rev \(report.planRevision)")
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(VPTheme.textTertiary)
        }
        .foregroundStyle(tint)
        .padding(.horizontal, 7)
        .padding(.vertical, 3)
        .background(tint.opacity(0.10), in: Capsule())
        .overlay {
            Capsule().strokeBorder(tint.opacity(0.32), lineWidth: 1)
        }
        .help(helpText)
        .textSelection(.enabled)
    }

    private var helpText: String {
        var lines = [
            "Runtime snapshot: \(report.current?.runtimeVersion ?? "unknown") / \(report.current?.runtimeBuild ?? "unknown")",
            "Plan revision \(report.planRevision), snapshot age \(report.snapshotAgeSecs)s.",
        ]
        lines.append(contentsOf: report.changed.prefix(8).map {
            "\($0.field): \($0.stored) -> \($0.current) (\($0.severity))"
        })
        lines.append(contentsOf: report.warnings.prefix(8))
        if report.migrationRequired {
            lines.append(
                "Run `vp plan-migrate <session> --yes` to revalidate and re-pin. It never rewrites tasks, scopes, or budgets."
            )
        }
        return lines.joined(separator: "\n")
    }
}

/// Stage inspector rows for a compatibility report. Aggregates are bounded by
/// the daemon; the view never computes a hash or a compatibility decision.
struct RuntimeCompatibilitySection: View {
    let report: CompatibilityReportView

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text("RUNTIME PIN")
                    .font(.system(size: 9, weight: .semibold, design: .monospaced))
                    .foregroundStyle(VPTheme.textTertiary)
                RuntimeCompatibilityBadge(report: report)
                Spacer(minLength: 0)
            }
            if let stored = report.stored {
                Text(
                    "stored: runtime \(stored.runtimeVersion) · policy \(short(stored.policyHash)) · bundle \(short(stored.toolBundleHash)) · layout \(stored.promptLayoutVersion) · contract \(short(stored.contractHash))"
                )
                .font(.system(size: 9, design: .monospaced))
                .foregroundStyle(VPTheme.textSecondary)
                .textSelection(.enabled)
            } else {
                Text("stored: no snapshot (legacy plan/run); resume may proceed with a visible drift warning")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            ForEach(report.changed.prefix(8)) { change in
                HStack(spacing: 8) {
                    Text(change.field.uppercased())
                        .font(.system(size: 8, weight: .semibold, design: .monospaced))
                        .foregroundStyle(VPTheme.textTertiary)
                        .frame(width: 150, alignment: .leading)
                    Text("\(change.stored) → \(change.current)")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(VPTheme.textSecondary)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                }
            }
            if report.migrationRequired {
                Text(
                    "Migration is explicit and approval-classified. It revalidates the stored plan and updates only the snapshot; tasks, scopes, budgets, contracts, receipts, and gates are never rewritten."
                )
                .font(.system(size: 9))
                .foregroundStyle(VPTheme.textTertiary)
            }
        }
    }

    private func short(_ hash: String) -> String {
        hash.isEmpty ? "-" : String(hash.prefix(12))
    }
}
