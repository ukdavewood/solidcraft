// Apple's on-device language model (FoundationModels, iOS 26) with SolidCraft's commands as its tools.
// Loaded by the Rust app at run time (dlopen) and reached through the C functions at the bottom.
import Foundation
import FoundationModels
import Speech
import AVFoundation
import Security
import UIKit

public typealias SCExecFn = @convention(c) (UnsafePointer<CChar>) -> UnsafeMutablePointer<CChar>?
public typealias SCDoneFn = @convention(c) (UnsafePointer<CChar>) -> Void

nonisolated(unsafe) private var execFn: SCExecFn?
nonisolated(unsafe) private var session: LanguageModelSession?

/// Ask the app to run something (on its UI thread) and wait for the text answer.
func exec(_ request: [String: Any]) -> String {
    guard let f = execFn,
          let data = try? JSONSerialization.data(withJSONObject: request),
          let text = String(data: data, encoding: .utf8) else { return "error: the app is not connected" }
    return text.withCString { p in
        guard let r = f(p) else { return "error: no reply" }
        defer { free(r) }
        return String(cString: r)
    }
}

struct FindCommandsTool: Tool {
    let name = "find_commands"
    let description = "Search SolidCraft's commands by keywords. Returns command ids with their parameters. Use it before run_command."

    @Generable
    struct Arguments {
        @Guide(description: "keywords such as fillet, show hide, extrude")
        var query: String
    }

    func call(arguments: Arguments) async throws -> String {
        exec(["tool": "find", "query": arguments.query])
    }
}

struct RunCommandTool: Tool {
    let name = "run_command"
    let description = "Run one SolidCraft command by id. The parameters are a JSON object written as text."

    @Generable
    struct Arguments {
        @Guide(description: "the command id, such as view.show_hide")
        var command: String
        @Guide(description: "the parameters as a JSON object in text, such as {\"rule\":\"-,+deck5\"}")
        var params: String
    }

    func call(arguments: Arguments) async throws -> String {
        exec(["tool": "run", "command": arguments.command, "params": arguments.params])
    }
}

struct InspectTool: Tool {
    let name = "inspect_design"
    let description = "List the open design's bodies, parameters and what is visible."

    @Generable
    struct Arguments {
        @Guide(description: "always the word ok")
        var scope: String
    }

    func call(arguments: Arguments) async throws -> String {
        exec(["tool": "inspect"])
    }
}

struct ShowHideTool: Tool {
    let name = "show_hide"
    let description = "Show or hide bodies by a rule. A rule is comma-separated terms: '-' hides everything, '+' shows everything, '+text' shows bodies whose name contains text, '-text' hides them. Example: '-,+deck5' shows only deck5."

    @Generable
    struct Arguments {
        @Guide(description: "the rule, for example -,+deck5")
        var rule: String
    }

    func call(arguments: Arguments) async throws -> String {
        exec(["tool": "run", "command": "view.show_hide", "params": ["rule": arguments.rule]])
    }
}

struct SetParameterTool: Tool {
    let name = "set_parameter"
    let description = "Set a design parameter (a named dimension) to a value in millimetres or degrees."

    @Generable
    struct Arguments {
        @Guide(description: "the parameter name")
        var name: String
        @Guide(description: "the new value, for example 25")
        var value: String
    }

    func call(arguments: Arguments) async throws -> String {
        exec(["tool": "run", "command": "parameters.change", "params": ["name": arguments.name, "expression": arguments.value]])
    }
}

struct AddCubeTool: Tool {
    let name = "add_cube"
    let description = "Add a cube at the origin. Only the side length is needed."

    @Generable
    struct Arguments {
        @Guide(description: "the side length of the cube in millimetres")
        var size: Double
    }

    func call(arguments: Arguments) async throws -> String {
        exec(["tool": "shape", "kind": "cube", "a": arguments.size, "b": 0.0, "c": 0.0])
    }
}

struct AddBoxTool: Tool {
    let name = "add_box"
    let description = "Add a rectangular box at the origin."

    @Generable
    struct Arguments {
        @Guide(description: "length in millimetres")
        var length: Double
        @Guide(description: "width in millimetres")
        var width: Double
        @Guide(description: "height in millimetres")
        var height: Double
    }

    func call(arguments: Arguments) async throws -> String {
        exec(["tool": "shape", "kind": "box", "a": arguments.length, "b": arguments.width, "c": arguments.height])
    }
}

struct AddCylinderTool: Tool {
    let name = "add_cylinder"
    let description = "Add a cylinder at the origin."

    @Generable
    struct Arguments {
        @Guide(description: "radius in millimetres")
        var radius: Double
        @Guide(description: "height in millimetres")
        var height: Double
    }

    func call(arguments: Arguments) async throws -> String {
        exec(["tool": "shape", "kind": "cylinder", "a": arguments.radius, "b": arguments.height, "c": 0.0])
    }
}

struct AddSphereTool: Tool {
    let name = "add_sphere"
    let description = "Add a sphere at the origin."

    @Generable
    struct Arguments {
        @Guide(description: "radius in millimetres")
        var radius: Double
    }

    func call(arguments: Arguments) async throws -> String {
        exec(["tool": "shape", "kind": "sphere", "a": arguments.radius, "b": 0.0, "c": 0.0])
    }
}

struct MoveBodyTool: Tool {
    let name = "move_body"
    let description = "Move an existing body by an offset. Left and right is x (right is +X), forward and back is y (away is +Y), up and down is z (up is +Z), in millimetres. Use negative numbers for the other way."

    @Generable
    struct Arguments {
        @Guide(description: "the body name; leave empty for the newest body")
        var body: String
        @Guide(description: "offset along X in millimetres, 0 for none")
        var x: Double
        @Guide(description: "offset along Y in millimetres, 0 for none")
        var y: Double
        @Guide(description: "offset along Z in millimetres, 0 for none")
        var z: Double
    }

    func call(arguments: Arguments) async throws -> String {
        exec(["tool": "move", "body": arguments.body, "x": arguments.x, "y": arguments.y, "z": arguments.z])
    }
}

struct NeedsClaudeTool: Tool {
    let name = "cannot_do"
    let description = "Call this when the request needs anything the other tools cannot do: holes, cutting or joining shapes, resizing or editing something that already exists, patterns, or a part built in several steps. Do not guess with the other tools."

    @Generable
    struct Arguments {
        @Guide(description: "a few words on why")
        var reason: String
    }

    func call(arguments: Arguments) async throws -> String {
        exec(["tool": "escalate", "reason": arguments.reason])
    }
}

struct FilletEdgesTool: Tool {
    let name = "fillet_edges"
    let description = "Fillet (round) edges of a body."

    @Generable
    struct Arguments {
        @Guide(description: "the fillet radius in millimetres")
        var size: Double
        @Guide(description: "which edges: all, top, bottom, vertical or horizontal")
        var which: String
        @Guide(description: "the body name; leave empty for the newest body")
        var body: String
    }

    func call(arguments: Arguments) async throws -> String {
        exec(["tool": "round", "kind": "fillet", "size": arguments.size, "which": arguments.which, "body": arguments.body])
    }
}

struct ChamferEdgesTool: Tool {
    let name = "chamfer_edges"
    let description = "Chamfer (bevel) edges of a body."

    @Generable
    struct Arguments {
        @Guide(description: "the chamfer distance in millimetres")
        var size: Double
        @Guide(description: "which edges: all, top, bottom, vertical or horizontal")
        var which: String
        @Guide(description: "the body name; leave empty for the newest body")
        var body: String
    }

    func call(arguments: Arguments) async throws -> String {
        exec(["tool": "round", "kind": "chamfer", "size": arguments.size, "which": arguments.which, "body": arguments.body])
    }
}

struct UndoTool: Tool {
    let name = "undo"
    let description = "Undo the last change to the design."

    @Generable
    struct Arguments {
        @Guide(description: "always the word ok")
        var ok: String
    }

    func call(arguments: Arguments) async throws -> String {
        exec(["tool": "run", "command": "edit.undo", "params": [String: String]()])
    }
}

struct ChangeViewTool: Tool {
    let name = "change_view"
    let description = "Move the camera, not the model: pan, zoom, rotate, or look from a standard direction."

    @Generable
    struct Arguments {
        @Guide(description: "one of: pan_left, pan_right, pan_up, pan_down, zoom_in, zoom_out, rotate_left (turn about the vertical axis), rotate_right, tilt_up (tip about the horizontal axis, looking from higher), tilt_down, front, back, left, right, top, bottom, isometric, home (the Home button), fit")
        var action: String
        @Guide(description: "how much: pixels for pan, degrees for rotate and tilt (negative goes the other way), times for zoom; 0 for the usual amount")
        var amount: Double
    }

    func call(arguments: Arguments) async throws -> String {
        exec(["tool": "view", "action": arguments.action, "amount": arguments.amount])
    }
}

struct NewDesignTool: Tool {
    let name = "new_design"
    let description = "Clear the model: start an empty design. Refused while there are unsaved changes."

    @Generable
    struct Arguments {
        @Guide(description: "always the word ok")
        var ok: String
    }

    func call(arguments: Arguments) async throws -> String {
        exec(["tool": "run", "command": "file.new", "params": [String: String]()])
    }
}

private let instructions = """
You control the CAD program SolidCraft. Lengths are millimetres. Pick the tool that matches the request: \
add_cube, add_box, add_cylinder, add_sphere, move_body, show_hide, set_parameter, fillet_edges, chamfer_edges, undo, new_design, change_view (pan, zoom, rotate or standard views), inspect_design. \
These tools only add new shapes, move or round existing bodies, and show or hide. They cannot make holes, cut, join, resize existing features or build a part in steps: for those call cannot_do. \
A fillet size is a radius, never a shape size. Only add a shape when the user says add, create or make: "move the cube" means move_body on the existing body, never add_cube. Reply in one short sentence.
"""

private func makeSession() -> LanguageModelSession {
    LanguageModelSession(model: SystemLanguageModel(guardrails: .permissiveContentTransformations), tools: [InspectTool(), ShowHideTool(), SetParameterTool(), AddCubeTool(), AddBoxTool(), AddCylinderTool(), AddSphereTool(), MoveBodyTool(), NeedsClaudeTool(), FilletEdgesTool(), ChamferEdgesTool(), UndoTool(), NewDesignTool(), ChangeViewTool()], instructions: instructions)
}

/// "available", or why the model is not: the caller shows it.
@_cdecl("sc_ai_availability")
public func sc_ai_availability() -> UnsafeMutablePointer<CChar>? {
    switch SystemLanguageModel.default.availability {
    case .available: return strdup("available")
    case .unavailable(let reason): return strdup("unavailable: \(reason)")
    }
}

@_cdecl("sc_ai_set_exec")
public func sc_ai_set_exec(_ f: SCExecFn?) { execFn = f }

@_cdecl("sc_ai_reset")
public func sc_ai_reset() { session = nil }

/// Ask the model; `done` gets the answer (or an error) on a background thread.
@_cdecl("sc_ai_ask")
public func sc_ai_ask(_ prompt: UnsafePointer<CChar>, _ done: SCDoneFn) {
    let text = String(cString: prompt)
    Task {
        let reply: String
        do {
            if session == nil { session = makeSession() }
            let r = try await session!.respond(to: text)
            reply = r.content
        } catch {
            // A session that failed (its context is full, say) starts again next time.
            session = nil
            // The content filter sometimes trips on harmless CAD talk: once more, fresh.
            var again: String?
            if "\(error)".contains("guardrail") {
                session = makeSession()
                again = try? await session!.respond(to: text).content
                if again == nil { session = nil }
            }
            reply = again ?? "error: \(error)"
        }
        reply.withCString { done($0) }
    }
}


// MARK: - Speech: the microphone to text, on the device when the language supports it.

/// Text so far (kind 0), the final text (1), or an error message (2).
public typealias SCTextFn = @convention(c) (UnsafePointer<CChar>, Int32) -> Void

nonisolated(unsafe) private var audioEngine: AVAudioEngine?
nonisolated(unsafe) private var speechRequest: SFSpeechAudioBufferRecognitionRequest?
nonisolated(unsafe) private var speechTask: SFSpeechRecognitionTask?
/// Bumped whenever listening stops or starts: words from an earlier session that arrive late are dropped.
nonisolated(unsafe) private var listenGeneration = 0

private func send(_ cb: SCTextFn, _ text: String, _ kind: Int32) {
    text.withCString { cb($0, kind) }
}

private func stopListening() {
    listenGeneration += 1
    if let e = audioEngine {
        e.stop()
        e.inputNode.removeTap(onBus: 0)
    }
    audioEngine = nil
    speechRequest?.endAudio()
    speechRequest = nil
    speechTask?.finish()
    speechTask = nil
    try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
}

private func startListening(_ cb: SCTextFn) {
    stopListening()
    let generation = listenGeneration
    guard let recognizer = SFSpeechRecognizer(locale: Locale.current) ?? SFSpeechRecognizer(), recognizer.isAvailable else {
        send(cb, "error: speech recognition is not available for this language", 2)
        return
    }
    do {
        let session = AVAudioSession.sharedInstance()
        try session.setCategory(.record, mode: .measurement, options: .duckOthers)
        try session.setActive(true, options: .notifyOthersOnDeactivation)
        let request = SFSpeechAudioBufferRecognitionRequest()
        request.shouldReportPartialResults = true
        if recognizer.supportsOnDeviceRecognition { request.requiresOnDeviceRecognition = true }
        let engine = AVAudioEngine()
        let input = engine.inputNode
        input.installTap(onBus: 0, bufferSize: 1024, format: input.outputFormat(forBus: 0)) { buffer, _ in
            request.append(buffer)
        }
        engine.prepare()
        try engine.start()
        audioEngine = engine
        speechRequest = request
        speechTask = recognizer.recognitionTask(with: request) { result, error in
            guard generation == listenGeneration else { return }
            if let r = result {
                send(cb, r.bestTranscription.formattedString, r.isFinal ? 1 : 0)
            }
            if let error = error, result == nil || result?.isFinal == true {
                stopListening()
                if result == nil { send(cb, "error: \(error.localizedDescription)", 2) }
            } else if result?.isFinal == true {
                stopListening()
            }
        }
    } catch {
        stopListening()
        send(cb, "error: \(error.localizedDescription)", 2)
    }
}

/// Start listening (asking for permission the first time). `cb` gets the words as they come.
@_cdecl("sc_ai_listen_start")
public func sc_ai_listen_start(_ cb: SCTextFn) {
    SFSpeechRecognizer.requestAuthorization { status in
        guard status == .authorized else {
            send(cb, "error: speech recognition is not allowed: turn it on in Settings > SolidCraft", 2)
            return
        }
        AVAudioApplication.requestRecordPermission { granted in
            guard granted else {
                send(cb, "error: the microphone is not allowed: turn it on in Settings > SolidCraft", 2)
                return
            }
            DispatchQueue.main.async { startListening(cb) }
        }
    }
}

@_cdecl("sc_ai_listen_stop")
public func sc_ai_listen_stop() {
    DispatchQueue.main.async { stopListening() }
}


// MARK: - Claude (the Anthropic API) as the second tier. The key lives in the Keychain.

private let keyService = "io.solidcraft.ai"
private let keyAccount = "anthropic-api-key"

private func loadKey() -> String? {
    let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: keyService,
                                kSecAttrAccount as String: keyAccount, kSecReturnData as String: true,
                                kSecMatchLimit as String: kSecMatchLimitOne]
    var item: CFTypeRef?
    guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess, let data = item as? Data else { return nil }
    return String(data: data, encoding: .utf8)
}

@_cdecl("sc_ai_set_key")
public func sc_ai_set_key(_ key: UnsafePointer<CChar>) -> Int32 {
    let text = String(cString: key).trimmingCharacters(in: .whitespacesAndNewlines)
    guard !text.isEmpty, let data = text.data(using: .utf8) else { return 0 }
    let base: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: keyService,
                               kSecAttrAccount as String: keyAccount]
    SecItemDelete(base as CFDictionary)
    var add = base
    add[kSecValueData as String] = data
    add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
    return SecItemAdd(add as CFDictionary, nil) == errSecSuccess ? 1 : 0
}

@_cdecl("sc_ai_has_key")
public func sc_ai_has_key() -> Int32 { loadKey() == nil ? 0 : 1 }

@_cdecl("sc_ai_clear_key")
public func sc_ai_clear_key() {
    let base: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: keyService,
                               kSecAttrAccount as String: keyAccount]
    SecItemDelete(base as CFDictionary)
}

/// Requests and tokens used so far, as JSON text for the window.
@_cdecl("sc_ai_claude_usage")
public func sc_ai_claude_usage() -> UnsafeMutablePointer<CChar>? {
    let d = UserDefaults.standard
    return strdup("{\"requests\":\(d.integer(forKey: "claude_requests")),\"input\":\(d.integer(forKey: "claude_in")),\"output\":\(d.integer(forKey: "claude_out"))}")
}

private func schema(_ props: [String: (String, String)], required: [String]) -> [String: Any] {
    var p: [String: Any] = [:]
    for (k, v) in props { p[k] = ["type": v.0, "description": v.1] }
    return ["type": "object", "properties": p, "required": required]
}

private let claudeTools: [[String: Any]] = [
    ["name": "inspect_design", "description": "List the open design's bodies (and which are hidden) and its parameters.",
     "input_schema": ["type": "object", "properties": [String: Any]()]],
    ["name": "add_shape", "description": "Add a box, cube, cylinder or sphere, optionally positioned, and optionally joined to or cut from an existing body in the same step. Millimetres. Box: size1 length, size2 width, size3 height, placed with its corner at x,y,z. Cube: only size1. Cylinder: size1 radius, size2 height, standing up from its base centre x,y,z. Sphere: size1 radius, centred on x,y,z. operation: new (default, a separate body), join (merge into target) or cut (remove from target, e.g. a hole: a cylinder placed so it passes through the body). target: the body name for join or cut, empty for the newest body.",
     "input_schema": schema(["kind": ("string", "box, cube, cylinder or sphere"), "size1": ("number", "first size"), "size2": ("number", "second size, 0 if unused"), "size3": ("number", "third size, 0 if unused"),
                             "x": ("number", "position X, 0 default"), "y": ("number", "position Y, 0 default"), "z": ("number", "position Z, 0 default"),
                             "operation": ("string", "new, join or cut"), "target": ("string", "body to join to or cut, empty for the newest")], required: ["kind", "size1"])],
    ["name": "show_hide", "description": "Show or hide bodies by a rule: comma-separated terms, '-' hides all, '+' shows all, '+text' shows bodies whose name contains text, '-text' hides them, '+@hull' shows bodies overlapping the body named hull. Example: '-,+deck5'.",
     "input_schema": schema(["rule": ("string", "the rule")], required: ["rule"])],
    ["name": "set_parameter", "description": "Set a named design parameter to a value (millimetres or degrees, or an expression).",
     "input_schema": schema(["name": ("string", "parameter name"), "value": ("string", "new value")], required: ["name", "value"])],
    ["name": "fillet_edges", "description": "Fillet (round) edges of a body.",
     "input_schema": schema(["size": ("number", "radius in millimetres"), "which": ("string", "all, top, bottom, vertical or horizontal"), "body": ("string", "body name or empty for the newest")], required: ["size", "which"])],
    ["name": "chamfer_edges", "description": "Chamfer (bevel) edges of a body.",
     "input_schema": schema(["size": ("number", "distance in millimetres"), "which": ("string", "all, top, bottom, vertical or horizontal"), "body": ("string", "body name or empty for the newest")], required: ["size", "which"])],
    ["name": "move_body", "description": "Move a body by an offset in millimetres: x right (+X), y away (+Y), z up (+Z).",
     "input_schema": schema(["body": ("string", "body name or empty for the newest"), "x": ("number", "offset along X"), "y": ("number", "offset along Y"), "z": ("number", "offset along Z")], required: ["x", "y", "z"])],
    ["name": "change_view", "description": "Move the camera (not the model): pan, zoom, rotate, or look from a standard direction.",
     "input_schema": schema(["action": ("string", "pan_left, pan_right, pan_up, pan_down, zoom_in, zoom_out, rotate_left (turn about the vertical axis), rotate_right, tilt_up (tip about the horizontal axis), tilt_down, front, back, left, right, top, bottom, isometric, home or fit"), "amount": ("number", "pixels for pan, degrees for rotate and tilt (negative goes the other way), times for zoom; 0 for the usual amount")], required: ["action"])],
    ["name": "get_selection", "description": "What the user has selected in the model (bodies, faces, edges), each with a point on it. Call it when the request says selected, this, that or highlighted. A face is passed to commands as faces: [[x,y,z]] with its point.",
     "input_schema": ["type": "object", "properties": [String: Any]()]],
    ["name": "undo", "description": "Undo the last change.", "input_schema": ["type": "object", "properties": [String: Any]()]],
    ["name": "new_design", "description": "Start an empty design. Refused while there are unsaved changes.", "input_schema": ["type": "object", "properties": [String: Any]()]],
    ["name": "find_commands", "description": "Search SolidCraft's other commands by keywords; returns ids with their parameter descriptions. Use before run_command.",
     "input_schema": schema(["query": ("string", "keywords")], required: ["query"])],
    ["name": "run_command", "description": "Run any SolidCraft command by id with its parameters as a JSON object.",
     "input_schema": ["type": "object", "properties": ["command": ["type": "string", "description": "the command id"], "params": ["type": "object", "description": "the parameters"]], "required": ["command"]]],
]

func claudeExec(_ name: String, _ input: [String: Any]) -> String {
    func str(_ k: String) -> String { (input[k] as? String) ?? "" }
    func num(_ k: String) -> Double { (input[k] as? Double) ?? Double(input[k] as? Int ?? 0) }
    switch name {
    case "inspect_design": return exec(["tool": "inspect"])
    case "add_shape": return exec(["tool": "shape", "kind": str("kind"), "a": num("size1"), "b": num("size2"), "c": num("size3"),
                                   "x": num("x"), "y": num("y"), "z": num("z"), "operation": str("operation"), "target": str("target")])
    case "show_hide": return exec(["tool": "run", "command": "view.show_hide", "params": ["rule": str("rule")]])
    case "set_parameter": return exec(["tool": "run", "command": "parameters.change", "params": ["name": str("name"), "expression": str("value")]])
    case "fillet_edges": return exec(["tool": "round", "kind": "fillet", "size": num("size"), "which": str("which"), "body": str("body")])
    case "chamfer_edges": return exec(["tool": "round", "kind": "chamfer", "size": num("size"), "which": str("which"), "body": str("body")])
    case "move_body": return exec(["tool": "move", "body": str("body"), "x": num("x"), "y": num("y"), "z": num("z")])
    case "change_view": return exec(["tool": "view", "action": str("action"), "amount": num("amount")])
    case "get_selection": return exec(["tool": "selection"])
    case "undo": return exec(["tool": "run", "command": "edit.undo", "params": [String: String]()])
    case "new_design": return exec(["tool": "run", "command": "file.new", "params": [String: String]()])
    case "find_commands": return exec(["tool": "find", "query": str("query")])
    case "run_command": return exec(["tool": "run", "command": str("command"), "params": input["params"] ?? [String: String]()])
    default: return "error: unknown tool \(name)"
    }
}

private let claudeSystem = """
You control the CAD program SolidCraft for the user through tools. Lengths are millimetres. \
Prefer the specific tools. To change something that already exists, call inspect_design: it lists the timeline features by name (use them with timeline.inspect and timeline.edit through run_command) and the parameters with the feature input each drives (change those with set_parameter). When you build a part with dimensions the user may change later, create named user parameters first (parameters.add through run_command) and use them in feature expressions, so a later change is one parameter edit. For anything else use find_commands to read a command's parameters, then run_command; never guess parameter names. \
When the request says "selected", "this" or "that", call get_selection first. Make only the changes the user asked for. Work efficiently: when several steps do not depend on each other, call several tools in the same reply. Place shapes directly with x, y, z and use operation cut for holes (a cylinder through the body) and join for bosses, instead of creating a shape and moving it afterwards. If an earlier attempt is described, it was already applied: check with inspect_design and finish or fix it (undo if it was wrong). \
Reply in one short sentence saying what you did.
"""

private struct ClaudeError: Error { let text: String }

private func claudeCall(_ key: String, _ body: [String: Any]) async throws -> [String: Any] {
    guard let url = URL(string: "https://api.anthropic.com/v1/messages") else { throw ClaudeError(text: "bad url") }
    var req = URLRequest(url: url)
    req.httpMethod = "POST"
    req.timeoutInterval = 60
    req.setValue(key, forHTTPHeaderField: "x-api-key")
    req.setValue("2023-06-01", forHTTPHeaderField: "anthropic-version")
    req.setValue("application/json", forHTTPHeaderField: "content-type")
    req.httpBody = try JSONSerialization.data(withJSONObject: body)
    let (data, response) = try await URLSession.shared.data(for: req)
    let obj = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? [:]
    if let http = response as? HTTPURLResponse, http.statusCode != 200 {
        let message = ((obj["error"] as? [String: Any])?["message"] as? String) ?? "HTTP \(http.statusCode)"
        throw ClaudeError(text: "\(http.statusCode): \(message)")
    }
    return obj
}

/// Ask Claude (the model named in `model`) with the tools; `done` gets the answer or an error.
@_cdecl("sc_ai_claude_ask")
public func sc_ai_claude_ask(_ prompt: UnsafePointer<CChar>, _ model: UnsafePointer<CChar>, _ done: SCDoneFn) {
    let text = String(cString: prompt)
    let modelName = String(cString: model)
    Task {
        func finish(_ s: String) { s.withCString { done($0) } }
        guard let key = loadKey() else { finish("error: no Claude key saved"); return }
        var messages: [[String: Any]] = [["role": "user", "content": text]]
        let d = UserDefaults.standard
        d.set(d.integer(forKey: "claude_requests") + 1, forKey: "claude_requests")
        do {
            var usedTokens = 0
            for _ in 0..<20 {
                let reply = try await claudeCall(key, ["model": modelName, "max_tokens": 1024, "system": claudeSystem,
                                                       "tools": claudeTools, "messages": messages])
                if let usage = reply["usage"] as? [String: Any] {
                    usedTokens += (usage["input_tokens"] as? Int ?? 0) + (usage["output_tokens"] as? Int ?? 0)
                    d.set(d.integer(forKey: "claude_in") + (usage["input_tokens"] as? Int ?? 0), forKey: "claude_in")
                    d.set(d.integer(forKey: "claude_out") + (usage["output_tokens"] as? Int ?? 0), forKey: "claude_out")
                }
                if usedTokens > 150_000 { finish("error: Claude stopped: this request used too many tokens"); return }
                let content = reply["content"] as? [[String: Any]] ?? []
                let uses = content.filter { ($0["type"] as? String) == "tool_use" }
                if uses.isEmpty {
                    finish(content.compactMap { $0["text"] as? String }.joined(separator: " "))
                    return
                }
                messages.append(["role": "assistant", "content": content])
                var results: [[String: Any]] = []
                for u in uses {
                    let result = claudeExec(u["name"] as? String ?? "", u["input"] as? [String: Any] ?? [:])
                    results.append(["type": "tool_result", "tool_use_id": u["id"] as? String ?? "", "content": result])
                }
                messages.append(["role": "user", "content": results])
            }
            finish("error: Claude used too many steps; ask again to carry on from where it stopped")
        } catch let e as ClaudeError {
            finish("error: Claude \(e.text)")
        } catch {
            let ns = error as NSError
            let offline = ns.domain == NSURLErrorDomain && [NSURLErrorNotConnectedToInternet, NSURLErrorCannotFindHost, NSURLErrorCannotConnectToHost, NSURLErrorTimedOut, NSURLErrorNetworkConnectionLost].contains(ns.code)
            finish(offline ? "error: offline" : "error: \(error.localizedDescription)")
        }
    }
}


/// The clipboard's text (empty if none). iOS asks the user to allow pasting the first time.
@_cdecl("sc_ai_paste")
public func sc_ai_paste() -> UnsafeMutablePointer<CChar>? {
    strdup(UIPasteboard.general.string ?? "")
}
