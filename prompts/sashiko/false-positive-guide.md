# False Positive Prevention Guide

This guide is used where avoiding false positives matters most. Shift bias away
from fast processing and follow it carefully.

## Core principle

**If you cannot prove an issue exists with concrete evidence from this
codebase, do not report it.**

Evidence means code you have read. Not "Rust services usually", not "this looks
like it could", not "a caller might". Name the function, name the path, name
the input that reaches it.

The corollary matters as much: proving a path is *structurally possible* is
enough. You do not have to prove it executes on every run. A lock held across
an `.await` is a bug even if the contention window is small. An outbox row with
no terminal state is a bug even if it has not stuck yet.

## Patterns that produce false positives here

### 1. The linter already ran

`make lint` runs `cargo clippy` and `cargo fmt` before any human sees the
change. Do not report formatting, naming, import order, `needless_borrow`,
missing `#[derive]`, redundant clones, or anything else clippy emits.

- Bad: "Consider using `iter()` instead of `into_iter()` here."
- Bad: "This `clone()` looks unnecessary."
- Good: "This `clone()` copies the full patch body on every stage, and
  `max_input_tokens` is already the binding constraint" — a consequence, not a
  style preference.

### 2. The compiler already ran

Type errors, missing match arms on a closed enum, borrow errors, unused
variables: none of these can reach review. If your finding would have been a
compile error, you have misread the code. Re-read it.

### 3. "Add a check for safety"

Do not ask for defensive validation unless you can show all three:
- the value comes from somewhere untrusted (a patch, a webhook, a PR, a model
  response, a config file), **and**
- a concrete path carries it to the code in question, **and**
- the current code demonstrably misbehaves on a value that path can produce.

- Bad: "This should validate the index before use."
- Good: "`selected_prompts` comes from model output, is joined onto the prompt
  root, and reaches `include_file` — a name containing `..` would escape the
  prompt directory."

### 4. `unwrap` and `expect` are not automatically bugs

The project's rule is that they need a proof they cannot panic, and many in
this tree have one. Before reporting, check whether the invariant holds:

- Bad: "`.unwrap()` here can panic."
- Good: "`.unwrap()` here assumes the patchset has at least one patch, but
  `create_patchset` is reachable with an empty `patches` array from
  `/api/submit`."

Also check *where* it is. A panic in the worker subprocess is recovered by the
reviewer and retried; a panic in the daemon's main loop is not. The same
`unwrap` has different severity in different modules.

### 5. Assuming a caller does not handle it

This is the most common way a real analysis turns into a false positive in
reverse. Do not dismiss a defect inside the changed code by assuming the
surrounding system handles it, unless you can point at the specific code that
makes the failure structurally impossible. "The API layer probably validates
this" is not evidence. Go read the API layer.

### 6. Missing error handling that cannot happen

Before reporting an unhandled error, confirm the error is reachable. Many
`Result`-returning helpers in this tree are infallible in practice for a
specific call site. Say why you believe the error can occur.

### 7. Prompt and instruction text

Prompt wording is a legitimate review target — an ambiguous field name or a
missing escape hatch in an enum is a real defect, and `llm-stages.md` explains
why. But do not report stylistic preferences about prompt prose, and do not
report that a prompt "could be clearer" without naming the specific
misinterpretation it permits and what the model would do instead.

### 8. Pre-existing issues

If the problem existed before this change, it is still worth reporting, but
mark `preexisting: true`. Do not present unchanged code as something the patch
broke. Check the surrounding context, not just the `+` lines.

### 9. Test code

Tests are held to a different standard than production code. A `unwrap` in a
test is fine. A fixed port, a shared database file, or a dependency on global
state in a test is *not* fine, because it makes the suite flaky for everyone —
report those.

## Before you report

For each finding, confirm you can answer all of these. If you cannot answer
one, the finding is speculative: report it, cap it at Medium, and say which
question you could not answer.

1. Which function, in which file, contains the defect?
2. What concrete input or sequence triggers it?
3. What is the observable consequence?
4. What code did you read that rules out the obvious reason this would be safe?
