// rho_host extension shim - creates the `rho` global.
//
// This module is loaded as the ESM entry point of the `rho_host` deno_core
// extension. It builds the `rho` namespace object and assigns it to
// `globalThis.rho` so that extension code can call `rho.log(...)`,
// `rho.getCwd()`, etc.
//
// The actual work is done by Rust ops registered in `host.rs`. This file
// only provides the JavaScript-facing wrapper.

const ops = Deno.core.ops;

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
};
