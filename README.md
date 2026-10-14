# jevgate

Gate a shell command on a yes/no judgment about its input.

jevgate reads text on stdin and asks a [Jev](https://flaviocopes.com/jev/)-style classifier a yes/no question about it. If P(yes) clears a threshold, the text passes through, like a line through `grep`. If not, nothing comes out.

```sh
some-producer | jevgate "Is this an urgent outage report?" | some-consumer
```

To run a command only when the gate passes, put it after `--`:

```sh
some-producer | jevgate "Is this an urgent outage report?" -- notify.sh
```

## Install

You need a Rust toolchain (edition 2024, Rust 1.85+).

```sh
cargo install --path .
```

That puts `jevgate` in `~/.cargo/bin`. To build without installing, run `cargo build --release`. The binary is then at `target/release/jevgate`.

## Quick start

1. Get an API key from TypeSafe and export it:

   ```sh
   export JEV_API_KEY=...
   ```

2. Ask a question:

   ```sh
   echo "Production API is returning 500s for all users since 09:12" \
     | jevgate -v "Does this report an outage?"
   ```

   On stderr you'll see `jevgate: pass score=0.98 threshold=0.50 model=jev-1.13.0`, and the input is printed to stdout.

3. Try an input that should be rejected:

   ```sh
   echo "Today's lunch menu: tacos" | jevgate -v "Does this report an outage?"
   echo $?   # 1, and nothing printed
   ```

## Usage

```
jevgate [OPTIONS] <QUESTION> [-- <COMMAND> [ARGS...]]
```

| Option              | Default      | Meaning                                                               |
| ------------------- | ------------ | --------------------------------------------------------------------- |
| `<QUESTION>`        | required     | Yes/no question about the input.                                      |
| `--threshold <P>`   | `0.5`        | Pass if P(yes) ≥ P. Must be in `[0, 1]`.                              |
| `--yes <TEXT>`      | —            | What counts as "yes". Requires `--no`.                                |
| `--no <TEXT>`       | —            | What counts as "no". Requires `--yes`.                                |
| `--provider <NAME>` | `systemone`  | `systemone`, `cloudflare`, or `openai`.                               |
| `--model <ID>`      | per provider | Model id sent to the provider.                                        |
| `--url <URL>`       | per provider | Full endpoint URL. Overrides the provider's default.                  |
| `--timeout <SECS>`  | `30`         | Per-request HTTP timeout.                                             |
| `-v, --verbose`     | off          | Print score, threshold and model to stderr for every decision.        |
| `-q, --quiet`       | off          | Don't print the input on pass. Only the exit code reports the result. |

### As a filter

On success, jevgate prints the input to stdout and exits 0. On reject it prints nothing and exits 1.

```sh
gh issue view 123 | jevgate "Is this a bug report?" | tee -a bugs.log
```

### As a command gate

When you have a script that should _not_ run on reject (paging, email) you can use the `--` option. Everything after `--` is a command that will be run on pass. jevgate runs it directly, without a shell. On pass, the command:

- gets the original input on stdin (a command that ignores stdin is fine),
- inherits jevgate's stdout, stderr and environment, plus two extra variables:
  - `JEVGATE_SCORE`: P(yes), e.g. `0.93`
  - `JEVGATE_MODEL`: the versioned model id the provider reported, e.g. `jev-1.13.0`

If you need pipes or redirects, wrap the command in `sh -c`:

```sh
jevgate "Is this a bug report?" -- sh -c 'cat >> bugs.log'
```

### As a predicate

With `-q`, jevgate prints nothing and only exits 0 on pass or 1 on reject, like `grep -q`:

```sh
if jevgate -q "Is this spam?" < "$f"; then mv "$f" spam/; fi
```

For if/else, branch on the exit code, so errors (2) don't count as "no":

```sh
jevgate -q "Is this urgent?" < "$f"
case $? in
  0) page-me.sh < "$f" ;;
  1) file-it.sh < "$f" ;;
  *) echo "jevgate failed" >&2 ;;
esac
```

### Sharpening the question

`--yes` and `--no` tell the classifier where the line is. Use them together:

```sh
jevgate "Does this convey urgency?" \
  --yes "Explicitly time-sensitive" \
  --no "No urgency expressed" \
  --threshold 0.8 -- page-me.sh
```

Raise `--threshold` to cut false positives and lower it to cut false negatives. Run with `-v` for a while to see what scores your inputs actually get.

## Exit codes

| Code               | Meaning                                                                                                                                      |
| ------------------ | -------------------------------------------------------------------------------------------------------------------------------------------- |
| `0`                | Passed, and no command was given (input printed unless `-q`).                                                                                |
| the command's code | Passed and the command ran. If a signal killed it, the code is `128 + signal`.                                                               |
| `1`                | Rejected: the score was below the threshold.                                                                                                 |
| `2`                | Error: bad usage, empty or non-UTF-8 input, missing credentials, network or API error, unparseable response, or the command failed to start. |

jevgate **fails closed**: on any error, the command does not run.

## Using it from cron

jevgate is quiet on reject, so cron doesn't email you. Errors go to stderr, so cron _does_ email you about them.

```cron
JEV_API_KEY=...
*/15 * * * * curl -s https://status.example.com/feed | jevgate --threshold 0.8 "Does this report a new outage affecting the API?" -- $HOME/bin/page-me.sh
```

jevgate has no memory between runs. If the same item shows up on every tick, filter it in the producer (e.g. with `--since`, `comm`, or a seen-file).

## Providers

All providers take the input and a yes/no question and return P(yes). Credentials come from **environment variables only**, so they never appear in `ps` output or crontab command lines.

| Provider     | Default URL                                                                   | Default model  | Env vars                                        |
| ------------ | ----------------------------------------------------------------------------- | -------------- | ----------------------------------------------- |
| `systemone`  | `https://api.typesafe.ai/v1/systemone`                                        | `jev-latest`   | `JEV_API_KEY`                                   |
| `cloudflare` | `https://api.cloudflare.com/client/v4/accounts/$CLOUDFLARE_ACCOUNT_ID/ai/run` | `typesafe/jev` | `CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ACCOUNT_ID` |
| `openai`     | —                                                                             | —              | `OPENAI_API_KEY` (not yet implemented)          |

If a variable is missing, jevgate exits 2 and names the variable. The one exception: with `systemone` and `--url`, `JEV_API_KEY` is optional (see below).

### `systemone` (default)

This is the native SystemOne protocol from TypeSafe's Jev. Other services speak it too, for example OpenRouter decision models and local SystemOne servers. Use `--url` and `--model` to point at them:

```sh
jevgate --url http://localhost:8080/v1/systemone --model my-local-model "Is this spam?"
```

With `--url`, `JEV_API_KEY` is optional. If it's set, jevgate sends it as a bearer token. If it isn't, jevgate sends no `Authorization` header, which suits a local server with no auth. Without `--url`, the key is required.

### Running locally with reflex

[reflex](https://github.com/kshetrajna12/reflex) is an open re-creation of Jev that runs on your own GPU and serves the SystemOne protocol, so jevgate works with it unchanged. On Apple Silicon:

```sh
git clone https://github.com/kshetrajna12/reflex && cd reflex
uv sync --no-sources    # plain PyPI torch, which includes the Mac GPU backend
uv run --no-sync reflex-serve --stable --device mps --dtype float16 --port 8008
```

Use `main`: the `stable` tag predates `--device mps`, but `--stable` still applies the recommended settings. The first run downloads Qwen3.5-4B (about 8 GB). On Linux with an NVIDIA GPU, follow reflex's README and drop `--device`/`--dtype`. Then:

```sh
echo "Production API is returning 500s for all users" \
  | jevgate -v --url http://localhost:8008/v1/systemone "Does this report an outage?"
# jevgate: pass score=0.93 threshold=0.50 model=Qwen/Qwen3.5-4B
```

`--model` doesn't matter here. reflex serves whatever model it loaded and reports that id in `JEVGATE_MODEL`. Things to know:

- **Auth.** If reflex is reachable from other machines, start it with `--api-key` (or `REFLEX_API_KEY`) and give jevgate the same value in `JEV_API_KEY`.
- **Thresholds.** reflex's scores aren't Jev's scores, and its stable configuration ships uncalibrated. Run with `-v` on your own inputs before choosing `--threshold`. `--yes`/`--no` help a lot on borderline inputs.
- **Long inputs.** On a Mac, reflex reads roughly 600 tokens/s the first time it sees an input (an M3 Max took about 31 s for a 35 KB log), then caches it. Raise `--timeout` for large inputs, or the first attempt times out and a retry runs.

### `cloudflare`

This provider uses Cloudflare Workers AI, which hosts Jev (`typesafe/jev`) and its open-weights clone Clef. To use Clef, pass its model id with `--model`.

```sh
export CLOUDFLARE_API_TOKEN=... CLOUDFLARE_ACCOUNT_ID=...
jevgate --provider cloudflare "Is this spam?" < message.txt
```

### `openai`

This is a placeholder for OpenAI Decisions. For now it exits 2 with "not yet implemented".

## Retries and timeouts

jevgate makes up to 3 attempts, waiting 1 s and then 2 s between them. It retries network errors, timeouts, HTTP 429 and 5xx. It does not retry other 4xx errors (e.g. 401 bad key, 422 bad request). For those it prints the status and response body and exits 2. `--timeout` applies to each request.

## What it doesn't do

- **Scheduling:** use cron, launchd, or systemd timers.
- **Fetching input:** input comes from stdin only.
- **Deduplication between runs:** filter in the producer.
- **Multiple questions, or choice and score gates:** v1 asks one yes/no question.
- **Config files:** flags and env vars cover it.

## Development

```sh
cargo test
```

- Unit tests in `src/provider.rs` cover request bodies and response parsing.
- `tests/e2e.rs` runs the built binary against a mock HTTP server on a local `TcpListener`. It covers pass and reject, filter and `-q` output, the inclusive threshold, retries, 401, when the bearer token is sent, empty and non-UTF-8 input, signal exit codes, and spawn failures. It doesn't need network access or API keys.

The code is in two files:

- `src/main.rs`: CLI, stdin, threshold decision, spawning the command, exit codes.
- `src/provider.rs`: the provider enum, request bodies, response parsing, retries.
