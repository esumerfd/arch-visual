# seam-client

`seam-client` is a small Claude Code `PostToolUse` hook: every time Claude Code edits or writes a
Rust file in a project you are working on, it reads the change from stdin, works out whether that
change means something structural — a top-level item appearing or disappearing, or a cross-module
reference appearing or disappearing — and advertises it to a running Seam Explorer over a local Unix
socket. It is the thing that makes the graph on your screen move while you code.

**What it deliberately does not do**, all of which is by design rather than unfinished:

- **It never reads a file.** The hook payload already carries the before and after text, so there is
  no filesystem access of the edited path at all.
- **It never touches the network.** One local Unix datagram socket, and nothing else.
- **It knows nothing about your graph.** It advertises what changed; it does not resolve what that
  change means. Working out which community a new node belongs to happens in the visualizer.
- **It never prints anything**, to stdout or stderr, on any path whatsoever — success, failure,
  malformed input, or no visualizer running.
- **It always exits 0.** There is exactly one exit status and it is success. No socket error,
  timeout, malformed payload or unexpected input can make one of your tool calls fail, hang, or put
  noise in your session.

If Seam Explorer is not running, the hook gives up quietly and fast. You should not be able to tell
it is installed.

## Building and installing

Build the release binary from the repository root:

```sh
make build-client
```

That prints the absolute path of the binary it built. It lands at:

```
<repo>/target/release/seam-client
```

**Use that absolute path in your hook configuration.** Do not rely on `seam-client` being found on
your `PATH`. A hook runs in an environment you did not set up — it does not necessarily inherit the
shell configuration, `PATH` entries, or version-manager shims your terminal has. An absolute path
also removes any chance of a different, same-named binary earlier on the path being run instead of
this one.

Copy or symlink the binary somewhere stable if you like; just keep the configuration pointing at
wherever it actually is.

## Registering the hook

Add this to the `.claude/settings.json` of **the project you want to visualize**, substituting the
absolute path printed by `make build-client`:

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Edit|Write",
        "hooks": [
          {
            "type": "command",
            "command": "/absolute/path/to/target/release/seam-client",
            "timeout": 5
          }
        ]
      }
    ]
  }
}
```

The `matcher` covers both tools that can change a file's contents. The `timeout` is a backstop only
— the client's own internal budget is far shorter (it gives up on the socket after 100 ms), so this
value should never actually be reached.

### This goes in the other project, not in this repository

Register the hook in the configuration of the codebase you want to look at. Do **not** register it
in this repository's own `.claude/settings.json`. `seam-client` exists to observe *other* projects;
pointing it at this one would make it fire on this repository's own development, which is not what
it is for and only adds noise. This is a deliberate rule, not a caution.

## Troubleshooting

### Start here: the one-time hooks-trust prompt

**"I registered it and nothing happens" is most often this, and not a broken binary.**

Claude Code gates hook *execution* behind a trust flag (`hasTrustDialogHooksAccepted`) that is
**separate from ordinary directory trust** (`hasTrustDialogAccepted`). The consequence catches
people out: a project you have worked in for months, and have long since trusted, will still not run
a newly added hook until you accept a second, one-time prompt that appears the first time a
`.claude/settings.json` containing a `hooks` block is introduced to that project.

So: after adding the block above, expect a trust prompt, and accept it. If you dismissed it, or
never saw one, that is the first thing to check — long before you start suspecting this code. The
symptom of an unaccepted prompt is indistinguishable from a broken hook: nothing happens, and
nothing is reported anywhere.

### The visualizer has to be running

Events go to a socket that Seam Explorer creates when it starts. With the app closed there is
nothing listening, and the client is designed to shrug and exit silently in exactly that case. Start
`seam-explorer-egui` (with a graph loaded) and edit again.

### Only Rust files are examined

Anything that does not end in `.rs` is ignored outright.

### Only some changes are structural

The detector is a deliberately scoped, parser-free heuristic. It sees top-level items — functions,
structs, enums, traits and type aliases — appearing and disappearing, plus cross-module references
written with a path (`use` statements and path-qualified calls). It does **not** see, among other
things, methods inside `impl` blocks, nested items, or calls made through a receiver or a local
binding.

The authoritative, complete list of what it deliberately does not see is the blind-spot inventory in
the module doc comment at the top of [`src/detect.rs`](src/detect.rs). Every entry there has a named
passing test. That inventory is the reference rather than this paragraph, which would otherwise
drift out of date the moment coverage changes.

## A note on the payload shape, and how it will eventually break

The stdin payload shape this client reads was **captured from a real, installed Claude Code CLI at
version `2.1.261` on 2026-09-05** — not taken from documentation. That distinction is load-bearing:
the documentation snapshot this project started from was materially wrong about two field names, and
building against it would have produced a hook that silently never fired. This surface has already
been observed changing faster than the documentation tracks it.

So it is worth knowing what a future change would look like, because it produces the worst possible
symptom: **the hook fires, nothing arrives, and there is no error anywhere** — no message in your
session, nothing on stderr, a clean exit every time. The client cannot tell the difference between
"this payload has no structural meaning" and "this payload has a shape I no longer recognize," and
it is required to stay silent in both cases.

If the hook stops producing events after a Claude Code upgrade, and the trust prompt above is not
the cause, the payload shape is the next thing to check — capture a real payload again and compare
it against the fields in [`src/hook_input.rs`](src/hook_input.rs).
