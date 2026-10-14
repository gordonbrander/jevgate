# jevgate — Spec

`jevgate` is a small Rust CLI that puts a judgment step into shell pipelines. It reads text on stdin and asks a Jev-style classifier a yes/no question about it. If the answer clears a threshold, it runs a command.

```
some-producer | jevgate "Is this an urgent outage report?" -- notify.sh
```

## 1. Goal

Cron can say *when* something runs, but not *whether it's worth acting on*. jevgate fills that gap:

1. Cron wakes up a pipeline.
2. A producer (`curl`, `gh`, `rss2txt`, `tail`, …) emits some text.
3. jevgate asks a classifier a yes/no question about that text.
4. If the answer passes, jevgate runs your script with the same text on stdin.

Example crontab line:

```cron
*/15 * * * * curl -s https://status.example.com/feed | jevgate --threshold 0.8 "Does this report a new outage affecting the API?" -- ~/bin/page-me.sh
```

Jev-style classifiers are fast (~100–250 ms) and cheap (fractions of a cent per call). They return calibrated probabilities instead of free text, so a classifier call on every cron tick is reasonable.

### Non-goals

- **Scheduling.** cron, launchd, or systemd timers handle that.
- **Fetching input.** Input comes from stdin only, and the producer handles fetching.
- **Dedup or memory between runs.** If the same item shows up every tick, the producer filters it (e.g. `comm`, a seen-file, `--since`).
- **Choice or score gates, and multiple questions per call.** v1 asks one yes/no question.
- **Config files.** Flags and env vars cover it.

## 2. Usage

```
jevgate [OPTIONS] <QUESTION> [-- <COMMAND> [ARGS...]]
```

| Option | Default | Meaning |
|---|---|---|
| `<QUESTION>` | required | Yes/no question about the input, sent as the question's `instructions`. |
| `--threshold <P>` | `0.5` | Pass if P(yes) ≥ P. Must be in `[0, 1]`. |
| `--yes <TEXT>` | — | Criteria for "yes" (`criteria.true`). Must be used with `--no`. |
| `--no <TEXT>` | — | Criteria for "no" (`criteria.false`). Must be used with `--yes`. |
| `--provider <NAME>` | `systemone` | `systemone`, `cloudflare`, or `openai` (see §4). |
| `--model <ID>` | per provider | Model id sent to the provider. |
| `--url <URL>` | per provider | Full endpoint URL. Overrides the provider's default URL. |
| `--timeout <SECS>` | `30` | Per-request HTTP timeout. |
| `-v, --verbose` | off | Log the score, model, and decision to stderr. |
| `-q, --quiet` | off | Without a command, don't print the input on pass. |

If there's no command after `--`, jevgate acts as a filter, like `grep`: on pass it prints the input to stdout.

```sh
some-producer | jevgate "Is this urgent?" | some-consumer
```

With `-q` it acts as a predicate, like `grep -q`:

```sh
echo "$msg" | jevgate -q "Is this spam?" && mv "$f" spam/
```

## 3. Behavior contract

**Input.** jevgate reads all of stdin as a UTF-8 string and sends it as `state`. Empty or whitespace-only stdin, and non-UTF-8 stdin, are errors (exit 2). jevgate doesn't cap input size. If the input is too large, the provider rejects it and jevgate reports an error.

**Decision.** jevgate sends one question named `gate` of type `noul`. The `noul` value in the reply is P(yes). The gate passes when `noul >= threshold`.

**On pass, without a command:** jevgate writes the original input to stdout unchanged, unless `-q` is given. A consumer that closes stdout early is fine, because write errors are ignored. On reject, it writes nothing, so a downstream consumer sees empty input.

**On pass, with a command:**
- jevgate spawns the command with its args, with no shell involved.
- It writes the original input to the child's stdin, then closes it. A child that doesn't read stdin is fine, because broken pipe is ignored.
- The child's environment is jevgate's environment plus:
  - `JEVGATE_SCORE`: P(yes), e.g. `0.93`
  - `JEVGATE_MODEL`: the versioned model id from the response, e.g. `jev-1.13.0`
- The child inherits stdout and stderr.

**Exit codes:**

| Code | Meaning |
|---|---|
| `0` | Passed, no command given (input printed unless `-q`). |
| the command's code | Passed and the command ran. If a signal killed it, the code is `128 + signal`. |
| `1` | Rejected (score below threshold). |
| `2` | Error: bad usage, empty input, network or API error, unparseable response, or command failed to spawn. |

**Fails closed.** On any error the command does **not** run.

**Quiet by default.** On reject, jevgate prints nothing, so cron doesn't send mail. Errors go to stderr, so cron *does* send mail for them. `-v` adds one line to stderr for every decision:

```
jevgate: pass score=0.93 threshold=0.80 model=jev-1.13.0
```

## 4. Providers

All Jev-like services do the same thing: they take a state and a yes/no question and return P(yes). jevgate hides each provider's differences behind one function:

```rust
struct Judge<'a> {
    state: &'a str,
    question: &'a str,
    criteria: Option<(&'a str, &'a str)>, // (yes, no)
}

enum Provider { SystemOne, Cloudflare, OpenAI }

impl Provider {
    /// Returns (P(yes), model id).
    fn judge(&self, j: &Judge, cfg: &Config) -> Result<(f64, String), Error>;
}
```

`judge` is a single `match` that builds the URL, auth header, and body, then extracts the probability. There are three closed variants and no need for plugins. A trait with `dyn` dispatch would add indirection without buying anything.

### 4.1 `systemone` (default)

This is the native SystemOne protocol from TypeSafe's Jev. Other services speak it unchanged: OpenRouter decision models (e.g. Microsoft Decision-1) and local SystemOne servers. For those, you point `--url` and `--model` at them.

- URL: `https://api.typesafe.ai/v1/systemone`
- Model: `jev-latest`
- Auth: `Authorization: Bearer $JEV_API_KEY`. With `--url`, the key is optional, and if it's unset no `Authorization` header is sent, since local servers (e.g. [reflex](https://github.com/kshetrajna12/reflex)) often run without auth. Without `--url`, a missing key is an error.

Request:

```json
{
  "model": "jev-latest",
  "state": "Help! My payouts have been failing for 3 days.",
  "questions": {
    "gate": {
      "type": "noul",
      "instructions": "Does this convey urgency?",
      "criteria": { "true": "Explicitly time-sensitive", "false": "No urgency expressed" }
    }
  }
}
```

`criteria` is left out unless both `--yes` and `--no` are given.

Response:

```json
{
  "model": "jev-1.13.0",
  "answers": { "gate": { "type": "noul", "noul": 0.95 } },
  "usage": { "input_tokens": 426, "output_tokens": 73 }
}
```

jevgate extracts `answers.gate.noul` (a number in [0, 1]) and `model`. If either is missing or `noul` is out of range, that's an error.

### 4.2 `cloudflare`

Cloudflare Workers AI hosts Jev (`typesafe/jev`) and its open-weights clone Clef. The payload is the SystemOne payload wrapped in `input`, and the response shape is the same.

- URL: `https://api.cloudflare.com/client/v4/accounts/$CLOUDFLARE_ACCOUNT_ID/ai/run`
- Model: `typesafe/jev`. Use `--model` to select Clef.
- Auth: `Authorization: Bearer $CLOUDFLARE_API_TOKEN`

```json
{ "model": "typesafe/jev", "input": { "state": "…", "questions": { "gate": { … } } } }
```

If the response is wrapped in Cloudflare's usual `{"result": …}` envelope, jevgate unwraps it before parsing.

### 4.3 `openai` (provisional)

OpenAI Decisions (`gpt-6-luna`) has its own native endpoint, with a "predicate" question type for yes/no. As of this writing I couldn't confirm the public request and response schema.

- v1 keeps the enum variant.
- `--provider openai` exits 2 with "not yet implemented" until the schema is checked against OpenAI's docs.
- Auth will be `Authorization: Bearer $OPENAI_API_KEY`.

### 4.4 Credentials

Credentials come from **environment variables only**, never flags, so they stay out of `ps` output and crontab files:

| Provider | Env vars |
|---|---|
| systemone | `JEV_API_KEY` |
| cloudflare | `CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ACCOUNT_ID` |
| openai | `OPENAI_API_KEY` |

A missing variable is an error (exit 2) that names the variable, except `JEV_API_KEY` with `--url` (§4.1). In cron, set the variables at the top of the crontab or in a wrapper script.

## 5. Implementation

**Layout:** one binary crate, two files.

```
Cargo.toml
src/main.rs      # CLI parsing, read stdin, call judge, threshold, spawn command, exit codes
src/provider.rs  # Provider enum, request bodies, response parsing, judge(), retries
tests/e2e.rs     # runs the binary against a std TcpListener mock server
```

**Dependencies:**
- `clap` (derive): argument parsing, including `--`, `requires`, and value ranges.
- `serde_json`: request and response bodies, built with `json!` and read as `Value`, so no derive structs.
- `ureq` (rustls): blocking HTTP client.

Each run makes one request, so jevgate doesn't need an async runtime and doesn't use tokio. It uses no other dependencies. Spawning the command uses `std::process::Command`.

**Retries:** up to 3 attempts, with backoff of 1 s then 2 s.
- Retried: network errors, timeouts, HTTP 429, 529, and other 5xx.
- Not retried: other 4xx errors (401 bad key, 422 bad body). jevgate prints the status and response body to stderr and exits 2.

**Determinism:** jevgate sends no temperature or other sampling options and leaves model defaults alone.

## 6. Testing

- **Provider unit tests:** serialize a `Judge` for each provider and compare with the JSON fixtures in §4. Parse the sample responses and check that a missing `gate`, a non-numeric `noul`, or an out-of-range `noul` is an error.
- **Decision tests:** boundary checks on the threshold (`noul == threshold` passes) and on the mapping from outcome to exit code.
- **End-to-end:** `tests/e2e.rs` starts a one-shot mock HTTP server on a `std::net::TcpListener` that returns canned responses. It runs the built binary with `--url http://127.0.0.1:<port>` and checks:
  - Pass runs the command, and the command sees the input on stdin and `JEVGATE_SCORE` in its env.
  - Reject exits 1 and doesn't run the command.
  - A 500 followed by a 200 retries and then passes.
  - A 401 exits 2 and doesn't run the command.
  - Empty stdin exits 2.
- **Manual smoke test:** a real call against TypeSafe, Cloudflare, or a local reflex server with a known-yes and a known-no input.

## 7. Open questions / future

- Verify the OpenAI Decisions schema and implement §4.3.
- Choice gates (e.g. run a different command for each category) and score gates.
- `--state-json`, which would send stdin as a JSON object, since SystemOne `state` accepts objects.
- If dedup between runs comes up again and again in practice, consider a `--once <file>` flag that skips inputs whose hash was seen before.

## 8. Sources

- Cloudflare: Jev (typesafe) model docs — https://developers.cloudflare.com/ai/models/typesafe/jev/
- Flavio Copes, "How to build typed decisions with Jev" — https://flaviocopes.com/jev/
- "Which Jev-like AI Decision/Classifier Model Is Best?" — https://akitaonrails.com/en/2026/10/10/which-jev-like-ai-decision-classifier-model-is-best/
- classifier-bench PR #10 (OpenAI Decisions + Cloudflare Clef adapters) — https://github.com/CMaintz/classifier-bench/pull/10
- The New Stack, "OpenAI answers TypeSafe's Jev with a Decision API built on Luna" — https://thenewstack.io/openai-decision-api-luna/
- Existing Rust clients (TypeSafe only): https://docs.rs/typesafe-rs, https://docs.rs/kunobi-jev
