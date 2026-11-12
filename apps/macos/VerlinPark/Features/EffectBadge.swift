import SwiftUI

/// Compact read-only effect-receipt status. The daemon owns receipts and
/// reconciliation; this view never mutates state and never renders file
/// content or command text.
struct EffectBadge: View {
    let report: SessionEffectsReport

    private var tint: Color {
        if report.blocked {
            return VPTheme.warning
        }
        if report.counts.unknown > 0 || report.counts.intent > 0 {
            return VPTheme.warning
        }
        if report.counts.done > 0, report.counts.failed == 0 {
            return VPTheme.accent
        }
        return VPTheme.textTertiary
    }

    private var label: String {
        if report.blocked {
            return "EFFECTS \(report.counts.blocking) BLOCKING"
        }
        if report.counts.total == 0 {
            return "EFFECTS 0"
        }
        return "EFFECTS \(report.counts.done) DONE · \(report.counts.unknown) UNKNOWN"
    }

    var body: some View {
        HStack(spacing: 6) {
            Image(
                systemName: report.blocked
                    ? "exclamationmark.octagon.fill"
                    : "checkmark.seal"
            )
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
            report.blockReason
                ?? "Local effect receipts: done means the runtime observed the postcondition; unknown blocks auto-retry and merge until reconciled."
        )
        .textSelection(.enabled)
    }
}

/// Bounded list of receipts for Stage/Efficiency. Shows target labels, state,
/// attempt, and a short evidence anchor only.
struct EffectReceiptsSection: View {
    let report: SessionEffectsReport

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text("EFFECT RECEIPTS")
                    .font(.system(size: 9, weight: .semibold, design: .monospaced))
                    .foregroundStyle(VPTheme.textTertiary)
                Text(
                    "\(report.counts.total) total · \(report.counts.done) done · \(report.counts.unknown) unknown · \(report.counts.failed) failed"
                )
                .font(.system(size: 9, design: .monospaced))
                .foregroundStyle(VPTheme.textTertiary)
                EffectBadge(report: report)
                Spacer(minLength: 0)
            }
            if let reason = report.blockReason {
                Text(reason)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(VPTheme.warning)
                    .textSelection(.enabled)
            }
            ForEach(report.receipts.prefix(12)) { receipt in
                HStack(spacing: 8) {
                    Text(receipt.stateLabel)
                        .font(.system(size: 8, weight: .semibold, design: .monospaced))
                        .foregroundStyle(
                            receipt.blocksMerge ? VPTheme.warning : VPTheme.textTertiary
                        )
                        .frame(width: 58, alignment: .leading)
                    Text(receipt.target.label)
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(VPTheme.textSecondary)
                        .lineLimit(1)
                    Text("attempt \(receipt.attempt)")
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(VPTheme.textTertiary)
                    if let anchor = receipt.evidence.anchor {
                        Text(anchor)
                            .font(.system(size: 9, design: .monospaced))
                            .foregroundStyle(VPTheme.textTertiary)
                            .textSelection(.enabled)
                    }
                    Spacer(minLength: 0)
                }
            }
            if report.receipts.count > 12 {
                Text("… \(report.receipts.count - 12) more receipt(s)")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            if let caveat = report.receipts.first(where: { $0.evidence.caveat != nil })?
                .evidence.caveat
            {
                Text(caveat)
                    .font(.system(size: 8, design: .monospaced))
                    .foregroundStyle(VPTheme.textTertiary)
            }
        }
        .padding(.vertical, 2)
    }
}
