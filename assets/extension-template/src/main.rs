//! EXT_NAME: an extension for SSH Remote. It talks with the app over stdin and stdout, one
//! JSON-RPC 2.0 message per line.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

/// The id of our form request, to know its answer.
const FORM: u64 = 1;

fn main() {
    let mut greeting = "Hello".to_string();
    let mut said = 0u32;
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match method {
            "initialize" | "settings" => {
                let settings = if method == "initialize" { params.get("settings").cloned().unwrap_or(Value::Null) } else { params.clone() };
                if let Some(g) = settings.get("greeting").and_then(|g| g.as_str()) {
                    greeting = g.to_string();
                }
                send("status.set", json!({ "id": "main", "text": format!("EXT_NAME: {said}"), "tooltip": "Click to say hello", "command": "hello" }), None);
            }
            "command" => match params.get("id").and_then(|i| i.as_str()) {
                Some("hello") => {
                    said += 1;
                    send("status.set", json!({ "id": "main", "text": format!("EXT_NAME: {said}"), "command": "hello" }), None);
                    let md = format!("# {greeting}\n\nSaid {said} times.\n\n[Open a terminal](sshremote:terminal)\n");
                    send("panel.show", json!({ "id": "main", "title": "EXT_NAME", "markdown": md }), None);
                }
                Some("ask") => ask(),
                _ => {}
            },
            // A link `sshremote:terminal` in the panel.
            "action" if params.get("action").and_then(|a| a.as_str()) == Some("terminal") => ask(),
            "shutdown" => break,
            _ => {}
        }
        // The form's answer.
        if msg.get("id").and_then(|i| i.as_u64()) == Some(FORM)
            && let Some(v) = msg.pointer("/result/values")
        {
            let cmd = v.get("command").and_then(|c| c.as_str()).unwrap_or("").to_string();
            let split = if v.get("beside").and_then(|b| b.as_bool()).unwrap_or(false) { "right" } else { "" };
            let argv: Vec<&str> = cmd.split_whitespace().collect();
            send("terminal.open", json!({ "argv": argv, "label": cmd, "split": split }), Some(2));
        }
    }
}

fn ask() {
    let fields = json!([
        { "id": "command", "label": "Command", "type": "text", "value": "" },
        { "id": "beside", "label": "Beside this pane", "type": "bool", "value": true }
    ]);
    send("input.form", json!({ "title": "Open a terminal", "prompt": "Empty: your login shell.", "submit": "Open", "fields": fields }), Some(FORM));
}

fn send(method: &str, params: Value, id: Option<u64>) {
    let mut msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
    if let Some(id) = id {
        msg["id"] = json!(id);
    }
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}
