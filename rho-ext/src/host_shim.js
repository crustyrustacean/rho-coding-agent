// rho_host extension shim - creates the `rho` global.
//
// This module is loaded as the ESM entry point of the `rho_host` deno_core
// extension. It builds the `rho` namespace object on `globalThis.rho`.
// Standard Web API shims (console, fetch, URL, etc.) are loaded separately
// as a plain JS script via the extension's `js` parameter.
//
// The actual work is done by Rust ops registered in `host.rs`. This file
// only provides the JavaScript-facing wrapper.

const ops = Deno.core.ops;

/**
 * Helper: check if the result is an error and throw if so.
 * @param {string} result - The result from a Rust op.
 * @returns {string} The result if not an error.
 */
function unwrapResult(result) {
  if (typeof result === "string" && result.startsWith("__ERROR__")) {
    throw new Error(result.slice("__ERROR__".length));
  }
  return result;
}

globalThis.rho = {
  /**
   * Emit a structured log line.
   * @param {"trace"|"debug"|"info"|"warn"|"error"} level
   * @param {string} message
   */
  log(level, message) {
    ops.op_rho_log(level, message);
  },

  /**
   * Get the extension's working directory.
   * @returns {string}
   */
  getCwd() {
    return ops.op_rho_get_cwd();
  },

  /**
   * Get the name of the currently active model.
   *
   * Returns the model identifier (e.g. `"claude-sonnet-4-20250514"`,
   * `"gpt-4o"`). The value updates live if the user switches models
   * during a session. Returns an empty string if no model has been set.
   *
   * @returns {string} The model name.
   */
  getModel() {
    return ops.op_rho_get_model();
  },

  /**
   * Read a file's contents within the extension sandbox.
   *
   * The path is resolved relative to the extension's root directory.
   * Files must be within the sandbox (the extension root or any
   * explicitly allowed paths). Maximum file size is 1 MiB.
   *
   * @param {string} path - File path (relative to extension root or absolute).
   * @returns {string} The file contents as a string.
   * @throws {Error} If the file is outside the sandbox, doesn't exist, or is too large.
   */
  readFile(path) {
    return unwrapResult(ops.op_rho_read_file(path));
  },

  /**
   * Write content to a file within the extension sandbox.
   *
   * The path is resolved relative to the extension's root directory.
   * Parent directories are created automatically. The parent directory
   * must be within the sandbox.
   *
   * @param {string} path - File path.
   * @param {string} content - Content to write.
   * @throws {Error} If the path is outside the sandbox or the write fails.
   */
  writeFile(path, content) {
    const result = ops.op_rho_write_file(path, content);
    unwrapResult(result);
  },

  /**
   * Run a shell command and return its output.
   *
   * Requires the `commands = true` permission in the extension config.
   * If the extension does not have this permission, an error is thrown.
   *
   * @param {string} cmd - The command to execute (e.g. "git", "npm").
   * @param {string[]} [args=[]] - Command arguments.
   * @returns {{ stdout: string, stderr: string, exitCode: number }}
   *   The command's stdout, stderr, and exit code.
   * @throws {Error} If the extension lacks command permission or execution fails.
   */
  runCommand(cmd, args = []) {
    const argsJson = Array.isArray(args) ? JSON.stringify(args) : "";
    const result = unwrapResult(ops.op_rho_run_command(cmd, argsJson));
    return JSON.parse(result);
  },

  /**
   * Fetch a URL and return the response.
   *
   * Requires the `network = true` permission in the extension config.
   *
   * @param {{ url: string, method?: string, headers?: Record<string,string>, body?: string, max_bytes?: number }} opts
   * @returns {{ status: number, headers: Record<string,string>, body: string }}
   * @throws {Error} If the extension lacks network permission or the request fails.
   */
  fetchUrl(opts) {
    const optsJson = typeof opts === "string" ? opts : JSON.stringify(opts);
    const result = unwrapResult(ops.op_rho_fetch_url(optsJson));
    return JSON.parse(result);
  },
};
