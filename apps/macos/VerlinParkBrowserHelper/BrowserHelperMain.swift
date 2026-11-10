// VerlinPark browser helper.
//
// A small, daemon-owned WKWebView host used by the `browser` agent tool. It is
// intentionally a separate executable:
//
// - the Rust daemon launches it under a dedicated macOS Seatbelt profile that
//   permits reads, localhost network access, and writes only under the helper
//   scratch directory;
// - it uses a non-persistent WKWebsiteDataStore, so cookies/cache do not become
//   a shared browser profile;
// - top-level navigation is restricted to localhost HTTP(S) and worktree file
//   URLs; downloads and new windows are refused;
// - stdout is a line-oriented JSON protocol, stderr is diagnostic only.
//
// JavaScript execution is available only through the explicit `evaluate`
// action; the daemon approval layer decides whether an agent may call it.

import AppKit
import CryptoKit
import Foundation
import WebKit

private let protocolVersion = "1"
private let maxConsoleEntries = 300
private let maxConsoleEntryBytes = 4096
private let maxSnapshotNodes = 1200
private let maxSnapshotTextChars = 24000
private let maxGenericTextChars = 16000

private func canonicalPath(_ path: String) -> String {
    guard let resolved = realpath(path, nil) else { return path }
    defer { free(resolved) }
    return String(cString: resolved)
}

struct HelperError: Error {
    let code: String
    let message: String
}

extension HelperError {
    static func invalid(_ message: String) -> HelperError {
        HelperError(code: "INVALID_ARGUMENT", message: message)
    }

    static func denied(_ message: String) -> HelperError {
        HelperError(code: "URL_DENIED", message: message)
    }

    static func timeout(_ message: String) -> HelperError {
        HelperError(code: "TIMEOUT", message: message)
    }
}

final class LineWriter {
    private let lock = NSLock()
    private let handle = FileHandle.standardOutput

    func send(_ object: [String: Any]) {
        guard JSONSerialization.isValidJSONObject(object),
              let data = try? JSONSerialization.data(withJSONObject: object, options: []),
              var line = String(data: data, encoding: .utf8)
        else {
            return
        }
        line.append("\n")
        guard let bytes = line.data(using: .utf8) else { return }
        lock.lock()
        defer { lock.unlock() }
        do {
            try handle.write(contentsOf: bytes)
        } catch {
            // stdout is gone; the daemon will observe the closed pipe.
        }
    }
}

final class ConsoleRing {
    struct Entry {
        let level: String
        let text: String
        let at: String
    }

    private var entries: [Entry] = []
    private var dropped = 0

    func append(level: String, text: String) {
        let text = String(text.prefix(maxConsoleEntryBytes))
        entries.append(
            Entry(
                level: String(level.prefix(20)),
                text: text,
                at: ISO8601DateFormatter().string(from: Date())
            )
        )
        while entries.count > maxConsoleEntries {
            entries.removeFirst()
            dropped += 1
        }
    }

    func snapshot(clear: Bool) -> [String: Any] {
        let payload: [String: Any] = [
            "entries": entries.map { ["level": $0.level, "text": $0.text, "at": $0.at] },
            "count": entries.count,
            "dropped": dropped,
        ]
        if clear {
            entries.removeAll()
            dropped = 0
        }
        return payload
    }
}

final class BrowserController: NSObject, WKNavigationDelegate, WKUIDelegate, WKScriptMessageHandler {
    private let writer = LineWriter()
    private let scratch: URL
    private let worktree: URL?
    private let webView: WKWebView
    private let window: NSWindow
    private let console = ConsoleRing()
    private var pendingOpen:
        ((Result<[String: Any], HelperError>) -> Void)?
    private var pendingOpenWorkItem: DispatchWorkItem?
    private var pendingOpenSequence = 0
    private var lastNavigationError: String?

    override init() {
        let arguments = CommandLine.arguments
        func argument(_ name: String) -> String? {
            guard let index = arguments.firstIndex(of: name), index + 1 < arguments.count else {
                return nil
            }
            return arguments[index + 1]
        }
        let scratchPath = argument("--scratch") ?? NSTemporaryDirectory()
        scratch = URL(
            fileURLWithPath: canonicalPath(scratchPath),
            isDirectory: true
        )
        if let worktreePath = argument("--worktree") {
            worktree = URL(
                fileURLWithPath: canonicalPath(worktreePath),
                isDirectory: true
            )
        } else {
            worktree = nil
        }

        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.preferences.isElementFullscreenEnabled = false
        configuration.defaultWebpagePreferences.allowsContentJavaScript = true
        let controller = WKUserContentController()
        configuration.userContentController = controller
        webView = WKWebView(
            frame: CGRect(x: 0, y: 0, width: 1280, height: 900),
            configuration: configuration
        )
        window = NSWindow(
            contentRect: CGRect(x: 0, y: 0, width: 1280, height: 900),
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )
        super.init()
        controller.add(self, name: "vpConsole")
        controller.addUserScript(
            WKUserScript(
                source: Self.injectionScript,
                injectionTime: .atDocumentStart,
                forMainFrameOnly: true
            )
        )
        webView.navigationDelegate = self
        webView.uiDelegate = self
        window.contentView = webView
        window.orderBack(nil)
        try? FileManager.default.createDirectory(
            at: scratch,
            withIntermediateDirectories: true
        )
    }

    func start() {
        writer.send([
            "event": "ready",
            "version": protocolVersion,
            "pid": ProcessInfo.processInfo.processIdentifier,
            "scratch": scratch.path,
        ])
    }

    func protocolOverflow() {
        writer.send([
            "id": "?",
            "ok": false,
            "error": ["code": "PROTOCOL_ERROR", "message": "request line too large"],
        ])
    }

    func handle(_ line: Data) {
        guard let object = try? JSONSerialization.jsonObject(with: line) as? [String: Any] else {
            writer.send([
                "id": "?",
                "ok": false,
                "error": ["code": "PROTOCOL_ERROR", "message": "request is not a JSON object"],
            ])
            return
        }
        let id = object["id"] as? String ?? "?"
        guard let action = object["action"] as? String else {
            respond(id: id, error: HelperError.invalid("action is required"))
            return
        }
        let params = object["params"] as? [String: Any] ?? [:]
        do {
            try dispatch(id: id, action: action, params: params)
        } catch let error as HelperError {
            respond(id: id, error: error)
        } catch {
            respond(id: id, error: HelperError(code: "INTERNAL", message: "\(error)"))
        }
    }

    private func dispatch(id: String, action: String, params: [String: Any]) throws {
        switch action {
        case "open":
            guard let raw = params["url"] as? String else {
                throw HelperError.invalid("url is required")
            }
            try open(id: id, raw: raw)
        case "snapshot":
            let script = "return window.__vpSnapshot(params);"
            let arguments: [String: Any] = [
                "params": [
                    "max_nodes": min(
                        max(params["max_nodes"] as? Int ?? 800, 1),
                        maxSnapshotNodes
                    ),
                    "max_text_chars": min(
                        max(params["max_text_chars"] as? Int ?? 12000, 200),
                        maxSnapshotTextChars
                    ),
                ],
            ]
            evaluateAsync(id: id, script: script, arguments: arguments) { [weak self] result in
                guard let self else { return }
                switch result {
                case let .success(value):
                    var payload = value as? [String: Any] ?? ["value": value]
                    payload["url"] = webView.url?.absoluteString ?? ""
                    if let title = webView.title, !title.isEmpty {
                        payload["title"] = title
                    }
                    respond(id: id, result: payload)
                case let .failure(error):
                    respond(id: id, error: error)
                }
            }
        case "click", "type", "press", "select":
            let script = "return window.__vpAction(params);"
            let arguments: [String: Any] = [
                "params": [
                    "action": action,
                    "ref": params["ref"] as? String ?? "",
                    "selector": params["selector"] as? String ?? "",
                    "text": params["text"] as? String ?? "",
                    "key": params["key"] as? String ?? "",
                    "value": params["value"] as? String ?? "",
                    "clear": params["clear"] as? Bool ?? true,
                ],
            ]
            evaluateAsync(id: id, script: script, arguments: arguments) { [weak self] result in
                switch result {
                case let .success(value):
                    self?.respond(id: id, result: value as? [String: Any] ?? ["value": value])
                case let .failure(error):
                    self?.respond(id: id, error: error)
                }
            }
        case "wait_for":
            guard let condition = params["condition"] as? String,
                  let value = params["value"] as? String
            else {
                throw HelperError.invalid("condition and value are required")
            }
            let timeout = min(max(params["timeout_ms"] as? Int ?? 5000, 50), 60000)
            let script = """
            return await (async () => {
              const deadline = Date.now() + params.timeout_ms;
              while (Date.now() < deadline) {
                if (params.condition === 'selector' && document.querySelector(params.value)) {
                  return { matched: true, condition: params.condition };
                }
                if (params.condition === 'text' && (document.body?.innerText || '').includes(params.value)) {
                  return { matched: true, condition: params.condition };
                }
                if (params.condition === 'url' && location.href.includes(params.value)) {
                  return { matched: true, condition: params.condition, url: location.href };
                }
                await new Promise(resolve => setTimeout(resolve, 100));
              }
              throw new Error('TIMEOUT: ' + params.condition + ' ' + params.value);
            })();
            """
            let arguments: [String: Any] = [
                "params": [
                    "condition": condition,
                    "value": value,
                    "timeout_ms": timeout,
                ],
            ]
            evaluateAsync(id: id, script: script, arguments: arguments) { [weak self] result in
                switch result {
                case let .success(value):
                    self?.respond(id: id, result: value as? [String: Any] ?? ["matched": true])
                case let .failure(error):
                    self?.respond(id: id, error: error)
                }
            }
        case "screenshot":
            takeScreenshot(id: id, fullPage: params["full_page"] as? Bool ?? false)
        case "console":
            respond(
                id: id,
                result: console.snapshot(clear: params["clear"] as? Bool ?? false)
            )
        case "evaluate":
            guard let script = params["script"] as? String, !script.isEmpty else {
                throw HelperError.invalid("script is required")
            }
            if script.utf8.count > 64 * 1024 {
                throw HelperError.invalid("script exceeds 64 KiB")
            }
            evaluateAsync(id: id, script: script, arguments: [:]) { [weak self] result in
                switch result {
                case let .success(value):
                    self?.respond(id: id, result: ["value": Self.jsonSafe(value)])
                case let .failure(error):
                    self?.respond(id: id, error: error)
                }
            }
        case "close":
            respond(id: id, result: ["closed": true])
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) {
                exit(0)
            }
        default:
            throw HelperError.invalid("unknown action: \(action)")
        }
    }

    private func open(id: String, raw: String) throws {
        guard pendingOpen == nil else {
            throw HelperError(code: "BUSY", message: "a navigation is already in progress")
        }
        let url = try validate(url: raw)
        pendingOpenSequence += 1
        let sequence = pendingOpenSequence
        let workItem = DispatchWorkItem { [weak self] in
            guard let self, pendingOpenSequence == sequence else { return }
            finishOpen(
                sequence: sequence,
                result: .failure(HelperError.timeout("navigation did not finish in 30 seconds"))
            )
        }
        pendingOpenWorkItem = workItem
        DispatchQueue.main.asyncAfter(deadline: .now() + 30, execute: workItem)
        let completion: (Result<[String: Any], HelperError>) -> Void = { [weak self] result in
            guard let self else { return }
            pendingOpenSequence += 1
            pendingOpenWorkItem?.cancel()
            pendingOpenWorkItem = nil
            pendingOpen = nil
            switch result {
            case var .success(payload):
                payload["url"] = webView.url?.absoluteString ?? url.absoluteString
                if let title = webView.title, !title.isEmpty {
                    payload["title"] = title
                }
                respond(id: id, result: payload)
            case let .failure(error):
                respond(id: id, error: error)
            }
        }
        pendingOpen = completion
        if url.isFileURL, let worktree {
            webView.loadFileURL(url, allowingReadAccessTo: worktree)
        } else {
            webView.load(URLRequest(url: url))
        }
    }

    private func finishOpen(
        sequence: Int,
        result: Result<[String: Any], HelperError>
    ) {
        guard pendingOpenSequence == sequence, let completion = pendingOpen else { return }
        completion(result)
    }

    private func takeScreenshot(id: String, fullPage: Bool) {
        let configuration = WKSnapshotConfiguration()
        configuration.rect = CGRect(x: 0, y: 0, width: webView.frame.width, height: webView.frame.height)
        configuration.snapshotWidth = NSNumber(value: Int(webView.frame.width))
        webView.takeSnapshot(with: configuration) { [weak self] image, error in
            guard let self else { return }
            if let error {
                respond(
                    id: id,
                    error: HelperError(code: "SCREENSHOT_FAILED", message: "\(error)")
                )
                return
            }
            guard let image,
                  let tiff = image.tiffRepresentation,
                  let bitmap = NSBitmapImageRep(data: tiff),
                  let png = bitmap.representation(using: .png, properties: [:])
            else {
                respond(
                    id: id,
                    error: HelperError(code: "SCREENSHOT_FAILED", message: "no image data")
                )
                return
            }
            let directory = scratch.appendingPathComponent("screenshots", isDirectory: true)
            do {
                try FileManager.default.createDirectory(
                    at: directory,
                    withIntermediateDirectories: true
                )
                let name = "screenshot-\(UUID().uuidString.lowercased()).png"
                let path = directory.appendingPathComponent(name)
                let temporary = directory.appendingPathComponent(".tmp-\(UUID().uuidString.lowercased())")
                try png.write(to: temporary)
                do {
                    try FileManager.default.moveItem(at: temporary, to: path)
                } catch {
                    try? FileManager.default.removeItem(at: temporary)
                    throw error
                }
                let digest = SHA256.hash(data: png).map { String(format: "%02x", $0) }.joined()
                respond(
                    id: id,
                    result: [
                        "path": path.path,
                        "width": bitmap.pixelsWide,
                        "height": bitmap.pixelsHigh,
                        "bytes": png.count,
                        "sha256": digest,
                        "full_page": fullPage,
                    ]
                )
            } catch {
                respond(
                    id: id,
                    error: HelperError(code: "SCREENSHOT_FAILED", message: "\(error)")
                )
            }
        }
    }

    private func evaluateAsync(
        id _: String,
        script: String,
        arguments: [String: Any],
        completion: @escaping (Result<Any, HelperError>) -> Void
    ) {
        webView.callAsyncJavaScript(
            script,
            arguments: arguments,
            in: nil,
            in: .page
        ) { result in
            switch result {
            case let .success(value):
                completion(.success(value))
            case let .failure(error):
                completion(.failure(HelperError(code: "SCRIPT_ERROR", message: "\(error)")))
            }
        }
    }

    private func respond(id: String, result: [String: Any]) {
        let safe = Self.jsonSafe(result)
        writer.send([
            "id": id,
            "ok": true,
            "result": safe as? [String: Any] ?? ["value": safe],
        ])
    }

    private func respond(id: String, error: HelperError) {
        writer.send([
            "id": id,
            "ok": false,
            "error": ["code": error.code, "message": error.message],
        ])
    }

    private static func jsonSafe(_ value: Any) -> Any {
        switch value {
        case let value as [String: Any]:
            value.mapValues { jsonSafe($0) }
        case let value as [Any]:
            value.map { jsonSafe($0) }
        case let value as NSNumber:
            if value.doubleValue.isFinite {
                value
            } else {
                String(describing: value)
            }
        case let value as String:
            String(value.prefix(maxGenericTextChars))
        case is NSNull:
            NSNull()
        default:
            String(describing: value)
        }
    }

    private func validate(url raw: String) throws -> URL {
        let raw = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !raw.isEmpty, let url = URL(string: raw) else {
            throw HelperError.denied("invalid URL")
        }
        if url.absoluteString == "about:blank" {
            return url
        }
        if let scheme = url.scheme?.lowercased(), scheme == "http" || scheme == "https" {
            guard url.user == nil, url.password == nil else {
                throw HelperError.denied("URL credentials are not allowed")
            }
            guard let host = url.host?.lowercased().trimmingCharacters(in: CharacterSet(charactersIn: "[]")),
                  ["localhost", "127.0.0.1", "::1"].contains(host)
            else {
                throw HelperError.denied("only localhost HTTP(S) URLs are allowed")
            }
            return url
        }
        if url.isFileURL {
            guard let worktree else {
                throw HelperError.denied("file URLs are not allowed without a worktree")
            }
            let standardized = URL(fileURLWithPath: canonicalPath(url.path))
            let root = URL(fileURLWithPath: canonicalPath(worktree.path))
            guard standardized.path == root.path || standardized.path.hasPrefix(root.path + "/") else {
                throw HelperError.denied("file URL leaves the worktree")
            }
            if standardized.pathComponents.contains(where: {
                $0.caseInsensitiveCompare(".git") == .orderedSame
            }) {
                throw HelperError.denied("file URL points into Git metadata")
            }
            var isDirectory: ObjCBool = false
            guard FileManager.default.fileExists(atPath: standardized.path, isDirectory: &isDirectory),
                  !isDirectory.boolValue
            else {
                throw HelperError.denied("file URL is not a regular file")
            }
            return URL(fileURLWithPath: standardized.path)
        }
        throw HelperError.denied("unsupported URL scheme")
    }

    // MARK: - WKNavigationDelegate

    func webView(
        _: WKWebView,
        decidePolicyFor navigationAction: WKNavigationAction,
        decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
    ) {
        guard let url = navigationAction.request.url else {
            decisionHandler(.cancel)
            return
        }
        if navigationAction.shouldPerformDownload {
            lastNavigationError = "downloads are blocked"
            console.append(level: "error", text: "blocked a download or attachment navigation")
            decisionHandler(.cancel)
            return
        }
        do {
            _ = try validate(url: url.absoluteString)
            decisionHandler(.allow)
        } catch let error as HelperError {
            lastNavigationError = error.message
            console.append(level: "error", text: "blocked navigation: \(error.message)")
            decisionHandler(.cancel)
        } catch {
            decisionHandler(.cancel)
        }
    }

    func webView(
        _: WKWebView,
        decidePolicyFor navigationResponse: WKNavigationResponse,
        decisionHandler: @escaping (WKNavigationResponsePolicy) -> Void
    ) {
        if !navigationResponse.canShowMIMEType {
            lastNavigationError = "response is not displayable and would download"
            console.append(level: "error", text: "blocked a non-displayable response")
            decisionHandler(.cancel)
            return
        }
        if let response = navigationResponse.response as? HTTPURLResponse,
           let disposition = response.value(forHTTPHeaderField: "Content-Disposition"),
           disposition.lowercased().contains("attachment")
        {
            lastNavigationError = "attachment download blocked"
            console.append(level: "error", text: "blocked an attachment response")
            decisionHandler(.cancel)
            return
        }
        decisionHandler(.allow)
    }

    func webView(_: WKWebView, didFinish _: WKNavigation!) {
        guard pendingOpen != nil else { return }
        finishOpen(
            sequence: pendingOpenSequence,
            result: .success(["loaded": true])
        )
    }

    func webView(
        _: WKWebView,
        didFail _: WKNavigation!,
        withError error: Error
    ) {
        guard pendingOpen != nil else { return }
        finishOpen(
            sequence: pendingOpenSequence,
            result: .failure(
                HelperError(
                    code: "NAVIGATION_FAILED",
                    message: lastNavigationError ?? "\(error)"
                )
            )
        )
    }

    func webView(
        _: WKWebView,
        didFailProvisionalNavigation _: WKNavigation!,
        withError error: Error
    ) {
        guard pendingOpen != nil else { return }
        finishOpen(
            sequence: pendingOpenSequence,
            result: .failure(
                HelperError(
                    code: "NAVIGATION_FAILED",
                    message: lastNavigationError ?? "\(error)"
                )
            )
        )
    }

    func webViewWebContentProcessDidTerminate(_: WKWebView) {
        console.append(level: "error", text: "web content process terminated")
        if pendingOpen != nil {
            finishOpen(
                sequence: pendingOpenSequence,
                result: .failure(
                    HelperError(
                        code: "WEB_CONTENT_TERMINATED",
                        message: "the WebKit content process terminated"
                    )
                )
            )
        }
    }

    // MARK: - WKUIDelegate

    func webView(
        _: WKWebView,
        createWebViewWith _: WKWebViewConfiguration,
        for _: WKNavigationAction,
        windowFeatures _: WKWindowFeatures
    ) -> WKWebView? {
        console.append(level: "warning", text: "blocked a new browser window")
        return nil
    }

    func webView(
        _: WKWebView,
        runJavaScriptAlertPanelWithMessage message: String,
        initiatedByFrame _: WKFrameInfo,
        completionHandler: @escaping () -> Void
    ) {
        console.append(level: "info", text: "alert: \(message)")
        completionHandler()
    }

    func webView(
        _: WKWebView,
        runJavaScriptConfirmPanelWithMessage message: String,
        initiatedByFrame _: WKFrameInfo,
        completionHandler: @escaping (Bool) -> Void
    ) {
        console.append(level: "info", text: "confirm (denied): \(message)")
        completionHandler(false)
    }

    func webView(
        _: WKWebView,
        runJavaScriptTextInputPanelWithPrompt prompt: String,
        defaultText _: String?,
        initiatedByFrame _: WKFrameInfo,
        completionHandler: @escaping (String?) -> Void
    ) {
        console.append(level: "info", text: "prompt (cancelled): \(prompt)")
        completionHandler(nil)
    }

    func webView(
        _: WKWebView,
        runOpenPanelWith _: WKOpenPanelParameters,
        initiatedByFrame _: WKFrameInfo,
        completionHandler: @escaping ([URL]?) -> Void
    ) {
        console.append(level: "warning", text: "blocked a file picker")
        completionHandler(nil)
    }

    // MARK: - WKScriptMessageHandler

    func userContentController(
        _: WKUserContentController,
        didReceive message: WKScriptMessage
    ) {
        guard let payload = message.body as? [String: Any] else { return }
        let level = payload["level"] as? String ?? "log"
        let text = payload["text"] as? String ?? ""
        console.append(level: level, text: text)
    }

    // MARK: - Injected page helpers

    private static let injectionScript = #"""
    (() => {
      if (window.__vpInstalled) return;
      window.__vpInstalled = true;
      window.__vpRefs = new Map();
      window.__vpRefSeq = 0;
      const push = (level, args) => {
        try {
          const text = args.map(value => {
            if (typeof value === 'string') return value;
            try { return JSON.stringify(value); } catch (_) { return String(value); }
          }).join(' ');
          window.webkit.messageHandlers.vpConsole.postMessage({ level, text });
        } catch (_) {}
      };
      ['log', 'info', 'warn', 'error', 'debug'].forEach(level => {
        const original = console[level];
        console[level] = (...args) => {
          push(level, args);
          if (original) original.apply(console, args);
        };
      });
      window.addEventListener('error', event => {
        push('error', [event.message + ' @ ' + (event.filename || '') + ':' + (event.lineno || 0)]);
      });
      window.addEventListener('unhandledrejection', event => {
        push('error', ['unhandled rejection: ' + String(event.reason)]);
      });

      const visible = element => {
        if (!element || !element.isConnected) return false;
        if (element.getAttribute && element.getAttribute('aria-hidden') === 'true') return false;
        if (element.checkVisibility) {
          return element.checkVisibility({ checkOpacity: false, checkVisibilityCSS: true });
        }
        return !!(element.offsetWidth || element.offsetHeight || element.getClientRects().length);
      };

      const roleOf = element => {
        const explicit = element.getAttribute('role');
        if (explicit) return explicit.trim().toLowerCase();
        const tag = element.tagName.toLowerCase();
        if (tag === 'a') return element.hasAttribute('href') ? 'link' : 'generic';
        if (tag === 'button') return 'button';
        if (tag === 'select') return element.multiple ? 'listbox' : 'combobox';
        if (tag === 'textarea') return 'textbox';
        if (tag === 'input') {
          const type = (element.getAttribute('type') || 'text').toLowerCase();
          if (type === 'checkbox') return 'checkbox';
          if (type === 'radio') return 'radio';
          if (type === 'range') return 'slider';
          if (type === 'number') return 'spinbutton';
          if (type === 'search') return 'searchbox';
          if (type === 'submit' || type === 'button' || type === 'reset') return 'button';
          if (type === 'file') return 'button';
          if (type === 'hidden') return 'hidden';
          return 'textbox';
        }
        if (/^h[1-6]$/.test(tag)) return 'heading';
        if (tag === 'img') return 'image';
        if (tag === 'ul' || tag === 'ol') return 'list';
        if (tag === 'li') return 'listitem';
        if (tag === 'table') return 'table';
        if (tag === 'tr') return 'row';
        if (tag === 'td' || tag === 'th') return 'cell';
        if (tag === 'form') return 'form';
        if (tag === 'label') return 'label';
        if (tag === 'nav') return 'navigation';
        if (tag === 'main') return 'main';
        if (tag === 'header') return 'banner';
        if (tag === 'footer') return 'contentinfo';
        if (tag === 'aside') return 'complementary';
        if (tag === 'dialog') return 'dialog';
        if (tag === 'summary') return 'button';
        if (tag === 'p') return 'paragraph';
        return tag === 'body' || tag === 'html' ? 'document' : 'generic';
      };

      const interactiveRoles = new Set([
        'button', 'link', 'textbox', 'searchbox', 'checkbox', 'radio', 'combobox',
        'listbox', 'slider', 'spinbutton', 'switch', 'tab', 'menuitem', 'option',
        'menuitemcheckbox', 'menuitemradio', 'treeitem'
      ]);

      const nameOf = element => {
        const aria = element.getAttribute('aria-label');
        if (aria && aria.trim()) return aria.trim();
        const labelledBy = element.getAttribute('aria-labelledby');
        if (labelledBy) {
          const text = labelledBy.split(/\s+/)
            .map(id => document.getElementById(id)?.innerText || '')
            .join(' ')
            .trim();
          if (text) return text;
        }
        const tag = element.tagName.toLowerCase();
        if (tag === 'img') return (element.getAttribute('alt') || '').trim();
        if (tag === 'input') {
          const type = (element.getAttribute('type') || 'text').toLowerCase();
          if (type === 'submit' || type === 'button' || type === 'reset') {
            return (element.value || type).trim();
          }
          return (element.getAttribute('placeholder') || element.getAttribute('title') || '').trim();
        }
        if (tag === 'textarea') return (element.getAttribute('placeholder') || '').trim();
        if (tag === 'select') return (element.getAttribute('title') || '').trim();
        const title = element.getAttribute('title');
        if (title && title.trim()) return title.trim();
        if (['button', 'a', 'label', 'summary', 'option'].includes(tag)) {
          return (element.innerText || element.textContent || '').trim();
        }
        return '';
      };

      const clean = text => (text || '').replace(/\s+/g, ' ').trim();

      window.__vpSnapshot = function (params) {
        const maxNodes = params.max_nodes || 800;
        const maxTextChars = params.max_text_chars || 12000;
        let count = 0;
        let truncated = false;
        const walk = (element, depth) => {
          if (count >= maxNodes) { truncated = true; return null; }
          if (depth > 40) { truncated = true; return null; }
          const tag = (element.tagName || '').toLowerCase();
          if (['script', 'style', 'noscript', 'template', 'head', 'meta', 'link'].includes(tag)) {
            return null;
          }
          if (tag === 'input' && (element.getAttribute('type') || '').toLowerCase() === 'hidden') {
            return null;
          }
          if (!visible(element)) return null;
          const role = roleOf(element);
          const interactive = interactiveRoles.has(role) || element.hasAttribute('onclick') ||
            element.hasAttribute('tabindex') || tag === 'input' || tag === 'button' ||
            tag === 'select' || tag === 'textarea';
          const name = clean(nameOf(element)).slice(0, 200);
          const children = [];
          for (const child of element.children) {
            const result = walk(child, depth + 1);
            if (result) children.push(result);
          }
          const textTags = new Set(['p', 'li', 'td', 'th', 'label', 'summary', 'option', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6']);
          let text = '';
          if (textTags.has(tag) || (interactive && ['button', 'link', 'paragraph'].includes(role))) {
            text = clean(element.innerText || element.textContent || '').slice(0, 600);
          }
          if (!interactive && !name && !text && children.length === 0) return null;
          let ref = null;
          if (interactive || name) {
            ref = window.__vpRefs.get(element);
            if (!ref || !element.isConnected) {
              window.__vpRefSeq += 1;
              ref = 'e' + window.__vpRefSeq;
              window.__vpRefs.set(element, ref);
            }
          }
          count += 1;
          const node = { ref, role, name, text, tag, depth, children };
          if (interactive) {
            node.disabled = !!element.disabled;
            node.value = element.type === 'password' ? '***' : (element.value ?? null);
            if (element.type === 'checkbox' || element.type === 'radio') {
              node.checked = !!element.checked;
            }
          }
          return node;
        };
        const root = document.body ? walk(document.body, 0) : null;
        const bodyText = clean(document.body?.innerText || '');
        const limitedText = bodyText.slice(0, maxTextChars);
        return {
          url: location.href,
          title: document.title,
          tree: root,
          text: limitedText,
          text_truncated: bodyText.length > limitedText.length,
          node_count: count,
          truncated: truncated || bodyText.length > limitedText.length,
          viewport: { width: window.innerWidth, height: window.innerHeight }
        };
      };

      const resolve = params => {
        if (params.selector) {
          const element = document.querySelector(params.selector);
          if (!element) throw new Error('ELEMENT_NOT_FOUND: ' + params.selector);
          return element;
        }
        if (params.ref) {
          for (const [element, ref] of window.__vpRefs) {
            if (ref === params.ref && element.isConnected) return element;
          }
          throw new Error('ELEMENT_NOT_FOUND: ' + params.ref);
        }
        throw new Error('ref or selector is required');
      };

      const describe = (element, ref) => {
        const name = clean(nameOf(element)).slice(0, 200);
        return {
          ref: ref || (window.__vpRefs.get(element) || null),
          tag: (element.tagName || '').toLowerCase(),
          role: roleOf(element),
          name,
        };
      };

      window.__vpAction = async function (params) {
        const element = resolve(params);
        const info = describe(element, params.ref);
        element.scrollIntoView({ block: 'center', inline: 'center' });
        if (params.action === 'click') {
          element.focus?.();
          const rect = element.getBoundingClientRect();
          const x = rect.left + Math.min(Math.max(rect.width / 2, 0), 10);
          const y = rect.top + Math.min(Math.max(rect.height / 2, 0), 10);
          for (const type of ['pointerdown', 'mousedown', 'pointerup', 'mouseup']) {
            element.dispatchEvent(new MouseEvent(type, {
              bubbles: true, cancelable: true, clientX: x, clientY: y, view: window,
            }));
          }
          element.click();
          await new Promise(resolve => setTimeout(resolve, 60));
          return { action: 'click', ...info, clicked: true };
        }
        if (params.action === 'type') {
          const text = params.text ?? '';
          element.focus?.();
          const tag = element.tagName.toLowerCase();
          const editable = element.isContentEditable;
          if (editable) {
            document.execCommand('selectAll', false, null);
            document.execCommand('insertText', false, text);
          } else if (tag === 'input' || tag === 'textarea') {
            const proto = tag === 'textarea' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
            const setter = Object.getOwnPropertyDescriptor(proto, 'value')?.set;
            if (params.clear !== false || !element.value) {
              setter?.call(element, text);
            } else {
              setter?.call(element, (element.value || '') + text);
            }
            element.dispatchEvent(new Event('input', { bubbles: true }));
            element.dispatchEvent(new Event('change', { bubbles: true }));
          } else {
            throw new Error('ELEMENT_NOT_EDITABLE: ' + tag);
          }
          return { action: 'type', ...info, value: element.type === 'password' ? '***' : (element.value ?? text) };
        }
        if (params.action === 'press') {
          const key = params.key || '';
          element.focus?.();
          const tag = element.tagName.toLowerCase();
          const options = { key, bubbles: true, cancelable: true };
          element.dispatchEvent(new KeyboardEvent('keydown', options));
          element.dispatchEvent(new KeyboardEvent('keypress', options));
          element.dispatchEvent(new KeyboardEvent('keyup', options));
          if (key === 'Enter' && (tag === 'input' || tag === 'textarea' || tag === 'button')) {
            const form = element.form;
            if (form && tag !== 'button') form.requestSubmit();
            else element.click?.();
          }
          if (key === ' ' && (tag === 'button' || element.type === 'checkbox' || element.type === 'radio')) {
            element.click?.();
          }
          return { action: 'press', ...info, key };
        }
        if (params.action === 'select') {
          element.focus?.();
          element.value = params.value ?? '';
          element.dispatchEvent(new Event('input', { bubbles: true }));
          element.dispatchEvent(new Event('change', { bubbles: true }));
          return { action: 'select', ...info, value: element.value ?? null };
        }
        throw new Error('UNKNOWN_ACTION: ' + params.action);
      };
    })();
    """#
}

@main
struct BrowserHelperMain {
    static func main() {
        let sandboxed = ProcessInfo.processInfo.environment["VERLINPARK_BROWSER_HELPER_SANDBOX"]
        guard sandboxed == "seatbelt" else {
            FileHandle.standardError.write(
                Data(
                    "VerlinParkBrowserHelper refuses to run without the daemon's Seatbelt profile\n"
                        .utf8
                )
            )
            exit(64)
        }
        let application = NSApplication.shared
        application.setActivationPolicy(.prohibited)
        let controller = BrowserController()
        controller.start()

        let input = FileHandle.standardInput
        DispatchQueue.global(qos: .userInitiated).async {
            var buffer = Data()
            while true {
                let chunk = input.availableData
                if chunk.isEmpty {
                    exit(0)
                }
                buffer.append(chunk)
                while let newline = buffer.firstIndex(of: 10) {
                    let line = buffer.subdata(in: 0 ..< newline)
                    buffer.removeSubrange(0 ... newline)
                    if line.isEmpty {
                        continue
                    }
                    DispatchQueue.main.async {
                        controller.handle(line)
                    }
                }
                if buffer.count > 8 * 1024 * 1024 {
                    controller.protocolOverflow()
                    buffer.removeAll()
                }
            }
        }
        application.run()
    }
}
