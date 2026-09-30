+++
title = "Branchable sessions"
description = "Fork the conversation, try another approach, switch back. Branches survive a restart."
+++

Your work isn't a chat log. It's a tree.

Every entry carries a *resolution* — how much of it reaches the model. Old
context gets summarised or outlined as a session grows, and pinned entries are
protected from being compacted away.

**Fork** creates a second cursor over the same session. Both branches share one
log, so forking is instant and costs no disk. Switch between them freely; each
keeps its own context window. Everything survives a restart, because branch
state is persisted rather than held in memory.

The point isn't the mechanism, it's the workflow. When rho takes an approach you
don't like, you fork from *before* it and ask for a different one — rather than
spending a dozen messages steering it back through a history that now includes
the failed attempt.
