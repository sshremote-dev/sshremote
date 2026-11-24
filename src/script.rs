//! Script extensions: WebAssembly modules an extension brings, run in an interpreter with a fuel
//! budget and a memory cap, with nothing of this computer reachable from inside. A command
//! receives JSON and returns text; what may go in and what its text may do is the extension's
//! declared permissions.
//!
//! extension.json:
//!   "main": "script.wasm",
//!   "permissions": ["terminal.selection", "terminal.write"],
//!   "contributes": { "commands": [ { "id": "upper", "label": "Upper case" } ] }
//!
//! The module exports `memory`, `alloc(len: i32) -> i32`, and per command a function
//! `run_ID(ptr: i32, len: i32) -> i64` taking its input (UTF-8 JSON at ptr) and returning
//! `(ptr << 32) | len` of its output (UTF-8 text). It may import `env.log(ptr, len)`.

use wasmi::{Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};

/// Instructions a command may run before it is stopped (well under a second interpreted).
const FUEL: u64 = 200_000_000;
/// Memory a command may use.
const MEMORY: usize = 64 << 20;
/// Output kept.
const OUTPUT: usize = 1 << 20;

/// What a script may be given and may do, as extension.json names them.
pub const PERMISSIONS: [(&str, &str); 6] = [
    ("workspace.open", "Open terminals in the workspace"),
    ("workspace.read", "Know which tabs and hosts are open"),
    ("native", "Run its own program on this computer, with your rights"),
    ("terminal.selection", "Read the text selected in a terminal when you run its command"),
    ("terminal.write", "Type its result into the terminal (as a paste you can still edit)"),
    ("programs", "Start programs of this computer's in a pane: a command line or a file it names"),
];

/// A command an extension offers.
#[derive(Clone, Debug, PartialEq)]
pub struct Command {
    pub ext: String,
    pub id: String,
    pub label: String,
}

struct State {
    limits: StoreLimits,
    log: String,
}

/// Runs command `id` of the module with `input`; its text, or why it failed.
pub fn run(wasm: &[u8], id: &str, input: &str) -> Result<String, String> {
    let mut config = Config::default();
    config.consume_fuel(true);
    let engine = Engine::new(&config);
    let module = Module::new(&engine, wasm).map_err(|e| format!("the script cannot be read: {e}"))?;
    let limits = StoreLimitsBuilder::new().memory_size(MEMORY).instances(1).memories(1).tables(4).build();
    let mut store = Store::new(&engine, State { limits, log: String::new() });
    store.limiter(|s| &mut s.limits);
    store.set_fuel(FUEL).map_err(|e| e.to_string())?;
    let mut linker = <Linker<State>>::new(&engine);
    linker
        .func_wrap("env", "log", |caller: wasmi::Caller<'_, State>, ptr: i32, len: i32| {
            let Some(mem) = caller.get_export("memory").and_then(|e| e.into_memory()) else { return };
            let data = mem.data(&caller);
            let (p, n) = (ptr as u32 as usize, (len as u32 as usize).min(4096));
            if let Some(b) = data.get(p..p.saturating_add(n)) {
                let line = String::from_utf8_lossy(b).into_owned();
                let mut caller = caller;
                let log = &mut caller.data_mut().log;
                if log.len() < 16 << 10 {
                    log.push_str(&line);
                    log.push('\n');
                }
            }
        })
        .map_err(|e| e.to_string())?;
    let instance = linker.instantiate_and_start(&mut store, &module).map_err(|e| format!("the script did not start: {e}"))?;
    let memory = instance.get_memory(&store, "memory").ok_or("the script has no memory export")?;
    let alloc = instance.get_typed_func::<i32, i32>(&store, "alloc").map_err(|_| "the script has no alloc export")?;
    let func = instance
        .get_typed_func::<(i32, i32), i64>(&store, &format!("run_{id}"))
        .map_err(|_| format!("the script has no command {id}"))?;
    let bytes = input.as_bytes();
    let ptr = alloc.call(&mut store, bytes.len() as i32).map_err(|e| trap(e, &store))?;
    memory.write(&mut store, ptr as u32 as usize, bytes).map_err(|_| "the script's alloc gave a bad place")?;
    let packed = func.call(&mut store, (ptr, bytes.len() as i32)).map_err(|e| trap(e, &store))?;
    let (p, n) = ((packed as u64 >> 32) as usize, (packed as u64 & 0xffff_ffff) as usize);
    let data = memory.data(&store);
    let out = data.get(p..p.saturating_add(n.min(OUTPUT))).ok_or("the script's result is outside its memory")?;
    Ok(String::from_utf8_lossy(out).into_owned())
}

fn trap(e: wasmi::Error, store: &Store<State>) -> String {
    let log = &store.data().log;
    let why = if matches!(e.as_trap_code(), Some(wasmi::TrapCode::OutOfFuel)) {
        "it ran too long and was stopped".to_string()
    } else {
        e.to_string()
    };
    if log.is_empty() { format!("the script failed: {why}") } else { format!("the script failed: {why}\n{log}") }
}

/// The commands of installed extensions.
pub fn commands() -> Vec<Command> {
    crate::ext::installed()
        .into_iter()
        .filter(|m| m.main.is_some())
        .flat_map(|m| m.commands.iter().map(|(id, label)| Command { ext: m.id.clone(), id: id.clone(), label: label.clone() }).collect::<Vec<_>>())
        .collect()
}

/// Runs an installed extension's command with what its permissions let it have; its text, and
/// whether it may be typed into the terminal.
pub fn run_command(c: &Command, selection: &str, host: &str) -> Result<(String, bool), String> {
    let m = crate::ext::installed().into_iter().find(|m| m.id == c.ext).ok_or("the extension is no longer installed")?;
    let main = m.main.as_deref().ok_or("the extension has no script")?;
    let wasm = std::fs::read(crate::ext::resolve(&format!("ext:{}/{main}", m.id))).map_err(|e| e.to_string())?;
    let may = |p: &str| m.permissions.iter().any(|x| x == p);
    let input = serde_json::json!({
        "selection": if may("terminal.selection") { selection } else { "" },
        "host": host,
    });
    Ok((run(&wasm, &c.id, &input.to_string())?, may("terminal.write")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny module: `upper` returns its input's ASCII letters in upper case; `spin` never ends.
    const WAT: &str = r#"(module
      (import "env" "log" (func $log (param i32 i32)))
      (memory (export "memory") 1)
      (global $top (mut i32) (i32.const 1024))
      (func (export "alloc") (param $n i32) (result i32)
        (local $p i32) (local.set $p (global.get $top))
        (global.set $top (i32.add (global.get $top) (local.get $n))) (local.get $p))
      (func (export "run_upper") (param $p i32) (param $n i32) (result i64)
        (local $i i32) (local $c i32)
        (call $log (local.get $p) (i32.const 0))
        (block $done (loop $next
          (br_if $done (i32.ge_u (local.get $i) (local.get $n)))
          (local.set $c (i32.load8_u (i32.add (local.get $p) (local.get $i))))
          (if (i32.and (i32.ge_u (local.get $c) (i32.const 97)) (i32.le_u (local.get $c) (i32.const 122)))
            (then (i32.store8 (i32.add (local.get $p) (local.get $i)) (i32.sub (local.get $c) (i32.const 32)))))
          (local.set $i (i32.add (local.get $i) (i32.const 1)))
          (br $next)))
        (i64.or (i64.shl (i64.extend_i32_u (local.get $p)) (i64.const 32)) (i64.extend_i32_u (local.get $n))))
      (func (export "run_spin") (param i32 i32) (result i64) (loop $l (br $l)) (i64.const 0))
      (func (export "run_grow") (param i32 i32) (result i64)
        (drop (memory.grow (i32.const 5000))) (if (i32.lt_s (memory.grow (i32.const 2000)) (i32.const 0)) (then unreachable)) (i64.const 0)))"#;

    pub fn module() -> Vec<u8> {
        wat::parse_str(WAT).unwrap()
    }

    #[test]
    fn commands_run_and_are_held_in() {
        let wasm = module();
        assert_eq!(run(&wasm, "upper", r#"{"selection":"ls -la"}"#).unwrap(), r#"{"SELECTION":"LS -LA"}"#);
        assert!(run(&wasm, "spin", "").unwrap_err().contains("too long"));
        assert!(run(&wasm, "grow", "").is_err(), "memory past the cap is refused");
        assert!(run(&wasm, "missing", "").unwrap_err().contains("no command"));
        assert!(run(b"not wasm", "upper", "").is_err());
    }
}
