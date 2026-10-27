// The local model (MLX): downloaded once from Hugging Face, then used when there is no internet.
// Loaded like the rest of this framework by the Rust app through the C functions at the bottom.
import Foundation
import Network
import HuggingFace
import MLXHuggingFace
import MLXLLM
import MLXLMCommon
import Tokenizers

private let localModelID = "mlx-community/Qwen3-4B-Instruct-2507-4bit"
private let localReadyKey = "local_model_ready"

nonisolated(unsafe) private var localContainer: ModelContainer?
nonisolated(unsafe) private var localLoading = false
nonisolated(unsafe) private var onlineNow = true
nonisolated(unsafe) private var pathMonitor: NWPathMonitor?
nonisolated(unsafe) private var localToolUsed = false

private let localToolNames = ["inspect_design", "show_hide", "set_parameter", "add_cube", "add_box", "add_cylinder", "add_sphere", "move_body",
                              "cannot_do", "fillet_edges", "chamfer_edges", "undo", "change_view", "new_design"]

/// A model sometimes writes its call as text (cannot_do{"reason": "..."}, add_box(80, 50, 8)): read it here.
private func textToolCall(_ text: String) -> ToolCall? {
    guard let name = localToolNames.first(where: { text.contains($0) }), let r = text.range(of: name) else { return nil }
    var args: [String: JSONValue] = [:]
    let rest = String(text[r.upperBound...]).trimmingCharacters(in: .whitespacesAndNewlines)
    if rest.hasPrefix("{"), let end = rest.lastIndex(of: "}"),
       let data = String(rest[...end]).data(using: .utf8),
       let decoded = try? JSONDecoder().decode([String: JSONValue].self, from: data) {
        args = decoded
    }
    return ToolCall(function: .init(name: name, arguments: args))
}

private func fn(_ name: String, _ description: String, _ props: [String: (String, String)], required: [String]? = nil) -> [String: any Sendable] {
    var p: [String: any Sendable] = [:]
    for (k, v) in props { p[k] = ["type": v.0, "description": v.1] as [String: String] }
    let params: [String: any Sendable] = ["type": "object", "properties": p, "required": required ?? Array(props.keys)]
    return ["type": "function", "function": ["name": name, "description": description, "parameters": params] as [String: any Sendable]]
}

/// The same typed tools as the on-device Apple model (scored in scratchpad/llmtest/bench.py).
private let localTools: [[String: any Sendable]] = [
    fn("inspect_design", "List the open design's bodies, parameters and what is visible.", [:]),
    fn("show_hide", "Show or hide bodies by a rule. A rule is comma-separated terms: '-' hides everything, '+' shows everything, '+text' shows bodies whose name contains text, '-text' hides them. Example: -,+deck5", ["rule": ("string", "the rule, for example -,+deck5")]),
    fn("set_parameter", "Set a design parameter (a named dimension) to a value in millimetres or degrees.", ["name": ("string", "the parameter name"), "value": ("string", "the new value, for example 25")]),
    fn("add_cube", "Add a cube at the origin. Set x, y, z (its corner) only when the user says where it goes.", ["size": ("number", "the side length in millimetres"), "x": ("number", "position X, 0 for the origin"), "y": ("number", "position Y, 0 for the origin"), "z": ("number", "position Z, 0 for the origin")], required: ["size"]),
    fn("add_box", "Add a rectangular box at the origin. Set x, y, z (its corner) only when the user says where it goes.", ["length": ("number", "length in millimetres"), "width": ("number", "width in millimetres"), "height": ("number", "height in millimetres"), "x": ("number", "position X, 0 for the origin"), "y": ("number", "position Y, 0 for the origin"), "z": ("number", "position Z, 0 for the origin")], required: ["length", "width", "height"]),
    fn("add_cylinder", "Add a cylinder standing up, at the origin. Set x, y, z (the centre of its base) only when the user says where it goes.", ["radius": ("number", "radius in millimetres"), "height": ("number", "height in millimetres"), "x": ("number", "position X, 0 for the origin"), "y": ("number", "position Y, 0 for the origin"), "z": ("number", "position Z, 0 for the origin")], required: ["radius", "height"]),
    fn("add_sphere", "Add a sphere at the origin. Set x, y, z (its centre) only when the user says where it goes.", ["radius": ("number", "radius in millimetres"), "x": ("number", "position X, 0 for the origin"), "y": ("number", "position Y, 0 for the origin"), "z": ("number", "position Z, 0 for the origin")], required: ["radius"]),
    fn("move_body", "Move an existing body by an offset. Left and right is x (right is +X), forward and back is y (away is +Y), up and down is z (up is +Z), in millimetres. Use negative numbers for the other way.",
       ["body": ("string", "the body name; leave empty for the newest body"), "x": ("number", "offset along X, 0 for none"), "y": ("number", "offset along Y, 0 for none"), "z": ("number", "offset along Z, 0 for none")]),
    fn("cannot_do", "Call this when the request needs anything the other tools cannot do: holes, cutting or joining shapes, resizing or editing something that already exists, patterns, colours, renaming, splitting, or a part built in several steps. Do not guess with the other tools.", ["reason": ("string", "a few words on why")]),
    fn("fillet_edges", "Fillet (round) edges of a body.", ["size": ("number", "the fillet radius in millimetres"), "which": ("string", "which edges: all, top, bottom, vertical or horizontal"), "body": ("string", "the body name; leave empty for the newest body")]),
    fn("chamfer_edges", "Chamfer (bevel) edges of a body.", ["size": ("number", "the chamfer distance in millimetres"), "which": ("string", "which edges: all, top, bottom, vertical or horizontal"), "body": ("string", "the body name; leave empty for the newest body")]),
    fn("undo", "Undo the last change to the design.", [:]),
    fn("change_view", "Move the camera, not the model: pan, zoom, rotate, or look from a standard direction.",
       ["action": ("string", "one of: pan_left, pan_right, pan_up, pan_down, zoom_in, zoom_out, rotate_left (turn about the vertical axis), rotate_right, tilt_up (tip about the horizontal axis), tilt_down, front, back, left, right, top, bottom, isometric, home (the Home button), fit"),
        "amount": ("number", "pixels for pan, degrees for rotate and tilt (negative goes the other way), times for zoom; 0 for the usual amount")], required: ["action"]),
    fn("new_design", "Start an empty design in a new tab. Only when the user asks for a new or empty design.", [:]),
]

private let localInstructions = """
You control the CAD program SolidCraft. Lengths are millimetres. Pick the tool that matches the request. \
These tools only add new shapes, move or round existing bodies, show or hide, and move the camera. They cannot make holes, cut, join, \
resize existing features, rename, colour or build a part in steps: for those call cannot_do. \
A fillet size is a radius, never a shape size. \
Show and hide rules: hide all is "-"; show all is "+"; hide the boss is "-boss"; show only the plate is "-,+plate". \
The design is listed with the request: every body has its extent [x min..max, y min..max, z min..max]. Use those names and numbers, and never create a body that already exists. \
To put a new shape beside or on top of a body, give its position when you add it: x, y, z is the corner of a cube or box, the base centre of a cylinder, the centre of a sphere. \
Example: a 30 mm cube to the left of a body that starts at x=0 has x=-30; to the right of a body that ends at x=80 has x=80; on top of a body whose top is z=8 has z=8. \
Only give a position when the request says where the shape goes; otherwise leave x, y and z at 0. Make every call the request needs, one shape per call. Do only what the user asked: no extra fillets, chamfers or other changes. Then reply in one short sentence.
"""

/// Runs a tool call from the local model through the same app code as the other tiers.
private func localDispatch(_ call: ToolCall) -> String {
    localToolUsed = true
    let a = call.function.arguments.mapValues { $0.anyValue }
    func str(_ k: String) -> String { (a[k] as? String) ?? (a[k].map { "\($0)" } ?? "") }
    func num(_ k: String) -> Double {
        if let d = a[k] as? Double { return d }
        if let i = a[k] as? Int { return Double(i) }
        return Double(str(k).trimmingCharacters(in: .whitespaces)) ?? 0
    }
    switch call.function.name {
    case "add_cube": return claudeExec("add_shape", ["kind": "cube", "size1": num("size"), "x": num("x"), "y": num("y"), "z": num("z")])
    case "add_box": return claudeExec("add_shape", ["kind": "box", "size1": num("length"), "size2": num("width"), "size3": num("height"), "x": num("x"), "y": num("y"), "z": num("z")])
    case "add_cylinder": return claudeExec("add_shape", ["kind": "cylinder", "size1": num("radius"), "size2": num("height"), "x": num("x"), "y": num("y"), "z": num("z")])
    case "add_sphere": return claudeExec("add_shape", ["kind": "sphere", "size1": num("radius"), "x": num("x"), "y": num("y"), "z": num("z")])
    case "show_hide": return claudeExec("show_hide", ["rule": str("rule")])
    case "set_parameter": return claudeExec("set_parameter", ["name": str("name"), "value": str("value")])
    case "fillet_edges", "chamfer_edges": return claudeExec(call.function.name, ["size": num("size"), "which": str("which"), "body": str("body")])
    case "move_body": return claudeExec("move_body", ["body": str("body"), "x": num("x"), "y": num("y"), "z": num("z")])
    case "change_view": return claudeExec("change_view", ["action": str("action"), "amount": num("amount")])
    case "inspect_design", "undo", "new_design": return claudeExec(call.function.name, [:])
    case "cannot_do": return exec(["tool": "escalate", "reason": str("reason")])
    default: return "error: unknown tool \(call.function.name)"
    }
}

/// "ready" when the model is loaded or was downloaded before, "loading", or "none".
@_cdecl("sc_ai_local_state")
public func sc_ai_local_state() -> UnsafeMutablePointer<CChar>? {
    if localLoading { return strdup("loading") }
    if localContainer != nil || UserDefaults.standard.bool(forKey: localReadyKey) { return strdup("ready") }
    return strdup("none")
}

/// Download (the first time) and load the model. `cb` gets "NN%" as it downloads (kind 0), "ready" (1) or an error (2).
@_cdecl("sc_ai_local_load")
public func sc_ai_local_load(_ cb: SCTextFn) {
    if localLoading { return }
    localLoading = true
    Task {
        defer { localLoading = false }
        do {
            let model = try await #huggingFaceLoadModelContainer(
                configuration: ModelConfiguration(id: localModelID),
                progressHandler: { p in
                    let text = "\(Int(p.fractionCompleted * 100))%"
                    text.withCString { cb($0, 0) }
                })
            localContainer = model
            UserDefaults.standard.set(true, forKey: localReadyKey)
            "ready".withCString { cb($0, 1) }
        } catch {
            "error: \(error.localizedDescription)".withCString { cb($0, 2) }
        }
    }
}

/// Free the model's memory (it loads again on the next request).
@_cdecl("sc_ai_local_unload")
public func sc_ai_local_unload() { localContainer = nil }

/// Ask the local model; `done` gets the answer or an error.
@_cdecl("sc_ai_local_ask")
public func sc_ai_local_ask(_ prompt: UnsafePointer<CChar>, _ done: SCDoneFn) {
    let text = String(cString: prompt)
    Task {
        func finish(_ s: String) { s.withCString { done($0) } }
        do {
            if localContainer == nil {
                localContainer = try await #huggingFaceLoadModelContainer(configuration: ModelConfiguration(id: localModelID))
                UserDefaults.standard.set(true, forKey: localReadyKey)
            }
            guard let container = localContainer else { finish("error: the local model is not loaded"); return }
            // The design's bodies and their extents go with the request, so the model need not ask for them.
            let design = exec(["tool": "inspect"])
            localToolUsed = false
            let chat = ChatSession(container, instructions: localInstructions,
                                   generateParameters: GenerateParameters(maxTokens: 300, temperature: 0),
                                   tools: localTools, toolDispatch: { call in localDispatch(call) })
            let reply = try await chat.respond(to: "Current design:\n\(design)\n\nRequest: \(text)")
            if !localToolUsed, let call = textToolCall(reply) {
                if call.function.name == "cannot_do" {
                    _ = localDispatch(call)
                    finish("I can't do that with the local tools: it needs Claude, which needs the internet.")
                } else {
                    finish(localDispatch(call))
                }
            } else {
                finish(reply)
            }
        } catch {
            finish("error: \(error.localizedDescription)")
        }
    }
}

/// 1 when the iPad has a network path, else 0.
@_cdecl("sc_ai_online")
public func sc_ai_online() -> Int32 {
    if pathMonitor == nil {
        let m = NWPathMonitor()
        m.pathUpdateHandler = { onlineNow = $0.status == .satisfied }
        m.start(queue: DispatchQueue(label: "solidcraft.path"))
        pathMonitor = m
    }
    return onlineNow ? 1 : 0
}
