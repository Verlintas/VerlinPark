import SwiftUI

/// Compact read-only failure root badge for the Stage header. The daemon owns
/// attribution; this view only renders the bounded report and never offers a
/// retry, cancel, merge, or reassignment action.
struct FailureDiagnosisBadge: View {
    let report: SessionDiagnosisReport

    private var tint: Color {
        switch report.earliest?.severity {
        case "root": VPTheme.warning
        case "missing_evidence": VPTheme.textTertiary
        default: VPTheme.accent
        }
    }

    private var label: String {
        guard let finding = report.earliest else { return "NO FAILURE ROOT" }
        return "ROOT · \(finding.reasonCode.uppercased())"
    }

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: report.earliest == nil ? "checkmark.seal" : "exclamationmark.triangle")
                .font(.system(size: 9, weight: .semibold))
            Text(label)
                .font(.system(size: 9, weight: .semibold, design: .monospaced))
            if let finding = report.earliest {
                Text("task \(finding.taskID ?? "-")")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(VPTheme.textTertiary)
            }
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
            "Diagnosis status: \(report.status) · advisory only",
            "Graph: \(report.graphSummary.nodes) nodes / \(report.graphSummary.edges) edges · depth \(report.graphSummary.maxDepth)",
        ]
        if let earliest = report.earliest {
            lines.append("Earliest unsupported claim: \(earliest.reasonCode) at seq \(earliest.atSeq)")
            lines.append(earliest.summary)
            lines.append(contentsOf: earliest.agents.prefix(4).map {
                "Responsible: \($0.displayName) [\($0.role ?? "-")]"
            })
        }
        lines.append(contentsOf: report.warnings.prefix(6))
        return lines.joined(separator: "\n")
    }
}

/// Read-only diagnosis section for the Efficiency inspector. No number here is
/// recomputed by the client and no finding is treated as an instruction.
struct FailureDiagnosisSection: View {
    let report: SessionDiagnosisReport

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text("FAILURE ATTRIBUTION")
                    .font(.system(size: 9, weight: .semibold, design: .monospaced))
                    .foregroundStyle(VPTheme.textTertiary)
                FailureDiagnosisBadge(report: report)
                Spacer(minLength: 0)
            }
            if let earliest = report.earliest {
                Text("\(earliest.reasonCode) · earliest unsupported claim at seq \(earliest.atSeq)")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(VPTheme.textSecondary)
                    .textSelection(.enabled)
                Text(earliest.summary)
                    .font(.system(size: 9))
                    .foregroundStyle(VPTheme.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
                if !earliest.agents.isEmpty {
                    Text(
                        earliest.agents.prefix(4).map {
                            "\($0.displayName) [\($0.role ?? "-")] \($0.responsibility)"
                        }.joined(separator: " · ")
                    )
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(2)
                }
            } else {
                Text(
                    "No failure root was found in the bounded window. The report is advisory and never retries or cancels work."
                )
                .font(.system(size: 9))
                .foregroundStyle(VPTheme.textTertiary)
            }
            if !report.cascade.isEmpty {
                Text(
                    "cascade: " + report.cascade.map {
                        "\($0.taskID ?? "?")[\($0.status ?? "unknown")]"
                    }.joined(separator: " → ")
                )
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(VPTheme.textSecondary)
                .lineLimit(2)
            }
            ForEach(report.findings.prefix(6)) { finding in
                HStack(spacing: 8) {
                    Text(finding.reasonCode.uppercased())
                        .font(.system(size: 8, weight: .semibold, design: .monospaced))
                        .foregroundStyle(finding.severity == "root" ? VPTheme.warning : VPTheme.textTertiary)
                        .frame(width: 180, alignment: .leading)
                    Text("task \(finding.taskID ?? "-") · seq \(finding.atSeq) · \(finding.severity)")
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(VPTheme.textSecondary)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                }
            }
            Text(
                "provenance: \(report.graphSummary.nodes) nodes · \(report.graphSummary.edges) edges · \(report.graphSummary.commits) commits · \(report.graphSummary.verifications) verdicts · depth \(report.graphSummary.maxDepth)\(report.graphSummary.depthLimited ? " (limited)" : "")"
            )
            .font(.system(size: 8, design: .monospaced))
            .foregroundStyle(VPTheme.textTertiary)
            .textSelection(.enabled)
            if report.otherFailures > 0 {
                Text("other failures in window: \(report.otherFailures) · not attributed by this report")
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            if !report.assignments.isEmpty {
                Text(
                    "assignment context: " + report.assignments.prefix(3).map {
                        "\($0.identityID) [\($0.role ?? "-")]"
                    }.joined(separator: " · ")
                )
                .font(.system(size: 8, design: .monospaced))
                .foregroundStyle(VPTheme.textTertiary)
                .lineLimit(1)
            }
            Text(
                "Advisory only. Evidence never retries, cancels, merges, reassigns, or changes budgets, capacity, approvals, or plans."
            )
            .font(.system(size: 8))
            .foregroundStyle(VPTheme.textTertiary)
        }
    }
}
