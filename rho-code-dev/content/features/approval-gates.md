+++
title = "Approval gates"
description = "Every write, edit, and command asks first. You can answer with instructions, not just yes or no."
+++

rho treats its own output as untrusted. Nothing happens without you saying so.

Writes, edits, and shell commands pass through an approval gate configured per
tool:

```toml
[approval.per_tool]
write_file = "ask"
edit_file = "ask"
run_command = "ask"
read_file = "auto"
```

Read-only tools run without interrupting you. Anything that changes state stops
and waits.

You don't have to answer with a plain refusal. Denying with a message — *"no,
use a trait instead"* — feeds that back to the model as a new instruction and it
tries again. That's the difference between a gate and a wall.
