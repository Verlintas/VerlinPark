import SwiftUI

/// Compact read-only external-gate status. The daemon owns gates and their
/// one-shot tokens; this view never receives or renders either the token or
/// the verifier hash.
struct GateBadge: View {
    let report: SessionGatesReport

    private var tint: Color {
        if report.counts.open > 0 {
            return VPTheme.accent
        }
        if report.counts.expired > 0 || report.counts.cancelled > 0 {
            return VPTheme.textTertiary
        }
        return VPTheme.textSecondary
    }

    private var label: String {
        if report.counts.open > 0 {
            return "GATES \(report.counts.open) OPEN"
        }
        if report.counts.total == 0 {
            return "GATES 0"
        }
        return "GATES \(report.counts.resolved) RESOLVED"
    }

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: report.counts.open > 0 ? "hourglass" : "checkmark.seal")
                .font(.system(size: 9, weight: .semibold))
            Text(label)
                .font(.system(size: 9, weight: .semibold, design: .monospaced))
            if report.truncated {
                Text("TRUNCATED")
                    .font(.system(size: 8, weight: .semibold, design: .monospaced))
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
        .help(
            "External gates wait for CI, PR review, deploy, a human, or a custom event. A resolve wakes a continuation; it never approves an approval or merges work."
        )
        .textSelection(.enabled)
    }
}

/// Bounded gate list for Stage/Efficiency. Tokens and verifier hashes are never
/// part of the model, so there is nothing secret to render.
struct GateListSection: View {
    let report: SessionGatesReport

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text("EXTERNAL GATES")
                    .font(.system(size: 9, weight: .semibold, design: .monospaced))
                    .foregroundStyle(VPTheme.textTertiary)
                Text(
                    "\(report.counts.total) total · \(report.counts.open) open · \(report.counts.resolved) resolved · \(report.counts.expired) expired"
                )
                .font(.system(size: 9, design: .monospaced))
                .foregroundStyle(VPTheme.textTertiary)
                GateBadge(report: report)
                Spacer(minLength: 0)
            }
            ForEach(report.gates.prefix(12)) { gate in
                HStack(spacing: 8) {
                    Text(gate.stateLabel)
                        .font(.system(size: 8, weight: .semibold, design: .monospaced))
                        .foregroundStyle(gate.isOpen ? VPTheme.accent : VPTheme.textTertiary)
                        .frame(width: 72, alignment: .leading)
                    Text(gate.kind.uppercased())
                        .font(.system(size: 8, weight: .semibold, design: .monospaced))
                        .foregroundStyle(VPTheme.textTertiary)
                        .frame(width: 64, alignment: .leading)
                    Text(gate.label)
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(VPTheme.textSecondary)
                        .lineLimit(1)
                    if let outcome = gate.outcome {
                        Text(outcome.uppercased())
                            .font(.system(size: 8, weight: .semibold, design: .monospaced))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                    Spacer(minLength: 0)
                }
            }
            if report.gates.count > 12 {
                Text("… \(report.gates.count - 12) more gate(s)")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            Text(
                "Gates are resolved with a one-time scoped token via the CLI or authenticated RPC. They never bypass approvals, sandboxing, verification, merge, capacity, or budgets."
            )
            .font(.system(size: 9))
            .foregroundStyle(VPTheme.textTertiary)
        }
    }
}
