# Providence

This is a experimental / prototype implementation of a proposed
technique to allow long-running sessions in the Rust Playground.

# Concept

The "server" process spawns a "child" process. In a real
implementation, the child would be encased inside of a Docker
container and the server would be the playground web server. In this
repository, both are standalone binaries.

The server sends messages to the child over stdin/stdout as
binary-encoded Rust enums. The child acts upon these messages to
read/write files or to spawn futher "child" processes (e.g. `cargo`).

Standard input, output, and error to/from the child process are piped
across the boundary, allowing for interaction with the spawned child.

# Thoughts

- The names "server" and "client" and "child" aren't the best, but
  what's better?
