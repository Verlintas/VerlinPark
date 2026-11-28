import SwiftUI

/// The sealed, read-only Custos instrument. It is a normal SwiftUI window:
/// typographic hierarchy, a drawn diamond, compact status rows, and counts.
/// The only character art is the wordmark logotype.
struct CustosWindowView: View {
    @Environment(CustosModel.self) private var model
    @State private var showDiagnostics = false

    var body: some View {
        ZStack {
            CustosBackdrop(state: model.state)

            VStack(spacing: 0) {
                ScrollView(.vertical, showsIndicators: false) {
                    VStack(spacing: 0) {
                        CustosHeaderBar(model: model) {
                            showDiagnostics = true
                        }
                        .padding(.horizontal, 34)
                        .padding(.top, 20)
                        .padding(.bottom, 16)

                        CustosRule()

                        VStack(spacing: 20) {
                            CustosWordmarkView(state: model.state)
                                .padding(.top, 18)

                            CustosHeroView(model: model)

                            CustosDimensionsPanel(rows: model.dimensionRows)

                            CustosCountsPanel(items: model.countItems)

                            CustosRingPanel(line: model.ringLine)
                        }
                        .padding(.horizontal, 34)
                        .padding(.bottom, 20)
                    }
                }

                CustosStatusLine(model: model)
            }
        }
        .frame(minWidth: 860, minHeight: 760)
        .preferredColorScheme(.dark)
        .task { model.start() }
        .sheet(isPresented: $showDiagnostics) {
            CustosDiagnosticsSheet()
                .environment(model)
        }
        .accessibilityElement(children: .contain)
    }
}

// MARK: - Header

private struct CustosHeaderBar: View {
    let model: CustosModel
    let onDiagnostics: () -> Void

    var body: some View {
        HStack(alignment: .center, spacing: 18) {
            VStack(alignment: .leading, spacing: 4) {
                Text("SEALED GUARDIAN")
                    .font(CustosTheme.mono(9, weight: .semibold))
                    .tracking(2.1)
                    .foregroundStyle(CustosTheme.iceDim)
                Text("INTEGRITY INSTRUMENT")
                    .font(CustosTheme.mono(8))
                    .tracking(1.2)
                    .foregroundStyle(CustosTheme.textTertiary)
            }

            Spacer(minLength: 20)

            HStack(spacing: 14) {
                CustosHeaderDatum(label: "TIER", value: model.tierLabel)
                CustosHeaderDatum(label: "MODE", value: model.observeOnlyLabel)
                CustosHeaderDatum(label: "ROLE", value: model.roleLabel)
                Button("DIAGNOSTICS", action: onDiagnostics)
                    .buttonStyle(.plain)
                    .font(CustosTheme.mono(8, weight: .semibold))
                    .tracking(0.8)
                    .foregroundStyle(CustosTheme.iceDim)
                    .padding(.leading, 4)
            }
        }
    }
}

private struct CustosHeaderDatum: View {
    let label: String
    let value: String

    var body: some View {
        VStack(alignment: .trailing, spacing: 4) {
            Text(label)
                .font(CustosTheme.mono(8, weight: .medium))
                .tracking(1.1)
                .foregroundStyle(CustosTheme.textTertiary)
            Text(value)
                .font(CustosTheme.mono(9, weight: .semibold))
                .tracking(0.8)
                .foregroundStyle(CustosTheme.textSecondary)
        }
    }
}

// MARK: - Wordmark

private struct CustosWordmarkView: View {
    let state: CustosState

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    @State private var bootIndex = 0
    @State private var bootComplete = false
    @State private var criticalIndex: Int?

    private struct PhaseKey: Equatable {
        let state: CustosState
        let bootComplete: Bool
    }

    var body: some View {
        TimelineView(.animation(minimumInterval: 1.0 / 30.0, paused: glowPaused)) { context in
            Text(displayRows.joined(separator: "\n"))
                .font(CustosTheme.mono(12))
                .lineSpacing(1)
                .foregroundStyle(CustosTheme.stateColor(state))
                .opacity(glowOpacity(at: context.date))
                .shadow(color: glowShadow, radius: glowRadius)
                .fixedSize(horizontal: true, vertical: true)
                .frame(maxWidth: .infinity, alignment: .center)
        }
        .task { await runBoot() }
        .task(id: PhaseKey(state: state, bootComplete: bootComplete)) { await runJitter() }
        .accessibilityHidden(true)
    }

    private var displayRows: [String] {
        if !bootComplete {
            let index = min(max(bootIndex, 0), CustosASCII.bootFrames.count - 1)
            return CustosASCII.bootFrames[index]
        }
        if let criticalIndex, CustosASCII.criticalFrames.indices.contains(criticalIndex) {
            return CustosASCII.criticalFrames[criticalIndex]
        }
        return CustosASCII.wordmark
    }

    private var glowPaused: Bool {
        reduceMotion
            || !bootComplete
            || criticalIndex != nil
            || CustosTheme.glowPeriod(state) == nil
    }

    private var glowRadius: CGFloat {
        guard !reduceTransparency,
              !reduceMotion,
              bootComplete,
              criticalIndex == nil,
              CustosTheme.glowPeriod(state) != nil
        else {
            return 0
        }
        return 9
    }

    private var glowShadow: Color {
        guard glowRadius > 0 else { return .clear }
        return CustosTheme.stateColor(state).opacity(0.24)
    }

    private func glowOpacity(at date: Date) -> Double {
        if criticalIndex != nil {
            return 1
        }
        return CustosTheme.glowLevel(state, at: date, reduceMotion: reduceMotion)
    }

    private func runBoot() async {
        guard !bootComplete else { return }
        if reduceMotion {
            bootIndex = CustosASCII.bootFrames.count - 1
            bootComplete = true
            return
        }
        for index in CustosASCII.bootFrames.indices {
            if Task.isCancelled {
                break
            }
            bootIndex = index
            try? await Task.sleep(for: .milliseconds(78))
            if Task.isCancelled {
                break
            }
        }
        bootIndex = CustosASCII.bootFrames.count - 1
        bootComplete = true
    }

    private func runJitter() async {
        guard state == .critical, bootComplete, !reduceMotion else {
            criticalIndex = nil
            return
        }
        // One 7-frame cycle at ~12 fps: roughly 580 ms total. No loop.
        for index in CustosASCII.criticalFrames.indices {
            if Task.isCancelled {
                criticalIndex = nil
                return
            }
            criticalIndex = index
            try? await Task.sleep(for: .milliseconds(83))
        }
        criticalIndex = nil
    }
}

// MARK: - Hero

private struct CustosHeroView: View {
    let model: CustosModel
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    private var tint: Color {
        CustosTheme.stateColor(model.state)
    }

    var body: some View {
        VStack(spacing: 14) {
            CustosDiamondView(state: model.state)
                .frame(width: 175, height: 175)

            Text(model.stateWord)
                .font(.system(size: 34, weight: .light))
                .tracking(5)
                .foregroundStyle(tint)
                .shadow(
                    color: reduceTransparency || CustosTheme.glowPeriod(model.state) == nil
                        ? .clear
                        : tint.opacity(0.22),
                    radius: 12
                )
                .accessibilityLabel("Custos state \(model.stateWord)")

            Text(model.stateSentence)
                .font(CustosTheme.body(13))
                .foregroundStyle(CustosTheme.textSecondary)
                .multilineTextAlignment(.center)
                .lineSpacing(3)
                .frame(maxWidth: 580)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity)
    }
}

// MARK: - Dimensions

private struct CustosDimensionsPanel: View {
    let rows: [CustosDimensionRow]

    private let columns = [
        GridItem(.flexible(minimum: 260), spacing: 30),
        GridItem(.flexible(minimum: 260), spacing: 30),
    ]

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            CustosSectionHeader(
                title: "INTEGRITY",
                detail: "NINE OBSERVATIONS"
            )

            LazyVGrid(columns: columns, alignment: .leading, spacing: 0) {
                ForEach(rows) { row in
                    CustosDimensionRowView(row: row)
                }
            }
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 14)
        .background(CustosTheme.panel)
        .overlay {
            Rectangle().strokeBorder(CustosTheme.rule, lineWidth: 1)
        }
    }
}

private struct CustosDimensionRowView: View {
    let row: CustosDimensionRow
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    private var tint: Color {
        CustosTheme.dimensionColor(row.state)
    }

    var body: some View {
        HStack(spacing: 9) {
            Circle()
                .fill(tint)
                .frame(width: 6, height: 6)
                .shadow(
                    color: reduceTransparency || row.state == .unknown ? .clear : tint.opacity(0.45),
                    radius: 4
                )

            Text(row.label)
                .font(CustosTheme.mono(9, weight: .semibold))
                .tracking(0.9)
                .foregroundStyle(CustosTheme.textPrimary)
                .frame(width: 68, alignment: .leading)

            Text(row.state.word)
                .font(CustosTheme.mono(8, weight: .semibold))
                .tracking(0.5)
                .foregroundStyle(tint)
                .frame(width: 66, alignment: .leading)

            Text(row.detail)
                .font(CustosTheme.body(10))
                .foregroundStyle(CustosTheme.textSecondary)
                .lineLimit(1)
                .truncationMode(.tail)
                .frame(maxWidth: .infinity, alignment: .leading)

            Spacer(minLength: 0)
        }
        .padding(.vertical, 8)
        .overlay(alignment: .bottom) {
            Rectangle()
                .fill(CustosTheme.rule)
                .frame(height: 0.5)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(row.label) \(row.state.word). \(row.detail)")
    }
}

// MARK: - Counts

private struct CustosCountsPanel: View {
    let items: [CustosCountItem]

    private let columns = Array(
        repeating: GridItem(.flexible(minimum: 100), spacing: 12, alignment: .topLeading),
        count: 6
    )

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            CustosSectionHeader(
                title: "COUNTS",
                detail: "OBSERVED STATE"
            )

            LazyVGrid(columns: columns, alignment: .leading, spacing: 14) {
                ForEach(items) { item in
                    VStack(alignment: .leading, spacing: 3) {
                        Text(item.value)
                            .font(.system(size: 19, weight: .medium).monospacedDigit())
                            .foregroundStyle(CustosTheme.textPrimary)
                            .lineLimit(1)
                        Text(item.label)
                            .font(CustosTheme.mono(8, weight: .medium))
                            .tracking(0.7)
                            .foregroundStyle(CustosTheme.textTertiary)
                            .lineLimit(1)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 14)
        .background(CustosTheme.panel)
        .overlay {
            Rectangle().strokeBorder(CustosTheme.rule, lineWidth: 1)
        }
    }
}

// MARK: - Ring and status

private struct CustosRingPanel: View {
    let line: String

    var body: some View {
        HStack(spacing: 12) {
            Text("RING")
                .font(CustosTheme.mono(9, weight: .semibold))
                .tracking(1.4)
                .foregroundStyle(CustosTheme.iceDim)
            Rectangle()
                .fill(CustosTheme.ruleStrong)
                .frame(width: 1, height: 11)
            Text(line)
                .font(CustosTheme.mono(9))
                .foregroundStyle(CustosTheme.textSecondary)
                .lineLimit(1)
                .truncationMode(.tail)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 2)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Custos ring \(line)")
    }
}

private struct CustosStatusLine: View {
    let model: CustosModel
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    private var tint: Color {
        CustosTheme.stateColor(model.state)
    }

    var body: some View {
        HStack(spacing: 12) {
            HStack(spacing: 7) {
                Circle()
                    .fill(tint)
                    .frame(width: 5, height: 5)
                    .shadow(
                        color: reduceTransparency || CustosTheme.glowPeriod(model.state) == nil
                            ? .clear
                            : tint.opacity(0.45),
                        radius: 4
                    )
                Text(model.protectionLabel)
                    .font(CustosTheme.mono(9, weight: .semibold))
                    .tracking(1.1)
                    .foregroundStyle(tint)
            }

            Rectangle()
                .fill(CustosTheme.ruleStrong)
                .frame(width: 1, height: 11)

            Text("CHARTER \(model.charterLabel)")
                .font(CustosTheme.mono(8))
                .foregroundStyle(CustosTheme.textTertiary)
                .lineLimit(1)

            Spacer(minLength: 12)

            Text("GENERATED \(model.generatedLabel)")
                .font(CustosTheme.mono(8))
                .foregroundStyle(CustosTheme.textTertiary)
                .lineLimit(1)
        }
        .padding(.horizontal, 34)
        .padding(.vertical, 14)
        .overlay(alignment: .top) {
            CustosRule()
        }
        .background(CustosTheme.obsidian.opacity(0.72))
        .accessibilityElement(children: .combine)
    }
}

// MARK: - Primitives

private struct CustosSectionHeader: View {
    let title: String
    let detail: String

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            Text(title)
                .font(CustosTheme.mono(9, weight: .semibold))
                .tracking(1.7)
                .foregroundStyle(CustosTheme.iceDim)
            Spacer(minLength: 12)
            Text(detail)
                .font(CustosTheme.mono(8))
                .tracking(0.9)
                .foregroundStyle(CustosTheme.textTertiary)
        }
    }
}

private struct CustosRule: View {
    var body: some View {
        Rectangle()
            .fill(CustosTheme.rule)
            .frame(height: 1)
    }
}
