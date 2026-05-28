/**
 * rho Extension API — Type Definitions
 *
 * Reference this file from your extension for full IntelliSense:
 *
 *   /// <reference path="~/.rho/types/rho.d.ts" />
 *
 * Or, if installed alongside your extension:
 *
 *   /// <reference path="./rho.d.ts" />
 *
 * Extension files are TypeScript modules that export a default object
 * conforming to {@link ExtensionManifest}. Example:
 *
 *   export default {
 *     name: "my-extension",
 *     version: "1.0.0",
 *     tools: [{
 *       name: "greet",
 *       description: "Greet someone",
 *       risk: "read" as const,
 *       parameters: {
 *         name: { type: "string", description: "Who to greet", required: true },
 *       },
 *       execute: async (args) => {
 *         const { name } = JSON.parse(args);
 *         return `Hello, ${name}!`;
 *       },
 *     }],
 *   };
 */

// ── Risk Level ────────────────────────────────────────────────────────────────

/**
 * Risk level for an extension tool.
 *
 * - `"read"` — The tool only reads data. No side effects.
 * - `"write"` — The tool may create or modify files.
 * - `"destructive"` — The tool may execute commands or cause irreversible effects.
 *
 * Use `as const` to narrow the type (e.g. `risk: "read" as const`).
 */
type ToolRisk = "read" | "write" | "destructive";

// ── Parameter Definitions ────────────────────────────────────────────────────

/**
 * JSON Schema type for a tool parameter.
 */
type ParameterType = "string" | "number" | "boolean";

/**
 * Definition of a single tool parameter.
 *
 * Used as values in the {@link ToolDefinition.parameters} record.
 */
interface ParameterDef {
  /** JSON Schema type. */
  type: ParameterType;
  /** Human-readable description for the model. */
  description?: string;
  /** Whether this parameter must be provided. */
  required?: boolean;
}

// ── Tool Definition ───────────────────────────────────────────────────────────

/**
 * A tool exposed by the extension.
 *
 * Each tool has:
 * - **Metadata** — name, description, risk level, parameter schema
 * - **execute** — an async function that receives a JSON-encoded argument string
 *   and returns a string result
 *
 * The `execute` function receives arguments as a **raw JSON string**.
 * Parse it with `JSON.parse(args)` to access structured data. Return a plain
 * string — it will be passed back to the agent as the tool output.
 *
 * @example
 * ```typescript
 * {
 *   name: "search",
 *   description: "Search for items",
 *   risk: "read" as const,
 *   parameters: {
 *     query: { type: "string", description: "Search query", required: true },
 *     limit: { type: "number", description: "Max results", required: false },
 *   },
 *   execute: async (args: string) => {
 *     const { query, limit } = JSON.parse(args);
 *     return JSON.stringify({ results: [query] });
 *   },
 * }
 * ```
 */
interface ToolDefinition {
  /** Tool name — must be unique across all extensions. */
  name: string;
  /** Human-readable description shown to the model when selecting tools. */
  description: string;
  /** Risk level. Use `as const` to satisfy the literal type. */
  risk: ToolRisk;
  /**
   * Parameter definitions keyed by parameter name.
   *
   * These are translated into a JSON Schema `object` with `properties`
   * and `required` fields for the model.
   */
  parameters: Record<string, ParameterDef>;
  /**
   * The function the agent calls when it selects this tool.
   *
   * @param args - A JSON-encoded string of the tool arguments.
   *   Parse with `JSON.parse(args)` to access structured data.
   * @returns A string result. This is the tool output shown to the model.
   */
  execute: (args: string) => Promise<string>;
}

// ── Extension Hooks ──────────────────────────────────────────────────────────

/**
 * Payload sent to the `onToolCall` hook.
 */
interface ToolCallPayload {
  /** Name of the tool about to be called. */
  toolName: string;
  /** The arguments string passed to the tool. */
  arguments: string;
}

/**
 * Payload sent to the `onToolResult` hook.
 */
interface ToolResultPayload {
  /** Name of the tool that was called. */
  toolName: string;
  /** The tool's output string. */
  output: string;
  /** Whether the tool returned an error. */
  isError: boolean;
}

/**
 * Lifecycle hooks for an extension.
 *
 * All hooks are optional. Declare only the ones you need.
 */
interface ExtensionHooks {
  /**
   * Called once when the extension is loaded.
   *
   * Use for initialization, logging, or warm-up logic.
   */
  onLoad?: () => Promise<void>;

  /**
   * Called before a tool executes.
   *
   * Receives a JSON string — parse with `JSON.parse(args)` to access
   * {@link ToolCallPayload}.
   *
   * **Note:** In the current implementation this hook is notification-only.
   * It cannot block tool execution. Intercept capability is planned for a
   * future release.
   */
  onToolCall?: (args: string) => Promise<void>;

  /**
   * Called after a tool produces a result.
   *
   * Receives a JSON string — parse with `JSON.parse(args)` to access
   * {@link ToolResultPayload}.
   */
  onToolResult?: (args: string) => Promise<void>;

  /**
   * Called before sending messages to the model.
   *
   * Receives a JSON string representing the message array.
   *
   * **Note:** This hook is declared in the manifest but is not yet wired
   * into the agent loop. It will be activated in a future release.
   */
  onBeforeModel?: (args: string) => Promise<void>;
}

// ── Commands ──────────────────────────────────────────────────────────────────

/**
 * A slash command exposed by the extension.
 *
 * Commands are invoked by the user (e.g. `/deploy production`) or via RPC.
 */
interface CommandDefinition {
  /** Command name (without the leading `/`). */
  name: string;
  /** Optional description. */
  description?: string;
  /**
   * Handler function for the command.
   *
   * @param args - The text after the command name (may be empty).
   * @returns A string result.
   */
  handler: (args: string) => Promise<string>;
}

// ── Extension Manifest ────────────────────────────────────────────────────────

/**
 * The manifest describing an extension's capabilities.
 *
 * This is the `export default` value of your extension's main TypeScript file.
 *
 * @example
 * ```typescript
 * export default {
 *   name: "my-extension",
 *   version: "1.0.0",
 *   tools: [{
 *     name: "ping",
 *     description: "Returns pong",
 *     risk: "read" as const,
 *     parameters: {},
 *     execute: async () => "pong",
 *   }],
 *   hooks: {
 *     onLoad: async () => {
 *       rho.log("info", "Extension loaded!");
 *     },
 *   },
 *   commands: [{
 *     name: "status",
 *     description: "Show extension status",
 *     handler: async () => "healthy",
 *   }],
 * } satisfies ExtensionManifest;
 * ```
 */
interface ExtensionManifest {
  /** Extension name. Must be unique. */
  name: string;
  /** Optional semantic version string. */
  version?: string;
  /** Tools declared by this extension. */
  tools?: ToolDefinition[];
  /** Lifecycle hooks. */
  hooks?: ExtensionHooks;
  /** Slash commands declared by this extension. */
  commands?: CommandDefinition[];
}

// ── rho Global ────────────────────────────────────────────────────────────────

/**
 * The `rho` global object available in all extensions.
 *
 * Host functions provided by the rho runtime. Extensions can ONLY perform I/O
 * through these functions — the V8 sandbox has no filesystem, network, or
 * process access on its own.
 *
 * Currently implemented:
 * - `log` — Structured logging
 * - `getCwd` — Extension's working directory
 *
 * Planned (not yet available):
 * - `readFile`, `writeFile`, `runCommand`, `fetchUrl`
 * - `getModel`, path utilities, truncation
 */
interface RhoGlobal {
  /**
   * Emit a structured log line.
   *
   * Logs are emitted via rho's `tracing` infrastructure and appear in rho's
   * log output. They are **not** visible to the model.
   *
   * @param level - Log level: `"trace"`, `"debug"`, `"info"`, `"warn"`, or `"error"`.
   *   Invalid levels are silently promoted to `"info"`.
   * @param message - The message to log.
   *
   * @example
   * ```typescript
   * rho.log("info", "Processing request");
   * rho.log("error", `Failed to parse: ${err}`);
   * ```
   */
  log(level: "trace" | "debug" | "info" | "warn" | "error", message: string): void;

  /**
   * Get the extension's working directory.
   *
   * When loaded from disk (`spawn_from_file`), this returns the extension's
   * root directory. When loaded inline (`spawn`), it falls back to the
   * current working directory of the rho process.
   *
   * @returns An absolute path string.
   *
   * @example
   * ```typescript
   * const cwd = rho.getCwd(); // "/home/user/.rho/extensions/my-tool"
   * ```
   */
  getCwd(): string;

  // ── Planned host functions (not yet implemented) ──────────────────────────

  /**
   * Read a file's contents.
   * @param path - File path (relative to extension root or absolute).
   * @returns The file contents as a string.
   */
  // readFile(path: string): Promise<string>;

  /**
   * Write content to a file.
   * @param path - File path.
   * @param content - Content to write.
   */
  // writeFile(path: string, content: string): Promise<void>;

  /**
   * Run a shell command.
   * @param cmd - The command to execute.
   * @param args - Command arguments.
   * @returns An object with stdout, stderr, and exit code.
   */
  // runCommand(cmd: string, args?: string[]): Promise<{ stdout: string; stderr: string; exitCode: number }>;

  /**
   * Fetch a URL and return its content.
   * @param url - The URL to fetch.
   * @param headers - Optional HTTP headers.
   */
  // fetchUrl(url: string, headers?: Record<string, string>): Promise<{ status: number; body: string }>;

  /**
   * Get the name of the currently active model.
   */
  // getModel(): string;

  /**
   * Truncate text to a maximum size.
   * @param text - The text to truncate.
   * @param maxBytes - Maximum bytes.
   * @param maxLines - Maximum lines.
   */
  // truncate(text: string, maxBytes?: number, maxLines?: number): string;
}

/**
 * The global `rho` object, injected into every extension's V8 isolate.
 *
 * Access it directly as `rho.log(...)`, `rho.getCwd()`, etc.
 */
declare const rho: RhoGlobal;
