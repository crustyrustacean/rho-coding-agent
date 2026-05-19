//! REPL interaction modes for rho.
//!
//! Two modes:
//! - [`run_repl`] — interactive read-eval-print loop
//! - [`run_prompt_file`] — read a prompt from a file, run once, exit

use crate::app::App;
use anyhow::Result;
use std::{
    fs,
    io::{self, Write},
};

/// Run the interactive REPL loop.
///
/// Reads lines from stdin, dispatches slash commands, and drives the agent
/// loop for user messages. Handles `/quit`, `/clear`, and empty-input
/// graceful exit on EOF.
pub async fn run_repl(app: &mut App) -> Result<()> {
    loop {
        print!("User: ");
        io::stdout().flush()?;

        let input = tokio::task::spawn_blocking(|| {
            let mut line = String::new();
            let bytes_read = std::io::stdin().read_line(&mut line).unwrap_or(0);
            (line, bytes_read)
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {e}"))?;

        // EOF on stdin (bytes_read == 0) — exit gracefully.
        if input.1 == 0 {
            app.session.close("stdin EOF");
            break;
        }

        let input = input.0.trim();

        match input {
            "/quit" | "quit" => {
                app.session.close("user quit");
                break;
            }
            "/clear" => {
                // Branch back to the system message — same effect as
                // clearing the conversation, but the old tree is preserved
                // on disk so it can be inspected or resumed later.
                let path = app.session.path_to_root();
                if let Some(root_entry) = path.last() {
                    let root_id = root_entry.id.clone();
                    let _ = app.session.branch_to(&root_id);
                }
                println!("[conversation cleared]");
                continue;
            }
            "" => continue,
            _ => {}
        }

        match rho_core::run_loop(
            &mut app.session,
            input,
            &app.client,
            &app.registry,
            &app.config,
            app.cancel.clone(),
            &app.gate,
        )
        .await
        {
            Ok(reply) => println!("Assistant: {reply}"),
            Err(e) => eprintln!("Error: {e}"),
        }
    }

    Ok(())
}

/// Read a prompt file, run the agent loop once, print the reply, and exit.
///
/// # Errors
///
/// Returns an error if the file cannot be read or the agent loop encounters
/// a fatal error.
pub async fn run_prompt_file(mut app: App, path: std::path::PathBuf) -> Result<()> {
    let input = fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("cannot read prompt file `{}`: {e}", path.display()))?;
    eprintln!("using prompt file: {}", path.display());

    match rho_core::run_loop(
        &mut app.session,
        &input,
        &app.client,
        &app.registry,
        &app.config,
        app.cancel.clone(),
        &app.gate,
    )
    .await
    {
        Ok(reply) => println!("Assistant: {reply}"),
        Err(e) => eprintln!("Error: {e}"),
    }

    app.session.close("prompt file completed");
    Ok(())
}
