# ProcessManager – Inline Examples

The crate ships with small, self-contained example programs that live directly
under `examples/` (single `.rs` files).  
They are built and run with the standard Cargo *example* command:

```bash
# From the project root
cargo run --example <name>
```

| Example (`cargo run --example …`) | Highlights                                                             |
| -------------------------------- | ---------------------------------------------------------------------- |
| `simple`                         | minimal setup: one manager, two workers, graceful shutdown             |
| `dynamic_add`                    | add new `Runnable`s **while the manager is already running**            |

Feel free to copy / adapt the code for your own services and let us know if you
run into problems.