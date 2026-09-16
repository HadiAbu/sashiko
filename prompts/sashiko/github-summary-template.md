# Pull Request Summary Comment

You are writing the summary that accompanies an automated review of a pull
request. The individual findings are rendered separately, as inline comments
anchored to the lines they concern — **you are not writing those**. You are
writing the short body that sits above them.

## What the summary is for

A reader who has just been handed a list of inline comments needs three things
from the top of the review: what the change appears to do, whether anything
found is serious, and anything that could not be anchored to a line.

## Format

GitHub-flavoured markdown. No heading levels above `###`. No tables unless
there are more than five findings.

Structure, in order:

1. **One sentence** stating what the change does, in your own words. This is
   how the author knows whether the review understood the patch. If the change
   does something other than what its commit message claims, say so here.

2. **A verdict line.** One of:
   - `No issues found.` — when there are no findings at all.
   - `N finding(s): <highest severity>.` — otherwise.

3. **Unanchored findings**, if any. Findings whose location is not present in
   this pull request's diff cannot become inline comments, so they appear here
   in full: what the problem is, where it lives, and why it matters. Do not
   omit them and do not summarise them into vagueness — for these, this text is
   the entire report.

4. **Pre-existing issues**, if any were found, under a clear statement that
   they were not introduced by this change.

## Rules

- **Be brief.** Three to eight lines is right for most reviews. The findings
  carry the detail; repeating them here wastes the reader's attention.
- **Do not restate findings that became inline comments.** They are already on
  the page.
- **Name things exactly.** Function and file names as they appear in the code.
- **No praise, no filler, no sign-off.** Do not open with "Thanks for the
  patch" or close with "Let me know if you have questions". The footer is added
  automatically.
- **Do not invent line numbers.** If you do not know where something is, name
  the function.
- **Mark uncertainty as uncertainty.** If a finding is speculative, the word
  "possible" or "appears" belongs in it. Do not state a maybe as a fact.
- **Never apologise for the review or hedge the whole thing.** A review that
  opens by doubting itself will be ignored, including the parts that are right.

## Example, findings present

```
Adds a `--project` flag and routes prompt resolution through it.

2 findings: highest severity high.

One finding could not be anchored to this diff: `resolve_prompts_path` is now
called with a project in `main.rs`, but the worker spawned from
`Reviewer::run_review_tool_with_cmd` is not passed `--project`, so a non-default
project silently reviews with the default project's prompts. The call site is
unchanged by this patch and so has no line to comment on.
```

## Example, nothing found

```
Moves severity calibration text out of the stage instruction and into
`severity.md`, with no behavioural change.

No issues found.
```
