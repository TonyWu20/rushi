# Coding Conventions

Standing code rules for this repo. Read before writing Rust here.

## Builders for wide functions (the `bon` rule)

A function with 8 or more parameters gets a `bon` builder. Do not
add `#[allow(clippy::too_many_arguments)]`. The lint stays at the
default warn; the builder removes the warning at the source.

### The pattern

Put `#[builder]` on the function, with `use bon::builder;` at the
top of the module. The entry point keeps the function name. It takes
no arguments and returns the builder. Setters are called in any
order, in one or several statements. `.call()` finishes the
builder and runs the function.

    use bon::builder;

    #[builder]
    fn decide_form(
        n_events: usize,
        budget_tokens: usize,
        measurements: &[(usize, usize)],
        state: Option<CompactState>,
        base_caps: Caps,
        keep_events: usize,
        max_drops: usize,
        boundary_seq: usize,
    ) -> (RequestForm, Option<CompactState>) { ... }

The caller:

    let builder = decide_form()
        .n_events(n)
        .budget_tokens(budget_tokens)
        .measurements(&measurements)
        .base_caps(base_caps)
        .keep_events(compact_keep_events)
        .max_drops(max_drops)
        .boundary_seq(boundary_seq);
    let (form, persist) = if let Some(s) = state {
        builder.state(s).call()
    } else {
        builder.call()
    };

The builder is typestate. It compiles only when every required
setter is present. Calling a setter twice is a compile error.

### `Option` parameters

A parameter of type `Option<T>` is an optional setter. It takes
the inner `T`. Omission means `None`.

To forward a runtime `Option` value, bind the builder first. Then
branch on the value.

    let builder = fn().a(x).b(y);
    let out = if let Some(v) = opt {
        builder.param(v).call()
    } else {
        builder.call()
    };

A `Some(v)` literal at the call site is written as
`.param(v)`, not `.param(Some(v))`.

### Lifetimes

`#[builder]` generates a builder struct that holds every parameter.
An elided lifetime of a generic type in a reference cannot be
elided there. Name it. `RenderState<'a>` in `&RenderState`
becomes `&'a RenderState<'a>` on `event_lines` in `bin/tui`.

### The dependency

`bon` is declared in `[workspace.dependencies]` in the root
`Cargo.toml`. A crate that uses the builder adds
`bon = { workspace = true }` to its own `[dependencies]`.

## Records

2026-09-03: the nine functions that carried
`#[allow(clippy::too_many_arguments)]` switched to the builder.
`decide_form` and `summary_input_request` in `bin/assemble`,
`run_compaction` in `bin/compact`, `event_lines`, `body_rows`,
`write_body`, `edit_body`, and `bash_body` in `bin/tui`, and
`monitor_thread` in `bin/tui` `ext.rs`.
