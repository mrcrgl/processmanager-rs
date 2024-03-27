# ProcessManager

Manage multiple running services. A ProcessManager collects impl of `Runnable`
and takes over the runtime management like starting, stopping (graceful or in 
failure) of services.

If one service fails, the manager initiates  a graceful shutdown for all other.

# Examples

```rust



```
