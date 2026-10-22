<!--
Copyright (c) Meta Platforms, Inc. and affiliates.

This source code is licensed under the MIT license found in the
LICENSE file in the root directory of this source tree.
-->

# Identifier pattern bindings

This implements [issue #54](https://github.com/facebook/bunnylol.rs/issues/54).
Patterns are opt-in: omit `pattern_bindings` to keep existing behavior.

## Configuration

Use an array of tables so declaration order is preserved:

```toml
[[pattern_bindings]]
name = "tickets"
pattern = '(?P<ticket>ENG-[0-9]+)'
url = "https://tracker.example.com/browse/{ticket}"
description = "Engineering ticket"

[[pattern_bindings]]
name = "pull-requests"
pattern = '#(?P<number>[0-9]+)'
url = "https://github.com/example/repo/pull/{number}"
```

Each rule requires a unique, nonempty `name`, a `pattern`, and a `url`.
`description` is optional. Unknown fields are errors. Names identify rules in
listings; they are not command prefixes. The first rule that resolves wins.

Patterns use Rust's `regex` syntax, including `(?P<name>...)` named captures.
The resolver wraps them in absolute start/end anchors. A substring, trailing
newline, or extra words will not match unless the pattern explicitly permits
those characters. Inputs are not trimmed or case-folded. Regex compilation is
limited to 1 MiB per rule; unsupported syntax such as lookaround and backreferences
is rejected by the regex engine.

The resolution order is:

1. Existing special prefixes such as `$AAPL` and `r/rust`.
2. User bindings with `override = true`.
3. Built-in commands.
4. Other user bindings.
5. Pattern bindings in declaration order.
6. The configured search engine.

Command aliases retain their existing one-dispatch behavior: rewritten inputs
can reach built-ins, URL patterns or search, and do not re-enter another command
alias. URL patterns cannot dispatch commands or recurse into aliases.

## URL templates

Write `{capture_name}` to insert a named regex capture. A capture can be repeated;
an unmatched optional capture inserts an empty value. Numbered and anonymous
placeholders are rejected, as are unknown captures and unbalanced braces.

Captures may appear in a path or in a query **value** after `=`. For example:

```toml
[[pattern_bindings]]
name = "reference-search"
pattern = 'ref:(?P<reference>.+)'
url = "https://example.com/items/{reference}?q={reference}&view=full"
```

Values are percent-encoded as UTF-8. ASCII letters, digits, `-`, `_`, and `~`
remain readable; slashes, percent signs, spaces, query delimiters, fragments,
controls and other bytes are encoded. A capture of `a/b?x=1&y=2` becomes
`a%2Fb%3Fx%3D1%26y%3D2`. Input containing `%2F` is treated as literal text and
becomes `%252F`, rather than being decoded before substitution.

The URL must have a literal `http://` or `https://` scheme and fixed host and
port. Credentials, whitespace, controls and backslashes are rejected. Captures
cannot appear in the scheme, authority, query keys, or fragment. Literal fragments
are allowed. A final origin check ensures substitutions cannot change the
destination. Dot-only path segments (`.` or `..`, even percent-encoded) are
rejected because browsers normalize them; if a capture would produce one, that
rule does not resolve and the next rule or search is tried. Dot segments in a
configured template are configuration errors.

## Validation, reload, and listings

Regexes and templates are compiled while deserializing the configuration, not
per request. Invalid rules prevent CLI/server startup. The server checks file
modification times as before; an invalid reload keeps the complete last valid
configuration, including its patterns and bindings page. Correcting the file
allows a later request to reload it. Removing the pattern tables disables them.

Run `bunnylol bindings`, `bunnylol --list`, or open the server's landing page to
see pattern names, regexes, descriptions, destinations, and declaration order.
The CLI and HTTP routes use the same resolver with one config snapshot per
command/request.

## Try locally without installing a service

The repository includes [an example config](../examples/identifier-patterns.toml).
From the repository root, with Rust installed:

```sh
cargo build --locked
mkdir -p target/pattern-preview/config/bunnylol
cp examples/identifier-patterns.toml target/pattern-preview/config/bunnylol/config.toml
export XDG_CONFIG_HOME="$PWD/target/pattern-preview/config"
export XDG_DATA_HOME="$PWD/target/pattern-preview/data"
export XDG_CACHE_HOME="$PWD/target/pattern-preview/cache"
./target/debug/bunnylol --dry-run ENG-1234
./target/debug/bunnylol --dry-run '#9164'
./target/debug/bunnylol --dry-run 'look up ENG-1234'
./target/debug/bunnylol bindings
./target/debug/bunnylol serve --address 127.0.0.1 --port 18766
```

Choose an unused port. Then, from another terminal:

```sh
curl -sS -D - -o /dev/null --get --data-urlencode 'cmd=#9164' http://127.0.0.1:18766/
```

The `Location` header should be `https://github.com/example/repo/pull/9164`.
The example disables history and binds only to loopback. Replace its example
hosts and repository with your destinations before regular use. Stop the
foreground server with Ctrl-C. No browser or service configuration is needed
for this trial.

On Unix, an existing `/etc/bunnylol/config.toml` takes precedence over the XDG
config, as in upstream Bunnylol; check for that before using an isolated config.

## Verification

```sh
cargo fmt --all -- --check
cargo check --all-features --locked
cargo test --all-features --locked
cargo clippy --all-features --locked -- -D warnings
```

The tests cover full-input matching and multiline anchors, declaration order,
command/prefix/user-binding precedence, capture encoding and URL boundaries,
invalid configuration, configuration serialization, real CLI/HTTP parity, and
hot-reload rejection, recovery, and removal of rules. Service installation and
browser defaults are outside this feature's scope.
