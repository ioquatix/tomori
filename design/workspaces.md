# Workspaces

This describes the target design for the Rust server that replaces `bridge/`. The current Node bridge differs; changes to it are not reviewed against this document.

## Requirement

The server runs agent sessions only in directories its operator has allowed, and only a bounded number at once.

- The operator names one or more workspace roots with the repeatable `tomori-server --workspace-root PATH` option. Without one, the only root is the server's current working directory when it starts. The server prints and logs its effective roots at startup, so that starting it somewhere broad, such as the home directory, is visible. Each root is canonicalized when the server starts, with symbolic links resolved; a root that does not exist or is not a directory stops the server with an error naming it.
- Every directory a client supplies is canonicalized, with symbolic links resolved, before any backend sees it. It must be an existing directory that is a root or lies beneath one. Containment is decided component by component on the canonical paths, never by string prefix: `/home/sam2` is not beneath `/home/sam`, and neither `..` nor a symbolic link inside a root can reach a directory outside it. A relative path is refused. This applies to creating a session, to binding an existing session, and to any backend session listing for a directory that a client can request. The backend receives the canonical directory, and the space shows it.
- Checking a directory never stalls the server. A path is refused without touching the file system unless, with `.` and `..` applied as written, it is a root or lies beneath one, either as the root was configured or in its canonical form. This narrows which paths are canonicalized; the canonical check still decides. Canonicalizing runs off the server's executor, for at most 5 seconds, after which the directory is refused; at most 8 checks run at once, and a request arriving while 8 are running is refused.
- Every refused directory receives the same short error reply, `That directory is not available.`, whether the path is missing, is not a directory, is outside every root or cannot be read, so the reply does not reveal whether a path exists or why it was refused. The reason is logged on the server.
- At most a limited number of spaces have a session starting or running at once: 8 by default, configurable with `tomori-server --max-spaces COUNT`. A space counts from when its directory is accepted, before its session has started, until its session has closed or failed to start, so concurrent requests cannot together pass the limit. A space whose session has ended stays listed as closed (see `spaces.md`) but does not count. A request beyond the limit is refused with an error that says the limit is reached, and starts nothing. A request refused for its directory holds no place, even while its directory is being checked.

## Rationale

There is no authentication yet (see `server.md`), and the local network is not fully trusted: anyone who can reach the server can ask it to start agent processes, which can read and change files in their directory. Confining sessions to chosen roots keeps an agent away from directories such as `/` or `~/.ssh`. By default the operator scopes agents by where they start the server, as with `claude` itself, which runs in the directory it is started in; so the default no longer exposes `~/.ssh` or `~/.claude` unless the server is started in the home directory. The current working directory replaced the home directory as the default in the October 2026 server review (finding 3); a uniform refusal stops the server from serving as a probe of the filesystem; and the cap stops one client from exhausting the machine with agent processes. Agreed in the October 2026 server review (finding 8).

## Scope

The server's handling of space creation, session binding and any client-triggered session listing; the `tomori-server` options for roots and the space limit. Which roots or directories the server tells clients about is not yet decided.

## Validation

- Test that a directory outside every root, a sibling sharing a root's prefix (`/root2` against `/root`), a `..` path leaving a root and a symbolic link inside a root to a directory outside it are each refused, for both creating and binding a session, and that no session starts.
- Test that a missing path, a file and a relative path receive exactly the same reply as a forbidden directory.
- Test that an allowed directory named with `.`, `..` or a trailing slash is accepted and the space shows its canonical form.
- Test that a missing root is refused at startup, that the options are parsed and validated, and that the default root is the current working directory.
- Test the limit with creations held open: of more concurrent requests than the limit, exactly the limit start and the rest are refused; running spaces count; a closed space frees its place; a refused directory holds none, including requests sent together with an allowed one.
- Test that paths outside the roots as written, such as `/net/host/share`, are refused before any file system call, that a root matches both as configured and in canonical form, and that the number of running checks is limited.
- Review the server for backend calls that take a client-supplied directory without resolving it against the roots.
