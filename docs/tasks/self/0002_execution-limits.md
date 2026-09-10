# Execution Limits and Failure Handling

## Goal

Prevent external commands and task history from consuming unlimited time or memory.

This task covers:

1. execution timeouts
2. bounded stdout/stderr
3. bounded in-memory log/history growth
4. useful failure-result preservation

Do not implement PostgreSQL yet.

## 1. Process Timeouts

Every external execution stage must have a defined timeout.

This includes:

* AI implementer execution
* Cargo
* Maven
* Gradle
* Node/npm commands
* Python test commands
* other verification commands

Introduce centralized configuration rather than scattered hard-coded values.

Conceptually support:

```text
implementer timeout
verification command timeout
```

When a timeout occurs:

1. terminate the child process
2. prevent orphaned processes where practical
3. preserve useful stdout/stderr already produced
4. emit a clear failure/timeout event
5. mark the execution stage failed
6. continue collecting ChangeSet/results where safe

Use very short configurable timeouts in tests so tests remain fast.

## 2. Bounded Process Output

Do not allow stdout/stderr to grow without limit.

Introduce reasonable limits for:

```text
stdout bytes
stderr bytes
individual event/log payload
```

Do not accumulate unlimited output and truncate only afterward when it can reasonably be avoided.

When truncating, include an explicit marker such as:

```text
[output truncated: limit exceeded]
```

Normal small output must remain unchanged.

## 3. Bounded In-Memory Task History

Until PostgreSQL persistence exists, repetitive log/build events should not grow without limit.

Keep important lifecycle information such as:

* task creation
* proposal
* critique
* specification
* approval
* implementation state
* verification result
* failure
* completion
* final result

Bound repetitive command/build output using a simple strategy such as:

* maximum retained log events
* maximum retained log bytes

Do not build a complex persistence substitute.

PostgreSQL/event persistence will replace this temporary limitation.

## 4. Failure Handling

Review:

* implementer failure
* implementer timeout
* verification failure
* verification timeout
* diff generation failure
* cleanup failure

If repository modifications already exist, make a reasonable effort to preserve:

* failure details
* partial stdout/stderr
* verification information
* ChangeSet/diff

Do not erase useful debugging information before presenting it to the user.

Temporary workspaces must still eventually be cleaned up.

## Tests

Add tests covering:

* verification timeout
* process termination
* partial output before timeout
* stdout limit
* stderr limit
* truncation marker
* normal small output
* repetitive history/log bounding
* lifecycle events preserved
* ChangeSet retained after verification failure where possible

## Verification

Run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

## Out of Scope

Do not implement:

* PostgreSQL
* SSE event replay
* authentication
* GitHub App
* Cloud Run
* strong execution sandbox
* branches/worktrees

## Completion Report

Report:

1. timeout design
2. output limit design
3. event/history limit design
4. failure handling changes
5. files changed
6. tests added
7. fmt result
8. clippy result
9. test result
10. remaining limitations
