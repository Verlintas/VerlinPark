// ScreenCaptureKit screenshot support for the VerlinPark computer helper.
//
// A screenshot is a readable host action, but it must produce a bounded PNG in
// the daemon-provided scratch directory. The Rust daemon verifies the path,
// size, and digest before it copies the bytes into the session artifact store.
// The helper never accepts an arbitrary destination path.

import AppKit
import CoreGraphics
import CryptoKit
import Foundation
import ScreenCaptureKit

final class ScreenController {
    private let scratch: URL
    private let maxCaptureSeconds = 30

    init(scratch: URL) {
        self.scratch = scratch
    }

    func capture(_ spec: ScreenshotSpec) throws -> [String: Any] {
        try AccessibilityController.requireScreenRecording()
        let content = try shareableContent()
        let source: (
            width: Int,
            height: Int,
            filter: SCContentFilter,
            windowID: UInt32?,
            displayID: UInt32?
        )
        switch spec.target {
        case .window:
            let window = try selectWindow(content: content, id: spec.windowID)
            source = (
                Int(window.frame.width.rounded()),
                Int(window.frame.height.rounded()),
                SCContentFilter(desktopIndependentWindow: window),
                window.windowID,
                nil
            )
        case .screen:
            let display = try selectDisplay(content: content, spec: spec)
            source = (
                display.width,
                display.height,
                SCContentFilter(display: display, excludingWindows: []),
                nil,
                display.displayID
            )
        }
        guard source.width > 0, source.height > 0 else {
            throw ComputerFailure("SCREENSHOT_FAILED", "capture target has no usable dimensions")
        }
        let size = ScreenshotSpec.fittedSize(
            width: source.width,
            height: source.height,
            maxWidth: spec.maxWidth,
            maxHeight: spec.maxHeight
        )
        let configuration = SCStreamConfiguration()
        configuration.width = size.width
        configuration.height = size.height
        configuration.pixelFormat = kCVPixelFormatType_32BGRA
        configuration.showsCursor = true
        configuration.capturesAudio = false
        configuration.scalesToFit = true
        let image = try captureImage(filter: source.filter, configuration: configuration)
        let encoded = try encodeBoundedPNG(image)
        let output = try ScreenshotOutput.validatedRelativePath(spec.output, scratch: scratch)
        try FileManager.default.createDirectory(
            at: output.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        let temporary = output.deletingLastPathComponent()
            .appendingPathComponent(".tmp-\(UUID().uuidString.lowercased())")
        do {
            try encoded.data.write(to: temporary)
            do {
                try FileManager.default.moveItem(at: temporary, to: output)
            } catch {
                try? FileManager.default.removeItem(at: temporary)
                throw error
            }
        } catch {
            throw ComputerFailure("SCREENSHOT_FAILED", "could not write screenshot: \(error)")
        }
        let digest = SHA256.hash(data: encoded.data).map { String(format: "%02x", $0) }.joined()
        let metadata = ScreenshotMetadata(
            path: output.path,
            width: encoded.width,
            height: encoded.height,
            bytes: encoded.data.count,
            sha256: digest,
            target: spec.target.rawValue,
            windowID: source.windowID,
            displayID: source.displayID
        )
        var result = Self.dictionary(metadata)
        result["source_width"] = source.width
        result["source_height"] = source.height
        return result
    }

    private func shareableContent() throws -> SCShareableContent {
        let semaphore = DispatchSemaphore(value: 0)
        var outcome: Result<SCShareableContent, Error>?
        SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: true) {
            content, error in
            if let error {
                outcome = .failure(error)
            } else if let content {
                outcome = .success(content)
            } else {
                outcome = .failure(
                    ComputerFailure("SCREENSHOT_FAILED", "ScreenCaptureKit returned no content")
                )
            }
            semaphore.signal()
        }
        if semaphore.wait(timeout: .now() + .seconds(maxCaptureSeconds)) == .timedOut {
            throw ComputerFailure.timeout("ScreenCaptureKit did not return shareable content")
        }
        guard let outcome else {
            throw ComputerFailure("SCREENSHOT_FAILED", "ScreenCaptureKit returned no result")
        }
        switch outcome {
        case let .success(content):
            return content
        case let .failure(error):
            throw ComputerFailure("SCREENSHOT_FAILED", "\(error)")
        }
    }

    private func captureImage(
        filter: SCContentFilter,
        configuration: SCStreamConfiguration
    ) throws -> CGImage {
        let semaphore = DispatchSemaphore(value: 0)
        var outcome: Result<CGImage, Error>?
        SCScreenshotManager.captureImage(
            contentFilter: filter,
            configuration: configuration
        ) { image, error in
            if let error {
                outcome = .failure(error)
            } else if let image {
                outcome = .success(image)
            } else {
                outcome = .failure(
                    ComputerFailure("SCREENSHOT_FAILED", "ScreenCaptureKit returned no image")
                )
            }
            semaphore.signal()
        }
        if semaphore.wait(timeout: .now() + .seconds(maxCaptureSeconds)) == .timedOut {
            throw ComputerFailure.timeout("ScreenCaptureKit did not return an image")
        }
        guard let outcome else {
            throw ComputerFailure("SCREENSHOT_FAILED", "ScreenCaptureKit returned no image")
        }
        switch outcome {
        case let .success(image):
            return image
        case let .failure(error):
            throw ComputerFailure("SCREENSHOT_FAILED", "\(error)")
        }
    }

    private func selectDisplay(
        content: SCShareableContent,
        spec: ScreenshotSpec
    ) throws -> SCDisplay {
        if let id = spec.displayID {
            guard let display = content.displays.first(where: { $0.displayID == id }) else {
                throw ComputerFailure(
                    "DISPLAY_NOT_FOUND",
                    "display id \(id) is not shareable; run computer displays for the current list"
                )
            }
            return display
        }
        if let index = spec.displayIndex {
            guard index >= 0, index < content.displays.count else {
                throw ComputerFailure(
                    "DISPLAY_NOT_FOUND",
                    "display index \(index) is out of range; run computer displays"
                )
            }
            return content.displays[index]
        }
        let mainNumber = NSScreen.main?.deviceDescription[
            NSDeviceDescriptionKey("NSScreenNumber")
        ] as? NSNumber
        if let mainID = mainNumber?.uint32Value,
           let display = content.displays.first(where: { $0.displayID == mainID })
        {
            return display
        }
        guard let display = content.displays.first else {
            throw ComputerFailure("SCREENSHOT_FAILED", "no shareable display is available")
        }
        return display
    }

    private func selectWindow(content: SCShareableContent, id: UInt32?) throws -> SCWindow {
        if let id {
            guard let window = content.windows.first(where: { $0.windowID == id }) else {
                throw ComputerFailure("WINDOW_NOT_FOUND", "window id \(id) is not shareable")
            }
            guard window.frame.width > 0, window.frame.height > 0 else {
                throw ComputerFailure("WINDOW_NOT_FOUND", "window id \(id) has no usable frame")
            }
            return window
        }
        let frontmostPid = NSWorkspace.shared.frontmostApplication?.processIdentifier
        if let frontmostPid,
           let info = AccessibilityController.cgWindows(includeOffscreen: false)
               .first(where: { $0.pid == frontmostPid && $0.layer == 0 }),
           let window = content.windows.first(where: { $0.windowID == info.id })
        {
            return window
        }
        guard let window = content.windows.first(where: {
            $0.frame.width > 1 && $0.frame.height > 1
        }) else {
            throw ComputerFailure("WINDOW_NOT_FOUND", "no shareable window is available")
        }
        return window
    }

    private func encodeBoundedPNG(_ image: CGImage) throws -> (data: Data, width: Int, height: Int) {
        var current = image
        var data = try pngData(current)
        var attempts = 0
        while data.count > ComputerProtocol.maxScreenshotBytes,
              current.width > ComputerProtocol.minScreenshotDimension,
              current.height > ComputerProtocol.minScreenshotDimension,
              attempts < 8
        {
            let nextWidth = max(ComputerProtocol.minScreenshotDimension, Int(Double(current.width) * 0.8))
            let nextHeight = max(ComputerProtocol.minScreenshotDimension, Int(Double(current.height) * 0.8))
            guard let resized = Self.resize(current, width: nextWidth, height: nextHeight) else {
                break
            }
            current = resized
            data = try pngData(current)
            attempts += 1
        }
        if data.count > ComputerProtocol.maxScreenshotBytes {
            throw ComputerFailure(
                "SCREENSHOT_TOO_LARGE",
                "screenshot exceeds \(ComputerProtocol.maxScreenshotBytes) bytes"
            )
        }
        return (data, current.width, current.height)
    }

    private func pngData(_ image: CGImage) throws -> Data {
        let representation = NSBitmapImageRep(cgImage: image)
        guard let data = representation.representation(using: .png, properties: [:]) else {
            throw ComputerFailure("SCREENSHOT_FAILED", "could not encode the screenshot as PNG")
        }
        return data
    }

    private static func resize(_ image: CGImage, width: Int, height: Int) -> CGImage? {
        guard width > 0, height > 0,
              let representation = NSBitmapImageRep(
                  bitmapDataPlanes: nil,
                  pixelsWide: width,
                  pixelsHigh: height,
                  bitsPerSample: 8,
                  samplesPerPixel: 4,
                  hasAlpha: true,
                  isPlanar: false,
                  colorSpaceName: .deviceRGB,
                  bytesPerRow: 0,
                  bitsPerPixel: 0
              )
        else {
            return nil
        }
        NSGraphicsContext.saveGraphicsState()
        if let context = NSGraphicsContext(bitmapImageRep: representation) {
            NSGraphicsContext.current = context
            context.imageInterpolation = .high
            context.cgContext.draw(
                image,
                in: CGRect(x: 0, y: 0, width: width, height: height)
            )
        }
        NSGraphicsContext.restoreGraphicsState()
        return representation.cgImage
    }

    private static func dictionary(_ metadata: ScreenshotMetadata) -> [String: Any] {
        var result: [String: Any] = [
            "path": metadata.path,
            "width": metadata.width,
            "height": metadata.height,
            "bytes": metadata.bytes,
            "sha256": metadata.sha256,
            "target": metadata.target,
        ]
        if let windowID = metadata.windowID {
            result["window_id"] = Int(windowID)
        }
        return result
    }
}
