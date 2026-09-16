# Inline Review Report Template

Produce a plain-text review report based on the findings provided.

## Text Formatting Rules (Strictly Enforced)

- **Plain text only.** No markdown, no backticks (`), no markdown headings
  (`#`), no bold/italics (`**` or `*`), and no fenced code blocks (` ``` `).
  Never quote function names, variable names, types, or file paths in backticks.
- **Wrap lines at 78 characters.** Every prose line, summary sentence, and
  problem description must be hard-wrapped at 78 characters or fewer so the
  report fits cleanly inside an 80-column terminal window.
- **Indented code snippets only.** When illustrating a code path or call chain,
  indent the snippet by 4 spaces on its own lines without backticks.
- **Clear, concise paragraphs.** Never write long or dense paragraphs. Spread
  information out into short, logical paragraphs separated by a blank line so it
  is easy to read.
- **NEVER EVER USE ALL CAPS.** Do not use ALL CAPS for labels, emphasis, or
  headings. The only time uppercase words are acceptable is when referencing an
  actual Rust constant or identifier defined in uppercase in the code.
- **NEVER QUOTE LINE NUMBERS.** Line numbers shift across commits and worktrees
  and are meaningless to the reader. Reference code locations strictly by file
  path and function, method, or struct name, or by call chain
  (for example:forge_webhook() -> create_fetching_patchset()).
- **Factual and undramatic.** Keep wording technical, direct, and concise. Do
  not add praise, filler, greetings, or sign-offs ("Thanks for the patch",
  "Let me know if you have questions"). Never apologize or hedge the review.
- **Include every finding.** You MUST include every finding passed to this stage
  in the output list. Do not omit findings or assume they are rendered
  elsewhere.
- **Always end the report with a blank line.**

## Structure

1. **Summary sentence(s)** (1-2 sentences, wrapped at 78 characters): State what
   the change does in your own words.

2. **Verdict line** (separated by a blank line):
   - If there are no findings:
     No issues found.
   - If there are findings:
     N finding(s): highest severity <Level>.

3. **Plain list of findings** (when findings are present):
   For each finding, output the following block separated by blank lines:

   [Severity: <Level>]
   File: <file_path> (<function_or_symbol>)

   <Short, concise problem description wrapped at 78 characters. Explain what
   is wrong, what condition triggers it, and why it matters. If helpful, include
   a brief 4-space indented code snippet or call chain.>

   Where <Level> is Critical, High, Medium, or Low. If a finding is flagged as
   pre-existing, append (pre-existing) to the File line.

## Example (findings present)

Adds a summary mechanism to the background sync worker that categorizes and
aggregates git fetch outcomes into a single status log per sync cycle.

2 findings: highest severity Critical.

[Severity: Critical]
File: src/worker/sync.rs (GitSyncWorker::run_cycle)

Holding the synchronous std::sync::MutexGuard across the async fetch_remote()
call can deadlock Tokio worker threads when multiple remotes sync concurrently.

    let guard = self.state.lock().unwrap();
    self.fetch_remote(remote).await?;

Drop the mutex guard before awaiting fetch_remote() or use tokio::sync::Mutex
if the lock must be held across await points.

[Severity: Medium]
File: src/api.rs (forge_webhook)

The placeholder cover letter message ID omits the @sashiko.local domain suffix
expected by resolve_root_msg_id(), causing git fetch ingestion to create a
second patchset row and leave the initial row stuck in Fetching status.

## Example (no issues found)

Moves severity calibration rules into severity.md with no behavioral changes
to the review pipeline.

No issues found.
