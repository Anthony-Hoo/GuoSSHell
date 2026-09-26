// M6 D 类：iPad 上系统合成的硬件键盘与指针事件（XCUITest 走 UIKit → Flutter 引擎的真实路径），
// 远端 m6-keyecho 记下收到的每一段字节，经验收服务器的 /m6/input 核对
// （docs/acceptance-m6-2026-09-26.md §3 D）。
//
// 前提：./scripts/sshd-test.sh up；同一台模拟器上先跑过 M6 的集成测试（已信任服务器的主机密钥）。
// 运行：./scripts/m6.sh ui <设备>
import XCTest

final class KeyboardMouseUITests: XCTestCase {
    private let server = URL(string: "http://127.0.0.1:2224")!
    private var app: XCUIApplication!

    override func setUpWithError() throws {
        continueAfterFailure = true
        try XCTSkipUnless(UIDevice.current.userInterfaceIdiom == .pad, "键盘与鼠标用例只在 iPad 上跑")
    }

    override func tearDown() {
        app?.terminate()
        super.tearDown()
    }

    // MARK: - D1–D7 硬件键盘

    func testHardwareKeyboard() throws {
        try launch(modes: "")

        // D1 可打印字符（含 Shift 大小写）与空格。
        try expectBytes("aZ09-=[];',./ ".utf8.map { $0 }) {
            app.typeText("aZ09-=[];',./ ")
        }
        try expectBytes([0x41]) { app.typeKey("a", modifierFlags: .shift) }

        // D2 回车、退格、Tab、Shift+Tab、Esc。
        try expectBytes([0x0d]) { app.typeKey(.`return`, modifierFlags: []) }
        try expectBytes([0x7f]) { app.typeKey(.delete, modifierFlags: []) }
        try expectBytes([0x09]) { app.typeKey(.tab, modifierFlags: []) }
        try expectBytes(esc("[Z")) { app.typeKey(.tab, modifierFlags: .shift) }
        try expectBytes([0x1b]) { app.typeKey(.escape, modifierFlags: []) }

        // D3 方向键与编辑键、F1–F12（远端没开应用光标键：CSI 形式）。
        let navigation: [(XCUIKeyboardKey, String)] = [
            (.upArrow, "[A"), (.downArrow, "[B"), (.rightArrow, "[C"), (.leftArrow, "[D"),
            (.home, "[H"), (.end, "[F"), (.pageUp, "[5~"), (.pageDown, "[6~"), (.forwardDelete, "[3~"),
            (.F1, "OP"), (.F2, "OQ"), (.F3, "OR"), (.F4, "OS"), (.F5, "[15~"), (.F6, "[17~"),
            (.F7, "[18~"), (.F8, "[19~"), (.F9, "[20~"), (.F10, "[21~"), (.F11, "[23~"), (.F12, "[24~"),
        ]
        for (key, sequence) in navigation {
            try expectBytes(esc(sequence), "\(key.rawValue)") { app.typeKey(key, modifierFlags: []) }
        }

        // D4 Ctrl 组合：C0 控制字符。
        let control: [(String, UInt8)] = [("a", 0x01), ("c", 0x03), ("d", 0x04), ("z", 0x1a), ("[", 0x1b), ("\\", 0x1c)]
        for (key, byte) in control {
            try expectBytes([byte], "ctrl+\(key)") { app.typeKey(key, modifierFlags: .control) }
        }

        // D5 Option：ESC 前缀；Option+方向键带修饰参数。
        try expectBytes(esc("b"), "option+b") { app.typeKey("b", modifierFlags: .option) }
        try expectBytes(esc("[1;3D"), "option+left") { app.typeKey(.leftArrow, modifierFlags: .option) }

        // D6 ⌘ 组合由 App 处理，不发到远端。
        try expectBytes([], "cmd+a") { app.typeKey("a", modifierFlags: .command) }
        try expectBytes([], "cmd+c") { app.typeKey("c", modifierFlags: .command) }

        // D7 一次连续输入 50 个键再回车：顺序不乱、不丢不重。
        let burst = "the quick brown fox jumps over the lazy dog 012345"
        try expectBytes(Array(burst.utf8) + [0x0d], "burst") { app.typeText(burst + "\n") }
    }

    // MARK: - D8–D12 鼠标 / 触控板

    func testPointerClicksDragsAndWheel() throws {
        try launch(modes: "mouse drag")
        let center = app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        let lower = app.coordinate(withNormalizedOffset: CGVector(dx: 0.6, dy: 0.62))

        // D8 单击、右键、双击：SGR 按下 / 松开（按钮 0 左、2 右）。
        var events = try mouseEvents { center.click() }
        XCTAssertEqual(events.map(\.kind), ["0M", "0m"], "单击：\(events)")
        events = try mouseEvents { center.rightClick() }
        XCTAssertEqual(events.map(\.kind), ["2M", "2m"], "右键：\(events)")
        events = try mouseEvents { center.doubleClick() }
        XCTAssertEqual(events.map(\.kind), ["0M", "0m", "0M", "0m"], "双击：\(events)")

        // D9 按住拖动：按下、若干拖动（按钮 0 + 32）、松开，列 / 行跟着移动。
        events = try mouseEvents { center.click(forDuration: 0.3, thenDragTo: lower) }
        XCTAssertEqual(events.first?.kind, "0M", "拖动的开始：\(events)")
        XCTAssertEqual(events.last?.kind, "0m", "拖动的结束：\(events)")
        XCTAssertTrue(events.contains { $0.kind == "32M" }, "拖动过程中要有移动上报：\(events)")
        if let first = events.first, let last = events.last {
            XCTAssertTrue(last.col > first.col && last.row > first.row, "拖到右下：\(first) → \(last)")
        }

        // D11 滚轮 / 触控板：全屏程序开了鼠标上报时是滚轮事件（64 上、65 下）。
        events = try mouseEvents { center.scroll(byDeltaX: 0, deltaY: -120) }
        XCTAssertFalse(events.isEmpty, "滚动要上报")
        XCTAssertTrue(events.allSatisfy { $0.kind == "64M" || $0.kind == "65M" }, "滚轮：\(events)")

        // D12 修饰键：Option（8）、Control（16）；Shift+点击走本地选区、不上报。
        events = try mouseEvents { XCUIElement.perform(withKeyModifiers: .option) { center.click() } }
        XCTAssertEqual(events.first?.kind, "8M", "Option+点击：\(events)")
        events = try mouseEvents { XCUIElement.perform(withKeyModifiers: .control) { center.click() } }
        XCTAssertTrue(events.first.map { $0.kind == "16M" || $0.kind == "18M" } ?? false, "Control+点击：\(events)")
        events = try mouseEvents { XCUIElement.perform(withKeyModifiers: .shift) { center.click() } }
        XCTAssertTrue(events.isEmpty, "Shift+点击是本地选区，不发到远端：\(events)")
    }

    func testPointerHover() throws {
        // D10 悬停移动：1003 下上报移动（按钮 3 + 32 = 35），同一格不重复。
        try launch(modes: "motion")
        let events = try mouseEvents {
            for step in 0..<6 {
                app.coordinate(withNormalizedOffset: CGVector(dx: 0.3 + Double(step) * 0.05, dy: 0.5)).hover()
            }
        }
        XCTAssertFalse(events.isEmpty, "悬停要上报")
        XCTAssertTrue(events.allSatisfy { $0.kind == "35M" }, "悬停：\(events)")
        let cells = events.map { "\($0.col),\($0.row)" }
        XCTAssertEqual(cells.count, Set(cells).count, "同一格不重复上报：\(cells)")
    }

    // MARK: - 辅助

    /// 启动 App，自动连上验收服务器并以 exec 模式运行 m6-keyecho；点一下终端拿到焦点，
    /// 再用探测键等回显程序就绪。
    private func launch(modes: String) throws {
        app = XCUIApplication()
        app.launchEnvironment = [
            "GUOSH_HOST": "127.0.0.1",
            "GUOSH_PORT": "2223",
            "GUOSH_USER": "probe",
            "GUOSH_PASS": "probe",
            "GUOSH_CMD": "m6-keyecho \(modes) --seconds 900",
        ]
        app.launch()
        let deadline = Date().addingTimeInterval(60)
        repeat {
            try resetLog()
            app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.4)).tap()
            app.typeKey("~", modifierFlags: [])
            Thread.sleep(forTimeInterval: 1.0)
            if try readLog().contains(0x7e) {
                try resetLog()
                return
            }
        } while Date() < deadline
        XCTFail("m6-keyecho 没有就绪（App 没连上验收服务器？先跑一遍 M6 集成测试信任主机密钥）")
        throw XCTSkip("回显程序未就绪")
    }

    /// 做 [action]，核对远端收到的字节正好是 [expected]。
    private func expectBytes(_ expected: [UInt8], _ label: String = "", file: StaticString = #filePath, line: UInt = #line,
                             _ action: () -> Void) throws {
        try resetLog()
        action()
        let received = try settleLog()
        XCTAssertEqual(hex(received), hex(expected), "\(label) 收到 \(hex(received))", file: file, line: line)
    }

    /// 做 [action]，解出远端收到的 SGR 鼠标事件。
    private func mouseEvents(_ action: () -> Void) throws -> [MouseEvent] {
        try resetLog()
        action()
        let text = String(decoding: try settleLog(), as: UTF8.self)
        let pattern = try NSRegularExpression(pattern: "\u{1b}\\[<(\\d+);(\\d+);(\\d+)([Mm])")
        return pattern.matches(in: text, range: NSRange(text.startIndex..., in: text)).map { match in
            func group(_ index: Int) -> String { String(text[Range(match.range(at: index), in: text)!]) }
            return MouseEvent(kind: group(1) + group(4), col: Int(group(2)) ?? 0, row: Int(group(3)) ?? 0)
        }
    }

    /// 等记录静下来（最后一次写入后 0.6 秒没有新字节）。
    private func settleLog() throws -> [UInt8] {
        var last = try readLog()
        var quiet = 0
        for _ in 0..<40 where quiet < 3 {
            Thread.sleep(forTimeInterval: 0.2)
            let now = try readLog()
            quiet = now == last ? quiet + 1 : 0
            last = now
        }
        return last
    }

    private func resetLog() throws {
        var request = URLRequest(url: server.appendingPathComponent("m6/input/reset"))
        request.httpMethod = "POST"
        _ = try fetch(request)
    }

    /// 回显程序记下的字节（每行一条 JSON：{"t": 毫秒, "hex": "..."}），按顺序拼起来。
    private func readLog() throws -> [UInt8] {
        let data = try fetch(URLRequest(url: server.appendingPathComponent("m6/input")))
        var bytes: [UInt8] = []
        for line in String(decoding: data, as: UTF8.self).split(separator: "\n") {
            guard let json = try JSONSerialization.jsonObject(with: Data(line.utf8)) as? [String: Any],
                  let hexText = json["hex"] as? String else { continue }
            var index = hexText.startIndex
            while index < hexText.endIndex {
                let next = hexText.index(index, offsetBy: 2)
                bytes.append(UInt8(hexText[index..<next], radix: 16) ?? 0)
                index = next
            }
        }
        return bytes
    }

    private func fetch(_ request: URLRequest) throws -> Data {
        let done = DispatchSemaphore(value: 0)
        var result: Result<Data, Error> = .failure(URLError(.timedOut))
        URLSession.shared.dataTask(with: request) { data, _, error in
            result = error.map { .failure($0) } ?? .success(data ?? Data())
            done.signal()
        }.resume()
        _ = done.wait(timeout: .now() + 10)
        return try result.get()
    }

    private func esc(_ sequence: String) -> [UInt8] { [0x1b] + Array(sequence.utf8) }

    private func hex(_ bytes: [UInt8]) -> String { bytes.map { String(format: "%02x", $0) }.joined(separator: " ") }
}

private struct MouseEvent: CustomStringConvertible {
    /// 按钮码加结尾（M 按下 / 移动，m 松开），如 "0M"、"32M"、"0m"。
    let kind: String
    let col: Int
    let row: Int

    var description: String { "\(kind)@\(col),\(row)" }
}

/// M6 B3 / C：真实旋转（系统转动设备）下的全屏 TUI。画面内容由截图复核（附在测试结果里），
/// 断言只管 App 活着、没有弹出异常界面。iPhone 与 iPad 都跑。
final class RotationUITests: XCTestCase {
    func testFullScreenTUIAcrossRotations() throws {
        let app = XCUIApplication()
        app.launchEnvironment = [
            "GUOSH_HOST": "127.0.0.1",
            "GUOSH_PORT": "2223",
            "GUOSH_USER": "probe",
            "GUOSH_PASS": "probe",
            "GUOSH_CMD": "m6-tui btop",
        ]
        XCUIDevice.shared.orientation = .portrait
        app.launch()
        Thread.sleep(forTimeInterval: 10)
        attach("竖屏")
        for orientation in [UIDeviceOrientation.landscapeLeft, .portrait, .landscapeRight, .portrait] {
            XCUIDevice.shared.orientation = orientation
            Thread.sleep(forTimeInterval: 4)
            attach(orientation.isLandscape ? "横屏" : "竖屏")
            XCTAssertEqual(app.state, .runningForeground)
        }
        app.terminate()
    }

    private func attach(_ name: String) {
        let attachment = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
