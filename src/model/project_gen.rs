use std::path::Path;

use crate::host::HostRegistry;

const TSCONFIG: &str = r#"{
  "compilerOptions": {
    "target": "ES2020",
    "module": "ES2020",
    "moduleResolution": "node",
    "strict": true,
    "noEmit": true,
    "lib": ["ES2020"],
    "skipLibCheck": true,
    "types": []
  },
  "include": ["**/*.ts", "**/*.js", "types/**/*.d.ts"]
}
"#;

const CONSOLE_DTS: &str = r#"/**
 * Runtime-provided `console` global object.
 * All output is captured by the host and displayed in the Console panel.
 */
declare const console: {
  /** Output a log message. */
  log(...args: any[]): void;
  /** Output an informational message. */
  info(...args: any[]): void;
  /** Output a warning message. */
  warn(...args: any[]): void;
  /** Output an error message. */
  error(...args: any[]): void;
  /** Output a debug-level message. */
  debug(...args: any[]): void;
};
"#;

const GLOBALS_DTS: &str = r#"/**
 * Runtime-provided timer globals.
 *
 * QuickJS ships no timers, so the host installs browser-style ones. Timers are
 * cancelled by id, and a callback that throws is reported in the Console panel.
 */

/** Run `callback` once after `delayMs` milliseconds; returns the timer id. */
declare function setTimeout(callback: () => void, delayMs?: number): number;
/** Cancel a timer created by `setTimeout`. */
declare function clearTimeout(id?: number): void;
/** Run `callback` every `periodMs` milliseconds until cleared; returns the timer id. */
declare function setInterval(callback: () => void, periodMs?: number): number;
/** Cancel a repeating timer created by `setInterval`. */
declare function clearInterval(id?: number): void;
"#;

const HELLO_JS: &str = r#"// Welcome to Bolt!
// Press Run (Cmd/Ctrl+Enter) to execute this script.
console.log('hello world');
console.info('info message');
console.warn('warn message');
console.error('error message');
"#;

const DEMO_TS: &str = r#"// Example: import a Rust-provided host module.
import { env, readFile } from 'bolt';

const name: string = env('USER') ?? 'world';
console.log(`hello, ${name}`);

const tsconfig = readFile('./tsconfig.json');
console.log(`tsconfig.json size: ${tsconfig.length} bytes`);
"#;

pub fn generate_project_files(dir: &Path, registry: &HostRegistry) -> Result<(), String> {
    let types = dir.join("types");
    std::fs::create_dir_all(&types).map_err(|e| format!("create types dir: {e}"))?;
    write_if_changed(&dir.join("tsconfig.json"), TSCONFIG)?;
    write_if_changed(&types.join("console.d.ts"), CONSOLE_DTS)?;
    write_if_changed(&types.join("globals.d.ts"), GLOBALS_DTS)?;
    registry
        .write_type_declarations(&types)
        .map_err(|e| format!("write host module declarations: {e}"))?;
    let hello = dir.join("hello.js");
    if !hello.exists() {
        std::fs::write(&hello, HELLO_JS).map_err(|e| e.to_string())?;
    }
    let demo = dir.join("demo.ts");
    if !demo.exists() {
        std::fs::write(&demo, DEMO_TS).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn write_if_changed(path: &Path, content: &str) -> Result<(), String> {
    if let Ok(existing) = std::fs::read_to_string(path)
        && existing == content
    {
        return Ok(());
    }
    std::fs::write(path, content).map_err(|e| format!("write {}: {e}", path.display()))
}
